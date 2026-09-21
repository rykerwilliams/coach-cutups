//! The export's sound (spec E3): one decode pipeline per file, mixed per
//! output frame into the blocks the muxer's AAC branch takes.
//!
//! ```text
//! per file:  filesrc ! decodebin3 (the audio stream only)
//!            ! audioconvert ! audioresample ! appsink F32LE/48k/2ch
//! per frame: the regions covering it, read at their own offsets, scaled by
//!            `core::audio::envelope` and summed
//! ```
//!
//! **Core owns the splice, media owns the samples.** Which span of which file
//! is heard at each emitted sample, how loud, and the fades at its edges are
//! all [`Region`]s handed in on the job; nothing here decides any of it.
//!
//! **The mixed stream is the output timeline with its first
//! [`PRIMING_SAMPLES`] samples dropped.** `avenc_aac` prepends that many
//! samples of priming and re-derives its output times by counting samples from
//! the first buffer, so shifting the timestamps is absorbed and the sound
//! still lands 21.3 ms late; dropping the samples puts it where the picture is
//! (measured). Core already expresses the regions on that emitted timeline, so
//! there is nothing to compensate for here beyond starting at emitted sample
//! 0.
//!
//! **A file with no audio track is silence,** and so is one that can't be
//! read. The muxer's audio pad must be fed for the whole run either way — an
//! export of footage filmed without sound is still an export — and the bus
//! refuses a missing source or recording before the run starts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use video_coach_core::audio::{
    envelope, Region, Track, AUDIO_SAMPLE_RATE, PRIMING_SAMPLES, SAMPLES_PER_FRAME,
};

use super::export::ExportJob;
use super::{CompositeError, Stopper, Watch, POLL, QUEUED};
use crate::player::seconds_to_clock;

/// The mix is stereo: every sample position is a pair of floats.
const CHANNELS: usize = 2;

/// How long [`Reader::start`] waits for `decodebin3`'s stream collection. A
/// file that posts neither a collection nor an `ERROR` — a directory, a named
/// pipe with nothing behind it — would otherwise wait forever.
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// The format the readers decode to, the mixer produces and the encode side
/// takes — one description so the three can't disagree.
pub(super) fn caps_description() -> String {
    description(AUDIO_SAMPLE_RATE, CHANNELS)
}

/// A reader's own caps. The export's are [`caps_description`]'s; transcription
/// reads the same files at whisper's 16 kHz mono (spec S2).
fn caps(rate: u32, channels: usize) -> gst::Caps {
    description(rate, channels)
        .parse()
        .expect("a constant caps description parses")
}

fn description(rate: u32, channels: usize) -> String {
    format!("audio/x-raw,format=F32LE,rate={rate},channels={channels},layout=interleaved")
}

/// The audio edit, played out one output frame at a time.
///
/// The regions of one track never overlap, and every region boundary is a
/// whole output frame — core quantizes entries and segments to frames — so a
/// block never retires one region and starts another on the **same** file.
/// That is what lets a region seek its reader as it comes in.
pub(super) struct Mixer {
    regions: Vec<Region>,
    /// The file each region reads, in the same order.
    paths: Vec<PathBuf>,
    /// Region indices by first emitted sample: the order they arrive in.
    order: Vec<usize>,
    /// How much of `order` has arrived.
    next: usize,
    /// The regions covering the block being mixed.
    active: Vec<usize>,
    /// One pipeline per file, opened on first use. `None` is a file with no
    /// sound to give.
    readers: HashMap<PathBuf, Option<Reader>>,
}

impl Mixer {
    /// The mixer for `job`: its regions, resolved to the files they read.
    pub(super) fn new(job: &ExportJob) -> Mixer {
        let entries = &job.compilation.plan.entries;
        let paths: Vec<PathBuf> = job
            .audio
            .iter()
            .map(|region| match region.track {
                // A source index with no file is caught by the pump, which
                // needs the same video; here it is one more silent file.
                Track::Game => job
                    .sources
                    .get(entries[region.entry].source_index)
                    .cloned()
                    .unwrap_or_default(),
                Track::Commentary => job.entries[region.entry].recording.clone(),
            })
            .collect();
        let mut order: Vec<usize> = (0..job.audio.len()).collect();
        order.sort_by_key(|&i| job.audio[i].out_samples.start);
        Mixer {
            regions: job.audio.clone(),
            paths,
            order,
            next: 0,
            active: Vec::new(),
            readers: HashMap::new(),
        }
    }

    /// The mixed block covering output frame `n`, stamped for the AAC branch.
    ///
    /// Always a full block, silent where no region covers it: the muxer's
    /// audio track has to run the length of the video.
    pub(super) fn block(&mut self, n: u64, cancel: &AtomicBool) -> gst::Buffer {
        let (start, end) = emitted_span(n);
        let regions = &self.regions;
        let mut retired = Vec::new();
        self.active.retain(|&r| {
            let live = regions[r].out_samples.end > start;
            if !live {
                retired.push(r);
            }
            live
        });
        // Close a file nothing later reads — in practice every recording, as
        // its entry ends. Kept open, a compilation of two hundred clips would
        // hold two hundred pipelines at once, where the picture holds one
        // recording at a time. The source video's reader stays, because the
        // next entry normally reads it again.
        for r in retired {
            let path = &self.paths[r];
            let wanted = self
                .active
                .iter()
                .chain(&self.order[self.next..])
                .any(|&i| self.paths[i] == *path);
            if !wanted {
                self.readers.remove(path);
            }
        }
        while self.next < self.order.len()
            && self.regions[self.order[self.next]].out_samples.start < end
        {
            let r = self.order[self.next];
            self.next += 1;
            // The seek is what a region's `source_offset` means; from here the
            // reader is read forward, block by block.
            if let Some(reader) = reader(&mut self.readers, &self.paths[r], cancel) {
                reader.seek(self.regions[r].source_offset);
            }
            self.active.push(r);
        }

        let mut block = vec![0.0f32; (end - start) as usize * CHANNELS];
        for &r in &self.active {
            let region = &self.regions[r];
            let lo = region.out_samples.start.max(start);
            let hi = region.out_samples.end.min(end);
            let Some(reader) = reader(&mut self.readers, &self.paths[r], cancel) else {
                continue;
            };
            let samples = reader.read((hi - lo) as usize, cancel);
            for (i, sample) in (lo..hi).enumerate() {
                let gain = envelope(region, sample) as f32;
                let at = (sample - start) as usize * CHANNELS;
                for c in 0..CHANNELS {
                    block[at + c] += samples[i * CHANNELS + c] * gain;
                }
            }
        }
        stamp(&block, start, end)
    }
}

/// The emitted samples output frame `n` covers.
///
/// Output frame `n` is output samples `[n·1600, (n+1)·1600)`, and the emitted
/// stream is the output one with its first [`PRIMING_SAMPLES`] dropped, so
/// frame 0 contributes only what survives the drop.
fn emitted_span(n: u64) -> (u64, u64) {
    let emitted = |frame: u64| (frame * SAMPLES_PER_FRAME).saturating_sub(PRIMING_SAMPLES);
    (emitted(n), emitted(n + 1))
}

/// `block` as a buffer at its place on the emitted timeline.
fn stamp(block: &[f32], start: u64, end: u64) -> gst::Buffer {
    let bytes: Vec<u8> = block.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut buffer = gst::Buffer::from_mut_slice(bytes);
    {
        let buffer = buffer.get_mut().expect("a new buffer is writable");
        buffer.set_pts(sample_time(start));
        buffer.set_dts(gst::ClockTime::NONE);
        buffer.set_duration(sample_time(end) - sample_time(start));
    }
    buffer
}

/// Emitted sample `s`'s time on the encoder's timeline.
fn sample_time(s: u64) -> gst::ClockTime {
    gst::ClockTime::SECOND
        .mul_div_floor(s, u64::from(AUDIO_SAMPLE_RATE))
        .expect("no overflow")
}

/// The reader for `path`, opened on first use. `None` is a file with no sound;
/// it contributes silence and the run goes on (spec E3).
fn reader<'a>(
    readers: &'a mut HashMap<PathBuf, Option<Reader>>,
    path: &Path,
    cancel: &AtomicBool,
) -> Option<&'a mut Reader> {
    readers
        .entry(path.to_owned())
        .or_insert_with(|| Reader::open(path, cancel))
        .as_mut()
}

/// One file's sound: an audio-only pipeline with a cursor, read forward from
/// wherever the last seek put it, at the rate and channel count
/// [`Reader::start`] was asked for.
///
/// [`Reader::read`] counts its frames in [`CHANNELS`], so only the export's
/// stereo readers may use it; [`Reader::rest`] is channel-agnostic.
pub(crate) struct Reader {
    pipeline: Stopper,
    appsink: gst_app::AppSink,
    path: PathBuf,
    /// This file's own errors, kept off the export's [`Watch`]: a track that
    /// stops decoding costs its sound, not the run — the same trade the
    /// picture-in-picture makes.
    errors: Arc<Mutex<Option<String>>>,
    /// The last pulled buffer, and how much of it is spent.
    held: Vec<f32>,
    spent: usize,
    /// Nothing more will come: the end of the file, an error, or a cancel.
    done: bool,
    /// Why, when it was not the end of the file. [`Reader::read`] pads over it
    /// and the export goes quiet; [`Reader::rest`] must not, since a clip cut
    /// short by a cancel is indistinguishable from a whole one once it is a
    /// buffer of samples.
    stopped: Option<CompositeError>,
    /// Where [`Reader::read`] hands its samples back.
    out: Vec<f32>,
}

impl Reader {
    /// `path`'s sound, or `None` when there is none to be had: a file with no
    /// audio track (silently — footage filmed without sound is normal), or one
    /// that can't be read, with a line on stderr.
    fn open(path: &Path, cancel: &AtomicBool) -> Option<Reader> {
        match Reader::start(path, AUDIO_SAMPLE_RATE, CHANNELS, cancel) {
            Ok(reader) => reader,
            Err(CompositeError::Cancelled) => None,
            Err(CompositeError::Failed(e)) => {
                eprintln!("export: no sound from {}: {e}", path.display());
                None
            }
        }
    }

    /// Builds the pipeline, prerolls it and sets it PLAYING, decoding to
    /// `rate` and `channels`. `Ok(None)` is a file with no audio track.
    pub(crate) fn start(
        path: &Path,
        rate: u32,
        channels: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Reader>, CompositeError> {
        let pipeline = gst::Pipeline::new();
        let make = |factory: &str| {
            gst::ElementFactory::make(factory)
                .build()
                .map_err(|e| CompositeError::Failed(format!("{factory} is missing: {e}")))
        };
        let filesrc = make("filesrc")?;
        filesrc.set_property("location", path);
        let decodebin = make("decodebin3")?;
        let convert = make("audioconvert")?;
        let resample = make("audioresample")?;
        let appsink = gst_app::AppSink::builder()
            .caps(&caps(rate, channels))
            .sync(false)
            .max_buffers(QUEUED as u32)
            .enable_last_sample(false)
            .build();
        pipeline
            .add_many([
                &filesrc,
                &decodebin,
                &convert,
                &resample,
                appsink.upcast_ref(),
            ])
            .expect("add the audio elements");
        filesrc
            .link(&decodebin)
            .expect("link filesrc to decodebin3");
        gst::Element::link_many([&convert, &resample, appsink.upcast_ref()])
            .expect("link the audio conversion chain");
        let sink_pad = convert
            .static_pad("sink")
            .expect("audioconvert has a sink pad");
        decodebin.connect_pad_added(move |_, pad| {
            if pad.name().starts_with("audio_") && !sink_pad.is_linked() {
                let _ = pad.link(&sink_pad);
            }
        });

        let errors: Arc<Mutex<Option<String>>> = Arc::default();
        let streams: Arc<Mutex<Option<gst::StreamCollection>>> = Arc::default();
        install(&pipeline, errors.clone(), streams.clone());
        let pipeline = Stopper(pipeline);
        let watch = Watch {
            cancel,
            error: errors.clone(),
        };
        if pipeline.set_state(gst::State::Paused).is_err() {
            return Err(watch.failure("could not open the sound"));
        }
        // `decodebin3` posts its stream collection before it exposes any pad,
        // and it does **not** post `no-more-pads` (measured), so the collection
        // is also how a file with no audio track is recognised — waiting for
        // the appsink to preroll would wait forever.
        let deadline = Instant::now() + COLLECTION_TIMEOUT;
        let audio = loop {
            watch.check()?;
            if let Some(collection) = &*streams.lock().expect("the stream slot isn't poisoned") {
                break collection
                    .iter()
                    .find(|s| s.stream_type().contains(gst::StreamType::AUDIO))
                    .and_then(|s| s.stream_id());
            }
            if Instant::now() >= deadline {
                return Err(watch.failure("the sound's streams never appeared"));
            }
            std::thread::sleep(Duration::from(POLL));
        };
        let Some(audio) = audio else {
            return Ok(None);
        };
        // Only the audio stream: an unselected video stream still gets a
        // decoder and still decodes every frame (measured: 2.2 s against
        // 46 ms for 10 s of 1080p, and the export decodes that video already).
        decodebin.send_event(gst::event::SelectStreams::new([audio.as_str()]));
        loop {
            watch.check()?;
            match pipeline.state(POLL) {
                (Ok(_), gst::State::Paused, gst::State::VoidPending) => break,
                (Err(_), ..) => {
                    watch.check()?;
                    return Err(CompositeError::Failed("could not read the sound".into()));
                }
                _ => {}
            }
        }
        if pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure("could not play the sound"));
        }
        Ok(Some(Reader {
            pipeline,
            appsink,
            path: path.to_owned(),
            errors,
            held: Vec::new(),
            spent: 0,
            done: false,
            stopped: None,
            out: Vec::new(),
        }))
    }

    /// Puts the cursor at `seconds`, exactly.
    ///
    /// **Flushing and ACCURATE**, not the decode side's
    /// `KEY_UNIT | SNAP_BEFORE`: that lands on the packet before the target,
    /// so every play segment would open with sound from before it. Measured at
    /// 1–11 ms and sample-exact.
    fn seek(&mut self, seconds: f64) {
        self.held.clear();
        self.spent = 0;
        self.done = false;
        self.stopped = None;
        if self
            .pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                seconds_to_clock(seconds),
            )
            .is_err()
        {
            eprintln!(
                "export: {} refused a seek to {seconds} s",
                self.path.display()
            );
            self.done = true;
            self.stopped = Some(CompositeError::Failed(format!(
                "{} refused a seek to {seconds} s",
                self.path.display()
            )));
        }
    }

    /// The next `frames` sample positions, interleaved, **zero-padded past the
    /// end of the file** — a recording shorter than its entry, or a source that
    /// runs out, goes quiet rather than stopping the export.
    fn read(&mut self, frames: usize, cancel: &AtomicBool) -> &[f32] {
        self.out.clear();
        self.out.resize(frames * CHANNELS, 0.0);
        let mut filled = 0;
        while filled < self.out.len() {
            if self.spent == self.held.len() && !self.pull(cancel) {
                break;
            }
            let take = (self.held.len() - self.spent).min(self.out.len() - filled);
            self.out[filled..filled + take]
                .copy_from_slice(&self.held[self.spent..self.spent + take]);
            self.spent += take;
            filled += take;
        }
        &self.out
    }

    /// Everything from the cursor to the end of the file, interleaved.
    ///
    /// Not over [`Reader::read`], which pads with silence past the end and so
    /// never finishes; and a cancel or a decode error is an error here rather
    /// than a short buffer, which a caller cannot tell from a whole one
    /// (spec S2).
    pub(crate) fn rest(&mut self, cancel: &AtomicBool) -> Result<Vec<f32>, CompositeError> {
        let watch = Watch {
            cancel,
            error: self.errors.clone(),
        };
        let mut all = self.held[self.spent..].to_vec();
        self.spent = self.held.len();
        loop {
            // The cancel flag between pulls as well as inside one: `pull` only
            // reaches its own check when the sink starves, and a file decoding
            // faster than it is read never starves.
            watch.check()?;
            if !self.pull(cancel) {
                break;
            }
            all.extend_from_slice(&self.held[self.spent..]);
            self.spent = self.held.len();
        }
        match &self.stopped {
            Some(why) => Err(why.clone()),
            None => Ok(all),
        }
    }

    /// Pulls the next non-empty buffer into `held`. `false` once nothing more
    /// will come, with `stopped` saying why unless it was the end of the file.
    fn pull(&mut self, cancel: &AtomicBool) -> bool {
        let watch = Watch {
            cancel,
            error: self.errors.clone(),
        };
        while !self.done {
            if let Some(sample) = self.appsink.try_pull_sample(POLL) {
                if let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) {
                    let samples: Vec<f32> = map
                        .as_slice()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect();
                    if !samples.is_empty() {
                        self.held = samples;
                        self.spent = 0;
                        return true;
                    }
                }
                continue;
            }
            if self.appsink.is_eos() {
                self.done = true;
            } else if let Err(e) = watch.check() {
                if let CompositeError::Failed(e) = &e {
                    eprintln!("export: the sound of {} stopped: {e}", self.path.display());
                }
                self.done = true;
                self.stopped = Some(e);
            }
        }
        false
    }
}

/// Records `pipeline`'s first `ERROR` and its stream collection, and drops
/// every message: nothing here needs a GL context, and nothing polls the bus.
fn install(
    pipeline: &gst::Pipeline,
    errors: Arc<Mutex<Option<String>>>,
    streams: Arc<Mutex<Option<gst::StreamCollection>>>,
) {
    pipeline
        .bus()
        .expect("a pipeline has a bus")
        .set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(err) => {
                    errors
                        .lock()
                        .expect("the error slot isn't poisoned")
                        .get_or_insert_with(|| crate::error_text(err));
                }
                gst::MessageView::StreamCollection(collection) => {
                    *streams.lock().expect("the stream slot isn't poisoned") =
                        Some(collection.stream_collection());
                }
                _ => {}
            }
            gst::BusSyncReply::Drop
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_covers_its_output_frame_less_the_priming_head() {
        // Frame 0 loses the dropped head; every later frame is a whole 1600.
        assert_eq!(emitted_span(0), (0, 1600 - PRIMING_SAMPLES));
        assert_eq!(
            emitted_span(1),
            (1600 - PRIMING_SAMPLES, 3200 - PRIMING_SAMPLES)
        );
        for n in 0..100 {
            let (start, end) = emitted_span(n);
            assert!(end - start <= SAMPLES_PER_FRAME);
            assert_eq!(end, emitted_span(n + 1).0, "frame {n} leaves a gap");
        }
    }

    #[test]
    fn a_block_is_stamped_where_its_samples_sit() {
        let buffer = stamp(&[0.0; 4], 48_000, 48_002);
        assert_eq!(buffer.pts(), Some(gst::ClockTime::SECOND));
        assert_eq!(buffer.size(), 16);
    }
}
