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
//! every source is opened, asked and closed again — **one at a time**, in
//! order — before the muxing pipeline exists at all. A header read is
//! milliseconds against a copy's half-minute, and reading them in turn means
//! the file that is refused is the first one that can't be joined, whatever
//! the machine was doing.
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
//! **Where a source starts is the plan's own `start_frame`, and only that**
//! (spec L7). `concat` advanced each track by its own length, so the sound
//! slipped against the picture by every source's A/V delta and the slips added
//! up (measured: 9 ms over two sources). Taking the base from the plan instead
//! fixes both tracks to one instant *and* makes it the same instant the
//! chapters and the cues are placed at, which is the only way those three can
//! agree by construction.
//!
//! **Nothing downstream refuses a mismatch for us.** Concatenating a 320×240
//! and a 640×480 H.264 file through this graph produced no error and no
//! warning (measured): one file, one `stsd`, describing most of its samples
//! wrongly. [`declare`] is the only thing standing between the coach and a
//! silently broken 2 GB file, and it reads the caps `qtdemux` negotiates,
//! which is where the parameter sets already are.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use video_coach_core::export::OUTPUT_FPS;

use super::export::{
    reserve_remaining, reserved_duration, ExportError, ExportJob, ExportMessage, Render, Rendered,
};
use super::{frame_time, watch_bus, Stopper, Watch, POLL};
use crate::player::{seconds, Diagnostics};

/// How long a file has to declare its streams before the copy gives up.
///
/// A header read, so it is generous rather than tuned; only a file `qtdemux`
/// never finishes with reaches it, and that reads as the same refusal a
/// Matroska source gets.
const DECLARE: Duration = Duration::from_secs(20);

/// How long the copy may make no progress at all before it is declared stuck.
///
/// **Every wait here is bounded, and this is the bound on the long one.** The
/// copy itself has no time limit — a two-gigabyte match takes as long as the
/// disk does — so what is watched is whether a packet moved, not how long the
/// run has taken. Nothing but a pipeline that will never speak again reaches
/// it: the muxer's closing write of the reserved `moov` is a seek and about a
/// megabyte.
const STALL: Duration = Duration::from_secs(60);

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
/// is conditional ([`Copying::wait_for_room`]) — see [`CEILING`] for what the
/// real bound is, and for what happens when a file exceeds it.
const AHEAD: u64 = 4 << 20;

/// What a track may hold before the copy gives up rather than buffering on.
///
/// Sixteen times [`AHEAD`]: far past any A/V divergence a camera writes —
/// six seconds of the design footage is one [`AHEAD`] — and far short of a
/// laptop's memory, so this fails a file that is malformed rather than one
/// that is merely awkward.
const CEILING: u64 = AHEAD * 16;

/// The way out of every refusal, which is one picker away.
const RE_ENCODE: &str = "choose Scoreboard: burned in to export it re-encoded";

/// A file this copy can't read at all: not MP4, not H.264, or one `qtdemux`
/// never produced a video pad from.
fn unreadable(name: &str) -> ExportError {
    ExportError::Failed(format!(
        "{name} isn't H.264 in MP4, so it can't be copied; {RE_ENCODE}"
    ))
}

/// Whether `files` — game videos, in the order a whole match would join them —
/// could be copied rather than re-encoded (spec L6).
///
/// **This is what makes the sheet's "Default" mean "the best available".** The
/// bus asks before it chooses a renderer, so a project of Matroska or HEVC
/// sources is re-encoded with the board burned in, as it was before this path
/// existed, rather than refused at a gate the coach never asked to be held to.
/// Choosing "Separate track" by hand still refuses, naming the file: there the
/// coach asked for the copy.
///
/// It is the gate's own header pass, so it costs a header read per file and
/// answers exactly what the copy would answer.
pub fn can_copy(files: &[PathBuf]) -> Result<(), ExportError> {
    let files: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let cancel = AtomicBool::new(false);
    let watch = Watch {
        cancel: &cancel,
        error: Arc::default(),
    };
    declare(&files, &watch).map(|_| ())
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
    debug_assert!(
        matches!(job.render, Render::Copy),
        "the copy renders only what asks for it"
    );
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
    for (entry, file) in job.compilation.plan.entries.iter().zip(&files) {
        // **The plan says where this source starts**, and so do the chapters
        // and the cues (spec L7). Placing it at the end of what has been
        // written instead would put every later source at the longest of its
        // predecessors' two tracks, which is not where the plan counted its
        // frames from.
        out.copying
            .start_source(frame_time(entry.start_frame as u64));
        let source = Source::play(file, &out.copying, audio_rate.is_some(), &watch)?;
        out.until(&watch, &mut percent, on_message, || source.done())?;
        // The source is closed here rather than at the end of the run.
        drop(source);
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
        reserve_remaining: reserve_remaining(&out.mux),
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

/// One file's streams, as `qtdemux` negotiated them.
#[derive(Default)]
struct Header {
    video: Option<gst::Caps>,
    audio: Option<gst::Caps>,
}

/// Reads every file's header **in turn**, holds each to the gate, and returns
/// the sample rate of the sound they all carry — `None` for none at all (spec
/// E4) — or the refusal that stops the copy before it has written anything.
///
/// One file at a time is what makes the refusal the first file that earns one,
/// on every machine and every run. Opening them all at once needed a slot per
/// entry, a shared refusal and a poll over the lot of them to say the same
/// thing more slowly.
fn declare(files: &[&Path], watch: &Watch) -> Result<Option<u32>, ExportError> {
    let mut first: Option<Header> = None;
    for file in files {
        let name = file_name(file);
        let header = read_header(file, &name, watch)?;
        absolutely(&header, &name)?;
        match &first {
            Some(first) => agree(first, &header, &name)?,
            None => first = Some(header),
        }
    }
    audio_rate(first.as_ref())
}

/// One file's streams: `filesrc ! qtdemux` with a `fakesink` per stream, in
/// `PAUSED`, until `qtdemux` says it has no more pads.
///
/// `qtdemux` parses the header there and adds the pads whose caps this reads,
/// and a `fakesink`'s preroll is what stops the file being read any further.
fn read_header(file: &Path, name: &str, caller: &Watch) -> Result<Header, ExportError> {
    // An error slot of this file's own: what it says on its way to NULL, once
    // it has been read, is not the copy's business. The cancel flag is still
    // the caller's.
    let watch = Watch {
        cancel: caller.cancel,
        error: Arc::default(),
    };
    let pipeline = gst::Pipeline::new();
    let source = make("filesrc")?;
    source.set_property("location", file);
    let demux = make("qtdemux")?;
    let (video, audio) = (make("fakesink")?, make("fakesink")?);
    add_many(&pipeline, &[&source, &demux, &video, &audio])?;
    link(&source, &demux)?;

    let header: Arc<Mutex<Header>> = Arc::default();
    let declaring = header.clone();
    demux.connect_pad_added(move |_, pad| {
        let Some(caps) = pad.current_caps() else {
            return;
        };
        let Some(media) = caps.structure(0).map(|s| s.name().to_string()) else {
            return;
        };
        let mut header = declaring.lock().expect("the header isn't poisoned");
        // A stream that is neither — a timecode, a subtitle — is left
        // unlinked, which `qtdemux` is happy with while something takes data.
        let into = if media.starts_with("video/") {
            header.video = Some(caps);
            &video
        } else if media.starts_with("audio/") {
            header.audio = Some(caps);
            &audio
        } else {
            return;
        };
        let _ = pad.link(&into.static_pad("sink").expect("a sink has a sink pad"));
    });
    let spoken = Arc::new(AtomicBool::new(false));
    let no_more = spoken.clone();
    demux.connect_no_more_pads(move |_| no_more.store(true, Ordering::SeqCst));
    // A header pipeline is never taken to PLAYING, so it never reaches EOS:
    // `no-more-pads` is what says the file has finished speaking.
    let _never_eos = watch_bus(&pipeline, &watch, |_| {});

    let pipeline = Stopper(pipeline);
    if pipeline.set_state(gst::State::Paused).is_err() {
        return Err(unreadable(name));
    }
    spoke(&watch, &spoken, name)?;
    let read = std::mem::take(&mut *header.lock().expect("the header isn't poisoned"));
    Ok(read)
}

/// Waits, **with a bound**, for one file's `no-more-pads`.
///
/// An error or a timeout here is a file this copy can't read — a Matroska
/// source, or one with no video track — and it is reported as that rather
/// than as whatever GStreamer said, which a coach can't act on.
fn spoke(watch: &Watch, spoken: &AtomicBool, name: &str) -> Result<(), ExportError> {
    let deadline = Instant::now() + DECLARE;
    loop {
        match watch.check() {
            // A cancel is the caller's answer, not the file's.
            Err(ExportError::Cancelled) => return Err(ExportError::Cancelled),
            Err(_) => return Err(unreadable(name)),
            Ok(()) if spoken.load(Ordering::SeqCst) => return Ok(()),
            Ok(()) if Instant::now() >= deadline => return Err(unreadable(name)),
            Ok(()) => std::thread::sleep(POLL.into()),
        }
    }
}

/// What one file has to be whatever the others are (spec L6): H.264 in MP4,
/// and AAC if it has sound at all.
fn absolutely(header: &Header, name: &str) -> Result<(), ExportError> {
    // No video pad at all is the same answer: `qtdemux` produced nothing this
    // copy can carry.
    let video = header.video.as_ref().ok_or_else(|| unreadable(name))?;
    if structure(video).name() != "video/x-h264" {
        return Err(unreadable(name));
    }
    if let Some(audio) = &header.audio {
        let audio = structure(audio);
        if audio.name() != "audio/mpeg" || audio.get::<i32>("mpegversion") != Ok(4) {
            return Err(ExportError::Failed(format!(
                "{name}'s sound isn't AAC, so it can't be copied; {RE_ENCODE}"
            )));
        }
    }
    Ok(())
}

/// What every file after the first has to share with it (spec L6).
fn agree(first: &Header, header: &Header, name: &str) -> Result<(), ExportError> {
    if !renegotiable(&first.video, &header.video) {
        return Err(ExportError::Failed(format!(
            "{name} was recorded differently from the first video (its H.264 \
             parameters differ); {RE_ENCODE}"
        )));
    }
    if first.audio.is_some() != header.audio.is_some() {
        return Err(ExportError::Failed(format!(
            "{name} has sound the first video hasn't, or the other way about; {RE_ENCODE}"
        )));
    }
    if !renegotiable(&first.audio, &header.audio) {
        return Err(ExportError::Failed(format!(
            "{name}'s sound was recorded differently from the first video's; {RE_ENCODE}"
        )));
    }
    Ok(())
}

/// Whether the muxer would take `next`'s caps on a track it configured from
/// `first`'s.
///
/// **This is `mp4mux`'s own rule, not a stricter one of ours**
/// (`gst_qt_mux_can_renegotiate`): a pad renegotiates only to caps its
/// configured ones are a subset of, and it refuses the stream otherwise —
/// which, a source in, is a failure with the file half written. Comparing
/// `codec_data` alone passed pairs the muxer would then stop on: one `stsd`
/// describes the whole track, so everything in the caps, not only the
/// parameter sets, has to be the same on every source.
///
/// These are `qtdemux`'s caps rather than the parser's, which is what the
/// muxer will see; a parser narrows what its demuxer declared, so two files
/// that agree here agree there.
fn renegotiable(first: &Option<gst::Caps>, next: &Option<gst::Caps>) -> bool {
    match (first, next) {
        (Some(first), Some(next)) => first.is_subset(next),
        (None, None) => true,
        _ => false,
    }
}

/// The sample rate of the output's audio track, or `None` when no source has
/// sound (spec E4). Read from the first file, which every other agreed with.
///
/// **Sound whose rate can't be read is a refusal, not silence.** The rate is
/// the audio pad's `trak-timescale`, so without it there is no audio track to
/// request — and the copy would then drop every audio packet and hand the
/// coach a silent film with nothing said about it.
fn audio_rate(first: Option<&Header>) -> Result<Option<u32>, ExportError> {
    let Some(caps) = first.and_then(|header| header.audio.as_ref()) else {
        return Ok(None);
    };
    structure(caps)
        .get::<i32>("rate")
        .ok()
        .and_then(|rate| u32::try_from(rate).ok())
        .map(Some)
        .ok_or_else(|| {
            ExportError::Failed(format!(
                "the first video's sound doesn't say its sample rate, so it \
                 can't be copied; {RE_ENCODE}"
            ))
        })
}

/// The source being copied: `filesrc ! qtdemux` with a branch per stream,
/// running from the moment it is built.
struct Source {
    pipeline: Stopper,
    /// Set when every sink of this pipeline has seen EOS. The source is built
    /// with a branch per stream it has and no others, so that is the whole
    /// file copied.
    eos: Arc<AtomicBool>,
    /// Cleared when this source is done with, so a push of its that is
    /// waiting for room returns.
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
    ///
    /// `with_audio` is the gate's answer for every source alike: with no sound
    /// to carry there is no audio branch, and so no sink that can never
    /// preroll standing between this pipeline and its EOS.
    fn play(
        file: &Path,
        copying: &Arc<Copying>,
        with_audio: bool,
        watch: &Watch,
    ) -> Result<Source, ExportError> {
        let pipeline = gst::Pipeline::new();
        let source = make("filesrc")?;
        source.set_property("location", file);
        let demux = make("qtdemux")?;
        add_many(&pipeline, &[&source, &demux])?;
        link(&source, &demux)?;
        let live = Arc::new(AtomicBool::new(true));
        let video = branch(&pipeline, "h264parse", Track::Video, copying, &live)?;
        let audio = with_audio
            .then(|| branch(&pipeline, "aacparse", Track::Audio, copying, &live))
            .transpose()?;

        // The branches are built before the pads, because a `pad-added`
        // handler runs on the demuxer's own thread and has no way to build one
        // there. What it *can* do is say why a stream never arrived: without
        // that, a pad with no caps or a link that failed left the muxer
        // waiting for a stream nothing would ever push.
        let failed = watch.error.clone();
        let name = file_name(file);
        demux.connect_pad_added(move |_, pad| {
            let Some(media) = media_type(pad) else {
                return record(
                    &failed,
                    format!("{name} has a stream that declared nothing, so it can't be copied"),
                );
            };
            // A stream with no track of its own — a timecode, a subtitle — is
            // simply not carried. The gate has agreed the two that are.
            let into = if media.starts_with("video/") {
                Some(&video)
            } else if media.starts_with("audio/") {
                audio.as_ref()
            } else {
                None
            };
            let Some(into) = into else { return };
            let sink = into.static_pad("sink").expect("a parser has a sink pad");
            if pad.link(&sink).is_err() {
                record(
                    &failed,
                    format!("{name}'s {media} stream could not be copied"),
                );
            }
        });
        let eos = watch_bus(&pipeline, watch, |_| {});

        let opened = Source {
            pipeline: Stopper(pipeline),
            eos,
            live,
        };
        if opened.pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure(format!("could not read {}", file_name(file))));
        }
        Ok(opened)
    }

    /// Every stream this source carries has reached EOS, so all of its
    /// packets are in the muxer's hands.
    fn done(&self) -> bool {
        self.eos.load(Ordering::SeqCst)
    }
}

/// One stream's `<parser> ! appsink`, returning what the demuxer's pad links
/// into.
///
/// **There is no `queue`**, and nothing downstream of the sink blocks except
/// the one wait this module states, so the demuxer's own thread carries each
/// packet all the way into the muxer, in the order its file has them.
fn branch(
    pipeline: &gst::Pipeline,
    parser: &str,
    track: Track,
    copying: &Arc<Copying>,
    live: &Arc<AtomicBool>,
) -> Result<gst::Element, ExportError> {
    let head = make(parser)?;
    let sink = gst_app::AppSink::builder()
        // Nothing here waits on a clock: the copy runs as fast as the disk.
        // And `async=false` is what lets it reach `PLAYING` at all — see this
        // module's header.
        .sync(false)
        .async_(false)
        .build();
    add_many(pipeline, &[&head, sink.upcast_ref()])?;
    link(&head, sink.upcast_ref())?;
    let (carrying, living) = (copying.clone(), live.clone());
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                carrying.carry(track, &sample, &living);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(head)
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
            offset: AtomicU64::new(0),
            frames: AtomicUsize::new(0),
            packets: AtomicUsize::new(0),
            total,
            error: watch.error.clone(),
        });
        let eos = watch_bus(&pipeline, watch, |_| {});
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
    ///
    /// **Bounded by [`STALL`]**, so a stream that will never arrive — a pad
    /// that declared nothing, a link that failed — fails the export instead of
    /// waiting on it for ever.
    fn until(
        &self,
        watch: &Watch,
        percent: &mut usize,
        on_message: &mut impl FnMut(ExportMessage),
        done: impl Fn() -> bool,
    ) -> Result<(), ExportError> {
        let mut moving = Moving::of(&self.copying);
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
            moving.check()?;
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
}

/// The deadline on a wait that has no duration to measure itself against: a
/// copy that has carried no packet for [`STALL`] has stopped for good.
struct Moving<'a> {
    copying: &'a Copying,
    packets: usize,
    since: Instant,
}

impl<'a> Moving<'a> {
    fn of(copying: &'a Copying) -> Moving<'a> {
        Moving {
            copying,
            packets: copying.packets.load(Ordering::SeqCst),
            since: Instant::now(),
        }
    }

    fn check(&mut self) -> Result<(), ExportError> {
        let packets = self.copying.packets.load(Ordering::SeqCst);
        if packets != self.packets {
            self.packets = packets;
            self.since = Instant::now();
        } else if self.since.elapsed() >= STALL {
            return Err(ExportError::Failed(format!(
                "the copy stopped after {packets} packets and was abandoned"
            )));
        }
        Ok(())
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
    /// Where the source being copied starts in the output, in nanoseconds:
    /// its plan entry's own `start_frame`. **One base for both tracks**, so a
    /// source's own A/V alignment survives the join (spec L7). Written between
    /// sources, when no packet is in flight.
    offset: AtomicU64,
    /// Output frames pushed so far, read for progress by the copy thread.
    frames: AtomicUsize,
    /// Packets carried on either track: what says the copy is still moving.
    packets: AtomicUsize,
    /// The plan's frame count: what progress is clamped to, and the
    /// denominator everything else in the run divides by.
    total: usize,
    /// The run's error slot, so a failure noticed on a demuxer's thread —
    /// [`Copying::wait_for_room`]'s ceiling — reaches the copy thread.
    error: Arc<Mutex<Option<String>>>,
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
        segment.set_base(gst::ClockTime::from_nseconds(
            self.offset.load(Ordering::SeqCst),
        ));
        if track == Track::Video {
            if let Some(at) = buffer.pts().and_then(|pts| segment.to_running_time(pts)) {
                let frames = (seconds(at) * f64::from(OUTPUT_FPS)).round() as usize;
                // **Never backwards.** With B-frames the packet being written
                // is not the latest one in presentation order, so a plain
                // store would walk the sheet's percentage back a frame or two
                // at every reordering.
                self.frames
                    .fetch_max(frames.min(self.total), Ordering::SeqCst);
            }
        }
        self.packets.fetch_add(1, Ordering::SeqCst);
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

    /// Moves to the source starting at `offset` in the output.
    fn start_source(&self, offset: gst::ClockTime) {
        self.offset.store(offset.nseconds(), Ordering::SeqCst);
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
    /// **So [`AHEAD`] is not the memory bound; [`CEILING`] is.** While one of
    /// a source's tracks has run out and the other has not, the muxer is
    /// waiting on the empty pad and writes nothing, and the track still being
    /// read grows past `AHEAD` unchecked: the true bound is `AHEAD` plus the
    /// bytes of that source's own A/V divergence, which is milliseconds on
    /// camera footage and a whole tail on a file whose sound stops early.
    /// **Waiting for it instead would deadlock** — the pad the muxer wants is
    /// fed only by the *next* source, which is opened only once this one has
    /// reached EOS, which needs this very push to return — so past `CEILING`
    /// the copy fails rather than buffering on.
    ///
    /// `live` ends the wait whatever the muxer is doing, so a cancelled or
    /// failed copy leaves no thread in here for the teardown to wait on.
    fn wait_for_room(&self, live: &AtomicBool) {
        if let Some(queued) = self.over_ceiling() {
            return record(
                &self.error,
                format!(
                    "the copy had to hold {} MB of one track before the other \
                     caught up, which is more than a joinable file needs",
                    queued >> 20
                ),
            );
        }
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

    /// The bytes a track is holding, once that is past [`CEILING`].
    fn over_ceiling(&self) -> Option<u64> {
        let queued = self.video.current_level_bytes().max(
            self.audio
                .as_ref()
                .map_or(0, gst_app::AppSrc::current_level_bytes),
        );
        (queued > CEILING).then_some(queued)
    }
}

/// Records `why` unless something is already there: the first failure is the
/// one worth reporting, and the ones after it are usually its consequences.
fn record(slot: &Mutex<Option<String>>, why: String) {
    slot.lock()
        .expect("the error slot isn't poisoned")
        .get_or_insert(why);
}

/// `caps`' first structure, which negotiated caps always have.
fn structure(caps: &gst::Caps) -> &gst::StructureRef {
    caps.structure(0).expect("negotiated caps have a structure")
}

/// `pad`'s media type, as `qtdemux` negotiated it.
fn media_type(pad: &gst::Pad) -> Option<String> {
    let caps = pad.current_caps()?;
    Some(caps.structure(0)?.name().to_string())
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
