//! The lossless join (spec L): the whole match written by copying the
//! sources' own packets, with no decoder, no encoder, no overlay and no GPU.
//!
//! ```text
//! first, every source's header: filesrc ! qtdemux ! fakesink (the gate)
//! then, one source at a time:   filesrc ! qtdemux ! h264parse ! appsink
//!                                                 ! aacparse  ! appsink
//! into, for the whole output:   appsrc ! mp4mux ! filesink <path>.part
//!                               appsrc ! that same mp4mux
//! ```
//!
//! **Rust owns the ordering, as the encoded export's pump does, and that is
//! the whole of why this cannot deadlock.** The first shape of this graph
//! gave the ordering to two `concat`s, one per track, feeding the muxer
//! directly. They switch source independently, so the muxer could end up
//! waiting for sound from source 2 while source 2's single `qtdemux` thread
//! was blocked pushing picture into a queue the video `concat` had not
//! reached yet — a deadlock, on roughly one run in three under load. Here
//! **only one source is open at a time**, its own demuxer thread carries each
//! packet straight into the muxer, and the one thing that ever waits is
//! [`Copying::wait_for_room`], whose condition says why it cannot wait for
//! good.
//!
//! **Which is why the headers are read first, in a pass of their own.** The
//! gate has to refuse a source that can't be joined *before* a byte is
//! written (spec L6), and what it reads is the caps `qtdemux` negotiates, so
//! every source is opened, asked and closed again before the muxing pipeline
//! exists at all. A header read is milliseconds against a copy's half-minute.
//!
//! **The copying `appsink`s are `async=false`,** and that is not a detail: a
//! bin will not commit `PLAYING` while a sink's asynchronous state change is
//! still outstanding, and with two sinks on one demuxer thread and no queues
//! the second one's never completes — the first blocks that thread in preroll
//! before a packet of the other stream has been read. `async=false` takes the
//! sinks out of that accounting, so the pipeline reaches `PLAYING`, which
//! releases the preroll, and from there they render everything they are
//! given. In the **header** pass that same preroll is exactly the bound that
//! is wanted — one packet read per file, no more — so there the sinks are
//! ordinary `fakesink`s and nothing is ever taken to `PLAYING`.
//!
//! **The timestamps are not rewritten.** Each source's packets are pushed
//! with their own PTS and DTS — negative DTS, edit lists and all — and with
//! a copy of `qtdemux`'s own segment whose *base* is where that source starts
//! in the output. Running time is then continuous across the join, which is
//! what `concat`'s `adjust-base` did, and the muxer sees the same stream it
//! always did.
//!
//! **`concat` advanced each track by its own length; one base advances both**
//! (spec L7). An MP4's two tracks rarely end on the same instant, so the old
//! graph slipped the sound against the picture by each source's own delta and
//! the slips added up (measured: 9 ms over two sources). One base per source
//! leaves a source's own A/V alignment exactly as its file has it.
//!
//! **Nothing downstream refuses a mismatch for us.** Concatenating a 320×240
//! and a 640×480 H.264 file through this graph produced no error and no
//! warning (measured): one file, one `stsd`, describing most of its samples
//! wrongly. [`Gate`] is the only thing standing between the coach and a
//! silently broken 2 GB file, and it reads the caps `qtdemux` negotiates,
//! which is where the parameter sets already are.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
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

/// How far ahead of the muxer a track may run before a push waits, in bytes
/// of queued packets — about six seconds of the design footage.
///
/// It bounds what the copy holds in memory rather than pacing it; the disk
/// does that. It is a floor rather than a ceiling, because the wait it drives
/// is conditional ([`Copying::wait_for_room`]) and a track the muxer is not
/// asking for yet runs past it — by the skew between a source's own two
/// tracks, which is milliseconds on real footage.
const AHEAD: u64 = 4 << 20;

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
    let files = files(job)?;
    // Every source is asked first and refused here, so the muxing pipeline —
    // and with it the `.part` — is built only once they can all be joined.
    let audio_rate = declare(&files, &watch)?;

    let out = Output::start(part, total, audio_rate, &watch)?;
    let mut percent = 0;
    for file in &files {
        let source = Source::play(file, &out.copying, &watch)?;
        out.until(&watch, &mut percent, on_message, || {
            source.done(audio_rate.is_some())
        })?;
        // The source is closed here rather than at the end of the run, and
        // the next one starts where everything so far ended.
        drop(source);
        out.copying.next_source();
    }
    out.close();
    out.until(&watch, &mut percent, on_message, || {
        out.eos.load(Ordering::SeqCst)
    })?;

    on_message(ExportMessage::Progress(total));
    Ok(Rendered {
        encoder: "copy".into(),
        // A copy selects no decoder, uploads nothing and has no GL platform
        // (spec X5).
        diagnostics: Diagnostics::default(),
        reserve_remaining: out.reserve_remaining(),
    })
}

/// The file each entry reads, in entry order (spec L1b).
fn files(job: &ExportJob) -> Result<Vec<&Path>, ExportError> {
    job.compilation
        .plan
        .entries
        .iter()
        .map(|entry| {
            job.sources
                .get(entry.source_index)
                .map(PathBuf::as_path)
                .ok_or_else(|| ExportError::Failed("a whole-match entry has no game video".into()))
        })
        .collect()
}

/// Which of the muxer's tracks a source's stream feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Track {
    Video,
    Audio,
}

/// Reads every file's header, puts its streams to the [`Gate`], and returns
/// the sample rate of the sound they all carry — `None` for none at all (spec
/// E4) — or the refusal that stops the copy before it has written anything.
///
/// Each file gets `filesrc ! qtdemux` with a `fakesink` per stream, in
/// `PAUSED`: `qtdemux` parses the header there and adds the pads the gate
/// reads, and the sinks' preroll is what stops the files being read any
/// further than that.
fn declare(files: &[&Path], watch: &Watch) -> Result<Option<u32>, ExportError> {
    // An error slot of the pass's own: what a file says on its way to NULL,
    // once the gate has read it, is not the copy's business. The cancel flag
    // is still the caller's.
    let watch = &Watch {
        cancel: watch.cancel,
        error: Arc::default(),
    };
    let gate = Arc::new(Gate::new(files.len()));
    // Every file is opened before any is asked, so one that can't be read is
    // refused with the same message wherever in the list it sits. They are
    // held here only to keep them open until the gate has finished.
    let mut headers = Vec::with_capacity(files.len());
    for (index, file) in files.iter().enumerate() {
        gate.name(index, file_name(file));
        let pipeline = gst::Pipeline::new();
        let source = make("filesrc")?;
        source.set_property("location", file);
        let demux = make("qtdemux")?;
        let (video, audio) = (make("fakesink")?, make("fakesink")?);
        add_many(&pipeline, &[&source, &demux, &video, &audio])?;
        link(&source, &demux)?;
        let declaring = gate.clone();
        demux.connect_pad_added(move |_, pad| declaring.declare(index, pad, &video, &audio));
        let no_more = gate.clone();
        demux.connect_no_more_pads(move |_| no_more.declared(index));
        install_bus(&pipeline, watch);
        let pipeline = Stopper(pipeline);
        if pipeline.set_state(gst::State::Paused).is_err() {
            return Err(unreadable(&file_name(file)));
        }
        headers.push(pipeline);
    }
    gate.wait(watch, DECLARE)?;
    gate.check()?;
    Ok(gate.audio_rate())
}

/// The source being copied: `filesrc ! qtdemux` with a branch per stream,
/// running from the moment it is built.
struct Source {
    pipeline: Stopper,
    video: Branch,
    audio: Branch,
    /// Cleared when this source is done with, so a push of its that is
    /// waiting for room returns. Its branches hold the other end.
    live: Arc<AtomicBool>,
}

impl Drop for Source {
    /// Releases a push that is waiting for room **before** the pipeline is
    /// taken to NULL, which waits for the very thread that push is on.
    /// Fields drop after this.
    fn drop(&mut self) {
        self.live.store(false, Ordering::SeqCst);
    }
}

impl Source {
    /// Opens `file` and copies it into `copying`, from here to its EOS.
    fn play(file: &Path, copying: &Arc<Copying>, watch: &Watch) -> Result<Source, ExportError> {
        let pipeline = gst::Pipeline::new();
        let source = make("filesrc")?;
        source.set_property("location", file);
        let demux = make("qtdemux")?;
        add_many(&pipeline, &[&source, &demux])?;
        link(&source, &demux)?;
        let live = Arc::new(AtomicBool::new(true));
        let video = Branch::build(&pipeline, "h264parse", Track::Video, copying, &live)?;
        let audio = Branch::build(&pipeline, "aacparse", Track::Audio, copying, &live)?;

        // The branches are built before the pads, because a `pad-added`
        // handler runs on the demuxer's own thread and has no way to report a
        // failure to build one. Which stream goes where needs no checking
        // here: the gate has read this file's caps and agreed to them.
        let (into_video, into_audio) = (video.head.clone(), audio.head.clone());
        demux.connect_pad_added(move |_, pad| {
            let Some(media) = media_type(pad) else { return };
            let into = if media.starts_with("video/") {
                &into_video
            } else if media.starts_with("audio/") {
                &into_audio
            } else {
                return;
            };
            let _ = pad.link(&into.static_pad("sink").expect("a parser has a sink pad"));
        });
        install_bus(&pipeline, watch);

        let opened = Source {
            pipeline: Stopper(pipeline),
            video,
            audio,
            live,
        };
        if opened.pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure(format!("could not read {}", file_name(file))));
        }
        Ok(opened)
    }

    /// Every stream this source carries has reached EOS, so all of its
    /// packets are in the muxer's hands.
    fn done(&self, with_audio: bool) -> bool {
        self.video.eos.load(Ordering::SeqCst)
            && (!with_audio || self.audio.eos.load(Ordering::SeqCst))
    }
}

/// One stream's `<parser> ! appsink`, waiting for the demuxer's pad.
///
/// **There is no `queue`**, and nothing downstream of the sink blocks except
/// the one wait this module states, so the demuxer's own thread carries each
/// packet all the way into the muxer, in the order its file has them.
struct Branch {
    /// What the demuxer's pad links into.
    head: gst::Element,
    /// Set by the sink's EOS: this stream of this source is fully copied. A
    /// stream the source hasn't got never sets it, which is why
    /// [`Source::done`] is told which to expect.
    eos: Arc<AtomicBool>,
}

impl Branch {
    fn build(
        pipeline: &gst::Pipeline,
        parser: &str,
        track: Track,
        copying: &Arc<Copying>,
        live: &Arc<AtomicBool>,
    ) -> Result<Branch, ExportError> {
        let head = make(parser)?;
        let sink = gst_app::AppSink::builder()
            // Nothing here waits on a clock: the copy runs as fast as the
            // disk. And `async=false` is what lets it reach `PLAYING` at all
            // — see this module's header.
            .sync(false)
            .async_(false)
            .build();
        add_many(pipeline, &[&head, sink.upcast_ref()])?;
        link(&head, sink.upcast_ref())?;
        let eos: Arc<AtomicBool> = Arc::default();
        let (carrying, living, ended) = (copying.clone(), live.clone(), eos.clone());
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    carrying.carry(track, &sample, &living);
                    Ok(gst::FlowSuccess::Ok)
                })
                .eos(move |_| ended.store(true, Ordering::SeqCst))
                .build(),
        );
        Ok(Branch { head, eos })
    }
}

/// The muxing pipeline: an `appsrc` per track into one `mp4mux`, writing the
/// `.part`.
///
/// **It exists only once the gate has passed** — that is what "a refusal
/// leaves nothing behind" means here: until then there is no `filesink`, and
/// so no file.
struct Output {
    pipeline: Stopper,
    mux: gst::Element,
    copying: Arc<Copying>,
    eos: Arc<AtomicBool>,
}

impl Output {
    /// Builds and starts the muxing pipeline. `audio_rate` is the sample rate
    /// of the sources' sound, or `None` when they have none (spec E4).
    fn start(
        part: &Path,
        total: usize,
        audio_rate: Option<u32>,
        watch: &Watch,
    ) -> Result<Output, ExportError> {
        let pipeline = gst::Pipeline::new();
        let mux = make("mp4mux")?;
        // `moov` first, in space reserved up front, with no temp file: the
        // layout `chapters::splice` needs, by the encoded export's formula
        // (spec L4). `faststart` would write the whole `mdat` to `$TMPDIR`.
        mux.set_property("reserved-max-duration", reserved_duration(total));
        let sink = make("filesink")?;
        sink.set_property("location", part);
        add_many(&pipeline, &[&mux, &sink])?;
        link(&mux, &sink)?;

        let copying = Arc::new(Copying {
            video: feed(&pipeline, &mux, "video_%u", VIDEO_TIMESCALE)?,
            audio: audio_rate
                .map(|rate| feed(&pipeline, &mux, "audio_%u", rate))
                .transpose()?,
            at: Mutex::default(),
            frames: AtomicUsize::new(0),
            total,
        });
        let eos = install_bus(&pipeline, watch);
        let out = Output {
            pipeline: Stopper(pipeline),
            mux,
            copying,
            eos,
        };
        if out.pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure("could not start the copy"));
        }
        Ok(out)
    }

    /// Polls until `done`, reporting the frames copied so far each time their
    /// whole percent changes (spec X3), and re-raising the first error any
    /// pipeline posted.
    fn until(
        &self,
        watch: &Watch,
        percent: &mut usize,
        on_message: &mut impl FnMut(ExportMessage),
        done: impl Fn() -> bool,
    ) -> Result<(), ExportError> {
        loop {
            // Read before the check: an error is recorded before any EOS.
            let finished = done();
            watch.check()?;
            let frames = self.copying.frames.load(Ordering::SeqCst);
            let now = frames * 100 / self.copying.total.max(1);
            if now != *percent {
                *percent = now;
                on_message(ExportMessage::Progress(frames));
            }
            if finished {
                return Ok(());
            }
            std::thread::sleep(POLL.into());
        }
    }

    /// Tells the muxer there are no more sources.
    fn close(&self) {
        let _ = self.copying.video.end_of_stream();
        if let Some(audio) = &self.copying.audio {
            let _ = audio.end_of_stream();
        }
    }

    /// What is left of the `moov` reserve, in seconds (spec L4, E7). `None`
    /// while the muxer has not accounted for any of it.
    fn reserve_remaining(&self) -> Option<f64> {
        gst::ClockTime::try_from(self.mux.property::<u64>("reserved-duration-remaining"))
            .ok()
            .map(seconds)
    }
}

/// One track of the muxer, fed by an `appsrc` with `trak-timescale` pinned to
/// `timescale` (spec L3).
fn feed(
    pipeline: &gst::Pipeline,
    mux: &gst::Element,
    template: &str,
    timescale: u32,
) -> Result<gst_app::AppSrc, ExportError> {
    let src = gst_app::AppSrc::builder()
        .format(gst::Format::Time)
        .is_live(false)
        // The copy does its own waiting, where it can be released: a blocking
        // push never returns after a downstream error.
        .block(false)
        .max_bytes(AHEAD)
        .build();
    // Each source arrives with its own segment, re-based onto the output's
    // timeline; without this `appsrc` would keep the first one and warn.
    src.set_property("handle-segment-change", true);
    add_many(pipeline, &[src.upcast_ref()])?;
    let pad = mux.request_pad_simple(template).ok_or_else(|| {
        ExportError::Failed(format!("the muxer gave no {template} pad for the copy"))
    })?;
    pad.set_property("trak-timescale", timescale);
    link_pads(src.upcast_ref(), &pad)?;
    Ok(src)
}

/// The muxer's tracks, and where on the output's timeline the copy has
/// reached. Every source's packets pass through here, one source at a time.
///
/// There is no third track: the scoreboard is a sidecar file (spec T1).
struct Copying {
    video: gst_app::AppSrc,
    /// `None` when no source has sound (spec E4).
    audio: Option<gst_app::AppSrc>,
    at: Mutex<At>,
    /// Output frames pushed so far, read for progress by the copy thread.
    frames: AtomicUsize,
    /// The plan's frame count: what progress is clamped to, and the
    /// denominator everything else in the run divides by.
    total: usize,
}

/// Where the copy has reached on the output's timeline.
#[derive(Default)]
struct At {
    /// Where the source being copied starts. **One offset for both tracks**,
    /// so a source's own A/V alignment survives the join (spec L7).
    offset: gst::ClockTime,
    /// Just past the last packet pushed, on either track: where the next
    /// source will start.
    end: gst::ClockTime,
}

impl Copying {
    /// Carries one packet from the source being copied into the muxer's
    /// `track`, on the output's timeline.
    ///
    /// The packet itself is untouched. What changes is the segment it rides:
    /// a copy of `qtdemux`'s own, based at the source's place in the output,
    /// so running time continues across the join and the muxer reads every
    /// PTS, DTS and edit list exactly as the file wrote them.
    fn carry(&self, track: Track, sample: &gst::Sample, live: &AtomicBool) {
        let src = match track {
            Track::Video => &self.video,
            Track::Audio => match &self.audio {
                Some(audio) => audio,
                // A source with sound the gate let through always has a track
                // to put it on; anything else is not carried.
                None => return,
            },
        };
        let (Some(buffer), Some(segment)) = (sample.buffer_owned(), sample.segment()) else {
            return;
        };
        let Some(segment) = segment.downcast_ref::<gst::ClockTime>() else {
            return;
        };
        let mut segment = segment.clone();
        // **The re-based segment has no stop.** `appsrc` takes the segment it
        // is handed as its own, and `basesrc` ends the stream the moment a
        // buffer passes that stop — which, with the source's own, is its last
        // packet (measured: everything after the first source was dropped).
        // When the output ends is the copy's business, not a source's.
        segment.set_stop(gst::ClockTime::NONE);
        let at_time = {
            let mut at = self.at.lock().expect("the copy's place isn't poisoned");
            segment.set_base(at.offset);
            let at_time = buffer.pts().and_then(|pts| segment.to_running_time(pts));
            if let Some(at_time) = at_time {
                // Every MP4 sample has a duration (`stts`); the fallback is
                // only there to keep the next source off this packet's own
                // instant.
                at.end = (at_time + buffer.duration().unwrap_or(gst::ClockTime::ZERO)).max(at.end);
            }
            at_time
        };
        if let (Track::Video, Some(at_time)) = (track, at_time) {
            let frames = (seconds(at_time) * f64::from(OUTPUT_FPS)).round() as usize;
            self.frames.store(frames.min(self.total), Ordering::SeqCst);
        }
        self.wait_for_room(live);
        // The caps this sample was negotiated with: `appsrc` takes them as
        // its own, so the track is described by the parser rather than by
        // anything this module guessed.
        let caps = sample.caps_owned();
        let mut carried = gst::Sample::builder()
            .buffer(&buffer)
            .segment(segment.upcast_ref());
        if let Some(caps) = &caps {
            carried = carried.caps(caps);
        }
        let _ = src.push_sample(&carried.build());
    }

    /// Moves to the next source: it starts where everything before it ended.
    /// Called between sources, when no packet is in flight.
    fn next_source(&self) {
        let mut at = self.at.lock().expect("the copy's place isn't poisoned");
        at.offset = at.end;
    }

    /// Waits, while it may, for the muxer to take what is already queued.
    ///
    /// **A push waits only while every track already has something for the
    /// muxer to write, and that is the whole of why the copy cannot
    /// deadlock.** The muxer takes the earliest packet across its pads, so
    /// while every pad has one it can always write, which drains a pad, which
    /// ends the wait. A track the muxer is waiting for is never held back —
    /// and that, exactly, is the condition the two-`concat` graph died of.
    ///
    /// `live` ends the wait whatever the muxer is doing, so a cancelled or
    /// failed copy leaves no thread in here for the teardown to wait on.
    fn wait_for_room(&self, live: &AtomicBool) {
        while live.load(Ordering::SeqCst) && self.crowded() {
            std::thread::sleep(Duration::from(POLL) / 5);
        }
    }

    /// Every track has a packet for the muxer, and one of them has more than
    /// [`AHEAD`] bytes of them.
    fn crowded(&self) -> bool {
        let video = self.video.current_level_bytes();
        match self
            .audio
            .as_ref()
            .map(gst_app::AppSrc::current_level_bytes)
        {
            Some(audio) => video > 0 && audio > 0 && (video > AHEAD || audio > AHEAD),
            None => video > AHEAD,
        }
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

    /// Records `pad`, and links it into the sink that drains it. A pad this
    /// copy can't carry is refused here and left unlinked; an extra stream (a
    /// timecode or a subtitle track) is simply left unlinked, which `qtdemux`
    /// is happy with as long as something is taking data.
    fn declare(&self, index: usize, pad: &gst::Pad, video: &gst::Element, audio: &gst::Element) {
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
            self.entries.lock().expect("the gate isn't poisoned")[index].audio = Some(caps.clone());
            audio
        } else {
            return;
        };
        let sink = into.static_pad("sink").expect("a sink has a sink pad");
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

    /// The sample rate of the output's audio track, or `None` when no source
    /// has sound. Read once the gate has passed, so the first entry's rate is
    /// every entry's.
    fn audio_rate(&self) -> Option<u32> {
        let entries = self.entries.lock().expect("the gate isn't poisoned");
        let rate = entries
            .first()?
            .audio
            .as_ref()?
            .structure(0)
            .expect("negotiated caps have a structure")
            .get::<i32>("rate")
            .ok()?;
        u32::try_from(rate).ok()
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

/// `pad`'s media type, as `qtdemux` negotiated it.
fn media_type(pad: &gst::Pad) -> Option<String> {
    let caps = pad.current_caps()?;
    Some(caps.structure(0)?.name().to_string())
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

fn make(factory: &str) -> Result<gst::Element, ExportError> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        ExportError::Failed(format!(
            "the copy needs the `{factory}` element, which isn't installed"
        ))
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
