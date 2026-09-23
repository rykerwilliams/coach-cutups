//! The lossless join (spec L): the whole match written by copying the
//! sources' own packets, with no decoder, no encoder, no overlay and no GPU.
//!
//! ```text
//! per entry: filesrc ! qtdemux ! queue ! h264parse ! concat name=video
//!                              ! queue ! aacparse  ! concat name=audio
//! video ! mp4mux ! filesink <path>.part
//! audio ! that same mp4mux
//! ```
//!
//! **A `queue` after every demux pad is not optional** (measured): without
//! them the graph deadlocks on the first file, because `qtdemux`'s single
//! streaming thread pushes video into an aggregator that is waiting for that
//! same thread's audio.
//!
//! **`concat` plays its sink pads in the order they were requested**, so they
//! are all requested here, in entry order, before any pad is linked — never in
//! the `pad-added` handlers, which run on one thread per file and in no order
//! at all.
//!
//! **Nothing downstream refuses a mismatch for us.** Concatenating a 320×240
//! and a 640×480 H.264 file through this graph produced no error and no
//! warning (measured): one file, one `stsd`, describing most of its samples
//! wrongly. [`Gate`] is the only thing standing between the coach and a
//! silently broken 2 GB file, and it reads the caps `qtdemux` negotiates,
//! which is where the parameter sets already are.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use video_coach_core::export::OUTPUT_FPS;

use super::export::{reserved_duration, ExportError, ExportJob, ExportMessage, Rendered};
use super::{Stopper, Watch, POLL};
use crate::player::{seconds, Diagnostics};

/// How long every file has to declare its streams before the copy gives up.
///
/// A header read, so it is generous rather than tuned; only a file `qtdemux`
/// never finishes with reaches it, and that reads as the same refusal a
/// Matroska source gets.
const DECLARE: Duration = Duration::from_secs(20);

/// The video track's timescale (spec L3): the source's own, and the
/// conventional MPEG video clock, which represents 30, 29.97, 25 and 24 fps
/// exactly. Left automatic, `mp4mux` picks a timescale that need not divide
/// the source's, and then every sample duration rounds.
const VIDEO_TIMESCALE: u32 = 90_000;

/// The way out of every refusal, which is one picker away.
const RE_ENCODE: &str = "choose Scoreboard: burned in to export it re-encoded";

/// A file this copy can't read at all: not MP4, not H.264, or one `qtdemux`
/// never produced a video pad from.
fn unreadable(name: &str) -> ExportError {
    ExportError::Failed(format!(
        "{name} isn't H.264 in MP4, so it can't be copied; {RE_ENCODE}"
    ))
}

/// Copies `job`'s entries into `part`, in entry order.
///
/// The file it leaves is finished but unchaptered and still named `.part`:
/// [`run`](super::export::run) owns the rest, for both renderers alike.
pub(super) fn copy(
    job: &ExportJob,
    part: &Path,
    cancel: &AtomicBool,
    on_message: &mut impl FnMut(ExportMessage),
) -> Result<Rendered, ExportError> {
    let total = job.compilation.plan.total_frames();
    let watch = Watch {
        cancel,
        error: Arc::default(),
    };
    let graph = Graph::build(job, part, &watch)?;
    if graph.pipeline.set_state(gst::State::Playing).is_err() {
        return Err(watch.failure("could not start the copy"));
    }
    graph.gate.wait(&watch, DECLARE)?;
    graph.gate.check()?;

    let mut percent = 0;
    loop {
        // Read before the check: an error is recorded before any EOS.
        let done = graph.eos.load(Ordering::SeqCst);
        watch.check()?;
        let frames = graph.frames.load(Ordering::SeqCst);
        let now = frames * 100 / total.max(1);
        if now != percent {
            percent = now;
            on_message(ExportMessage::Progress(frames));
        }
        if done {
            on_message(ExportMessage::Progress(total));
            return Ok(Rendered {
                encoder: "copy".into(),
                // A copy selects no decoder, uploads nothing and has no GL
                // platform (spec X5).
                diagnostics: Diagnostics::default(),
                reserve_remaining: graph.reserve_remaining(),
            });
        }
        std::thread::sleep(POLL.into());
    }
}

/// The copy's pipeline and what the copy thread reads while it runs.
struct Graph {
    pipeline: Stopper,
    mux: gst::Element,
    gate: Arc<Gate>,
    /// Output frames muxed so far, written by the progress probe.
    frames: Arc<AtomicUsize>,
    eos: Arc<AtomicBool>,
}

impl Graph {
    fn build(job: &ExportJob, part: &Path, watch: &Watch) -> Result<Graph, ExportError> {
        let entries = &job.compilation.plan.entries;
        let gate = Arc::new(Gate::new(entries.len()));
        let pipeline = gst::Pipeline::new();

        let mux = make("mp4mux")?;
        // `moov` first, in space reserved up front, with no temp file: the
        // layout `chapters::splice` needs, by the encoded export's formula
        // (spec L4). `faststart` would write the whole `mdat` to `$TMPDIR`.
        mux.set_property(
            "reserved-max-duration",
            reserved_duration(job.compilation.plan.total_frames()),
        );
        let sink = make("filesink")?;
        sink.set_property("location", part);
        let video = make("concat")?;
        let audio = make("concat")?;
        add_many(&pipeline, &[&mux, &sink, &video, &audio])?;
        link(&mux, &sink)?;

        let video_pad = request(&mux, "video_%u")?;
        video_pad.set_property("trak-timescale", VIDEO_TIMESCALE);
        let frames = install_progress(&video_pad, job.compilation.plan.total_frames());
        link_pads(&video, &video_pad)?;

        // Requested here, all of them, so the order they play in is the entry
        // order rather than the order the files happen to parse in.
        let video_pads = sink_pads(&video, entries.len());
        let audio_pads = sink_pads(&audio, entries.len());
        let audio = Arc::new(AudioTrack {
            concat: audio,
            mux: mux.clone(),
            linked: Mutex::new(false),
        });

        for (index, entry) in entries.iter().enumerate() {
            let file = job.sources.get(entry.source_index).ok_or_else(|| {
                ExportError::Failed("a whole-match entry has no game video".into())
            })?;
            gate.name(index, file_name(file));
            let source = make("filesrc")?;
            source.set_property("location", file);
            let demux = make("qtdemux")?;
            add_many(&pipeline, &[&source, &demux])?;
            link(&source, &demux)?;
            let video_branch = branch(&pipeline, "h264parse", &video_pads[index])?;
            let audio_branch = branch(&pipeline, "aacparse", &audio_pads[index])?;

            let (added, track) = (gate.clone(), audio.clone());
            demux.connect_pad_added(move |_, pad| {
                added.declare(index, pad, &video_branch, &audio_branch, &track);
            });
            let no_more = gate.clone();
            demux.connect_no_more_pads(move |_| no_more.declared(index));
        }

        let eos = install_bus(&pipeline, watch);
        Ok(Graph {
            pipeline: Stopper(pipeline),
            mux,
            gate,
            frames,
            eos,
        })
    }

    /// What is left of the `moov` reserve, in seconds (spec L4, E7). `None`
    /// while the muxer has not accounted for any of it.
    fn reserve_remaining(&self) -> Option<f64> {
        gst::ClockTime::try_from(self.mux.property::<u64>("reserved-duration-remaining"))
            .ok()
            .map(seconds)
    }
}

/// The audio track, which exists only if a source has one (spec E4).
///
/// The muxer's pad is requested by the **first** audio pad any file declares,
/// which is always before that file pushes a buffer — `qtdemux` adds every pad
/// before it streams — and so always before the muxer starts. `mp4mux` refuses
/// a pad after that, which is what makes the ordering safe rather than lucky.
struct AudioTrack {
    concat: gst::Element,
    mux: gst::Element,
    linked: Mutex<bool>,
}

impl AudioTrack {
    /// Links the audio `concat` into the muxer, once, with the track
    /// timescale `caps` asks for (spec L3).
    fn start(&self, caps: &gst::Caps) -> Result<(), String> {
        let mut linked = self.linked.lock().expect("the audio slot isn't poisoned");
        if *linked {
            return Ok(());
        }
        let pad = self
            .mux
            .request_pad_simple("audio_%u")
            .ok_or("the muxer had already started")?;
        let rate = caps
            .structure(0)
            .expect("negotiated caps have a structure")
            .get::<i32>("rate")
            .ok()
            .and_then(|rate| u32::try_from(rate).ok());
        if let Some(rate) = rate {
            pad.set_property("trak-timescale", rate);
        }
        link_pads(&self.concat, &pad).map_err(|e| e.to_string())?;
        *linked = true;
        Ok(())
    }
}

/// What every entry's demuxer told the gate, and the refusal it earned.
///
/// The **absolute** checks happen in the `pad-added` handler, where the caps
/// are; the **relative** ones in [`Gate::check`], once every entry has spoken,
/// because the first entry's caps need not arrive first.
struct Gate {
    entries: Mutex<Vec<Declared>>,
    refusal: Mutex<Option<String>>,
}

/// One entry's streams, as `qtdemux` negotiated them.
#[derive(Default)]
struct Declared {
    name: String,
    video: Option<gst::Caps>,
    audio: Option<gst::Caps>,
    /// `no-more-pads`: this entry has nothing left to say.
    done: bool,
}

impl Gate {
    fn new(entries: usize) -> Gate {
        Gate {
            entries: Mutex::new((0..entries).map(|_| Declared::default()).collect()),
            refusal: Mutex::new(None),
        }
    }

    /// Records `pad`, and links it into the branch that carries it. A pad
    /// this copy can't carry is refused here and left unlinked; an extra
    /// stream (a timecode or a subtitle track) is simply left unlinked, which
    /// `qtdemux` is happy with as long as something is taking data.
    fn declare(
        &self,
        index: usize,
        pad: &gst::Pad,
        video: &gst::Element,
        audio: &gst::Element,
        track: &AudioTrack,
    ) {
        let Some(caps) = pad.current_caps() else {
            return;
        };
        let name = self.entries.lock().expect("the gate isn't poisoned")[index]
            .name
            .clone();
        let structure = caps.structure(0).expect("negotiated caps have a structure");
        let media = structure.name();
        let into = if media.starts_with("video/") {
            if media != "video/x-h264" {
                return self.refuse(unreadable(&name).to_string());
            }
            self.entries.lock().expect("the gate isn't poisoned")[index].video = Some(caps.clone());
            video
        } else if media.starts_with("audio/") {
            if media != "audio/mpeg" || structure.get::<i32>("mpegversion") != Ok(4) {
                return self.refuse(format!(
                    "{name}'s sound isn't AAC, so it can't be copied; {RE_ENCODE}"
                ));
            }
            if let Err(why) = track.start(&caps) {
                return self.refuse(format!(
                    "{name}'s sound can't be joined to the others ({why}); {RE_ENCODE}"
                ));
            }
            self.entries.lock().expect("the gate isn't poisoned")[index].audio = Some(caps.clone());
            audio
        } else {
            return;
        };
        let sink = into.static_pad("sink").expect("a queue has a sink pad");
        if pad.link(&sink).is_err() {
            self.refuse(format!("{name}'s {media} stream could not be copied"));
        }
    }

    /// What a refusal calls entry `index`'s file.
    fn name(&self, index: usize, name: String) {
        self.entries.lock().expect("the gate isn't poisoned")[index].name = name;
    }

    /// `no-more-pads` for entry `index`.
    fn declared(&self, index: usize) {
        self.entries.lock().expect("the gate isn't poisoned")[index].done = true;
    }

    fn refuse(&self, why: String) {
        let mut refusal = self.refusal.lock().expect("the gate isn't poisoned");
        refusal.get_or_insert(why);
    }

    /// Waits, **with a bound**, for every entry's `no-more-pads`.
    ///
    /// An error or a timeout here is a file this copy can't read — a Matroska
    /// source, or one with no video track — and it is reported as that rather
    /// than as whatever GStreamer said, which a coach can't act on.
    fn wait(&self, watch: &Watch, bound: Duration) -> Result<(), ExportError> {
        let deadline = Instant::now() + bound;
        loop {
            self.check()?;
            let Some(name) = self.undeclared() else {
                return Ok(());
            };
            match watch.check() {
                // A cancel is the caller's answer, not the file's.
                Err(ExportError::Cancelled) => return Err(ExportError::Cancelled),
                // Whatever GStreamer said about a file it couldn't demux, this
                // is what the coach can act on.
                Err(_) => return Err(unreadable(&name)),
                Ok(()) if Instant::now() >= deadline => return Err(unreadable(&name)),
                Ok(()) => std::thread::sleep(POLL.into()),
            }
        }
    }

    /// The first entry that has not declared a video track yet, if any.
    fn undeclared(&self) -> Option<String> {
        self.entries
            .lock()
            .expect("the gate isn't poisoned")
            .iter()
            .find(|e| !e.done || e.video.is_none())
            .map(|e| e.name.clone())
    }

    /// The refusal, if any: the absolute checks', or the relative ones
    /// against the first entry (spec L6).
    fn check(&self) -> Result<(), ExportError> {
        if let Some(why) = &*self.refusal.lock().expect("the gate isn't poisoned") {
            return Err(ExportError::Failed(why.clone()));
        }
        let entries = self.entries.lock().expect("the gate isn't poisoned");
        // Nothing is relative to a first entry that hasn't spoken yet: the
        // files parse on a thread each, in no order, and [`Gate::wait`] is
        // what refuses one that never speaks at all.
        let Some(first) = entries.first().filter(|f| f.done && f.video.is_some()) else {
            return Ok(());
        };
        for entry in entries.iter().skip(1) {
            // One `stsd` is written per track, and it carries the parameter
            // sets, and so the profile, level, resolution and chroma with
            // them: comparing the bytes is one comparison instead of six, and
            // it cannot be fooled.
            if entry.video.is_some() && codec_data(&entry.video) != codec_data(&first.video) {
                return Err(ExportError::Failed(format!(
                    "{} was recorded differently from the first video (its H.264 \
                     parameters differ); {RE_ENCODE}",
                    entry.name
                )));
            }
            if entry.done && entry.audio.is_some() != first.audio.is_some() {
                return Err(ExportError::Failed(format!(
                    "{} has sound the first video hasn't, or the other way about; {RE_ENCODE}",
                    entry.name
                )));
            }
            if entry.audio.is_some() && codec_data(&entry.audio) != codec_data(&first.audio) {
                return Err(ExportError::Failed(format!(
                    "{}'s sound was recorded differently from the first video's; {RE_ENCODE}",
                    entry.name
                )));
            }
        }
        Ok(())
    }
}

/// `caps`' `codec_data` bytes: the parameter sets for video, the audio
/// specific config for sound. `None` where there are none, which two streams
/// still have to agree on.
fn codec_data(caps: &Option<gst::Caps>) -> Option<Vec<u8>> {
    let buffer = caps
        .as_ref()?
        .structure(0)
        .expect("negotiated caps have a structure")
        .get::<gst::Buffer>("codec_data")
        .ok()?;
    let map = buffer.map_readable().ok()?;
    Some(map.to_vec())
}

/// Output frames muxed so far, from the buffers reaching `pad`.
///
/// **Running time, not PTS** (spec X3): a source with an edit list starts its
/// segment after 0, and `concat`'s `adjust-base` moves each later source's
/// base on top of that. The count is clamped to the plan's, which is the
/// denominator everything else in the run divides by.
fn install_progress(pad: &gst::Pad, total: usize) -> Arc<AtomicUsize> {
    let frames = Arc::new(AtomicUsize::new(0));
    let segment: Mutex<Option<gst::FormattedSegment<gst::ClockTime>>> = Mutex::new(None);
    let counted = frames.clone();
    pad.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
        move |_, info| {
            match &info.data {
                Some(gst::PadProbeData::Event(event)) => {
                    if let gst::EventView::Segment(s) = event.view() {
                        *segment.lock().expect("the segment isn't poisoned") =
                            s.segment().downcast_ref::<gst::ClockTime>().cloned();
                    }
                }
                Some(gst::PadProbeData::Buffer(buffer)) => {
                    let at = segment
                        .lock()
                        .expect("the segment isn't poisoned")
                        .as_ref()
                        .zip(buffer.pts())
                        .and_then(|(segment, pts)| segment.to_running_time(pts));
                    if let Some(at) = at {
                        let n = (seconds(at) * f64::from(OUTPUT_FPS)).round() as usize;
                        counted.store(n.min(total), Ordering::SeqCst);
                    }
                }
                _ => {}
            }
            gst::PadProbeReturn::Ok
        },
    );
    frames
}

/// Records the first error `pipeline` posts into `watch`, and its EOS.
fn install_bus(pipeline: &gst::Pipeline, watch: &Watch) -> Arc<AtomicBool> {
    let (error, eos) = (watch.error.clone(), Arc::new(AtomicBool::new(false)));
    let flag = eos.clone();
    pipeline
        .bus()
        .expect("a pipeline has a bus")
        .set_sync_handler(move |_, msg| {
            match msg.view() {
                gst::MessageView::Error(err) => {
                    let mut error = error.lock().expect("the error slot isn't poisoned");
                    error.get_or_insert_with(|| crate::error_text(err));
                }
                gst::MessageView::Eos(_) => flag.store(true, Ordering::SeqCst),
                _ => {}
            }
            gst::BusSyncReply::Drop
        });
    eos
}

/// One entry's `queue ! <parser>`, linked into `into` and left waiting for the
/// demuxer's pad.
fn branch(
    pipeline: &gst::Pipeline,
    parser: &str,
    into: &gst::Pad,
) -> Result<gst::Element, ExportError> {
    let queue = make("queue")?;
    let parser = make(parser)?;
    add_many(pipeline, &[&queue, &parser])?;
    link(&queue, &parser)?;
    link_pads(&parser, into)?;
    Ok(queue)
}

/// `count` sink pads on `concat`, in order.
fn sink_pads(concat: &gst::Element, count: usize) -> Vec<gst::Pad> {
    (0..count)
        .map(|_| {
            concat
                .request_pad_simple("sink_%u")
                .expect("concat always gives a sink pad")
        })
        .collect()
}

fn make(factory: &str) -> Result<gst::Element, ExportError> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        ExportError::Failed(format!(
            "the copy needs the `{factory}` element, which isn't installed"
        ))
    })
}

fn request(element: &gst::Element, template: &str) -> Result<gst::Pad, ExportError> {
    element.request_pad_simple(template).ok_or_else(|| {
        ExportError::Failed(format!("the muxer gave no {template} pad for the copy"))
    })
}

fn add_many(pipeline: &gst::Pipeline, elements: &[&gst::Element]) -> Result<(), ExportError> {
    pipeline
        .add_many(elements)
        .map_err(|e| ExportError::Failed(format!("could not build the copy graph: {e}")))
}

fn link(from: &gst::Element, to: &gst::Element) -> Result<(), ExportError> {
    from.link(to)
        .map_err(|e| ExportError::Failed(format!("could not build the copy graph: {e}")))
}

fn link_pads(from: &gst::Element, to: &gst::Pad) -> Result<(), ExportError> {
    from.static_pad("src")
        .expect("every element linked here has a src pad")
        .link(to)
        .map(|_| ())
        .map_err(|e| ExportError::Failed(format!("could not build the copy graph: {e}")))
}

/// What a refusal calls a file: its name, which is what the coach sees in the
/// Sources list.
fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
