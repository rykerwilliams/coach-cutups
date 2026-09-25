//! The export tail (spec X2–X4, E2): a compilation read back as NV12, encoded
//! to H.264 and muxed into an MP4, driven by the pump on a thread of its own.
//!
//! ```text
//! pad 0: the pumped source frame through `gltransformation` (zoom), at that
//!        entry's fit rect
//! pad 1: the entry's inset -- its webcam recording, or the project's avatar,
//!        at the PiP rect (the avatar's scaled by the pulse)
//! pad 2: the overlay -- drawings, the text bar and the scoreboard -- at the
//!        output size
//! ```
//!
//! Which of the three lands on top, and why, is
//! [`install_overlay_pad`](super::install_overlay_pad)'s.
//!
//! The sound goes into the same muxer, mixed per output frame by
//! [`Mixer`](super::audio::Mixer) and pushed **at or ahead of** the frames it
//! covers.
//!
//! **Every pad gets a buffer for every frame.** A requested pad that never
//! receives one produced no output at all and backed the base `appsrc` up,
//! with no error (measured), so an entry with the PiP off, or with a recording
//! that can't be read, pushes a 1×1 transparent pixel instead — in GL memory,
//! like the recording's own frames (see [`Texture`]). A cut whose pieces come
//! from several matches alternates fed and filler pads as the ordinary case,
//! so the GL memory is not a nicety: a system-memory filler breaks the
//! `glupload` of the next entry that has a real inset.
//!
//! **Caps may change from the pushing thread; geometry may not.** Every
//! `appsrc` takes a mid-stream caps change and it lands on exactly the right
//! frame, but a pad rect set the same way applied up to [`QUEUED`] frames
//! early, so the last frames of an entry took the next entry's layout
//! (measured). The rects live in the [`Schedule`] instead, keyed to each
//! buffer's PTS in a pad probe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use video_coach_core::audio::{Region, AUDIO_SAMPLE_RATE};
use video_coach_core::chapters::chapter_list;
use video_coach_core::cues::{cues_to_srt, Cue};
use video_coach_core::export::{Compilation, OUTPUT_FPS};
use video_coach_core::highlight::{highlight_shapes, PlayerHighlight};
use video_coach_core::layout::pip_rect;
use video_coach_core::metadata::FileTags;
use video_coach_core::project::{Clip, Quality, Resolution};
use video_coach_core::scoreboard::ScoreboardContext;

use super::audio::Mixer;
use super::decode::Decoder;
use super::{
    audio, avatar, fit_rect, frame_time, head, install_geometry, install_overlay_pad, install_zoom,
    overlay_branch, premultiplied_over, push_buffer, rounded, stamp, stamp_buffer, tags,
    CompositeError, Gl, Layout, PadRect, Schedule, Stopper, Watch, POLL, QUEUED,
};
use crate::chapters::{self, ChapterOutcome};
use crate::overlay::{OverlayFrame, OverlayRenderer};
use crate::player::{gl_caps, seconds, seconds_to_clock, Diagnostics};

/// Where the PiP's filler lands: one transparent pixel, so the rect is only
/// something for `glvideomixer` to scale nothing into.
const FILLER_RECT: PadRect = (0, 0, 1, 1);

/// Why an export produced no file. The composite's error under export's name,
/// which the bus and the UI have always used.
pub type ExportError = CompositeError;

/// **Which renderer writes the file, not which mode the app is in.** The bus
/// decides from the target and the scoreboard picker (spec M3, X1); media only
/// branches on it, once, at the top of [`run`].
///
/// Everything only one of them reads travels inside it, so a job can't carry a
/// resolution for a run that copies or a cue list drawn by an encoder.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "one of these is built per export target and moved once, onto the \
              export thread; boxing it would buy nothing and cost every caller \
              a `Box::new`"
)]
pub enum Render {
    /// The composite graph: every frame decoded, drawn on and encoded.
    Encode(Encode),
    /// The stream copy ([`copy`](super::copy)): the sources' own packets
    /// joined, which only the whole match in track mode may ask for. Carries
    /// the files to join, one per plan entry and in that order.
    Copy(Vec<PathBuf>),
}

/// What only the encoded export reads: the pixels it composites, the sound it
/// mixes, and what it writes them as.
#[derive(Debug, Clone)]
pub struct Encode {
    /// One per `compilation.plan.entries`, in the same order. **Not an
    /// `Option`**: every entry has a game video and a match behind it, even
    /// when it has no clip.
    pub entries: Vec<EntryMedia>,
    /// The audio edit over the same compilation, from
    /// `video_coach_core::audio::audio_regions`: which span of which file is
    /// heard at each emitted sample, and how loud. Empty is a silent track,
    /// which is still a track — the muxer needs one either way.
    pub audio: Vec<Region>,
    pub resolution: Resolution,
    pub quality: Quality,
}

/// What to export: one compilation, the files its entries read, and the
/// renderer that writes it.
#[derive(Debug, Clone)]
pub struct ExportJob {
    /// Every output frame and the plan they came from
    /// (`video_coach_core::export::compilation_schedule`).
    pub compilation: Compilation,
    /// The output file. Written as `<path>.part` and renamed on success, so a
    /// failed export never touches a file already there.
    pub path: PathBuf,
    /// The scoreboard beside the file, as lines of text against output time
    /// (spec T1): `Some(cues)` writes them, `Some(empty)` writes none **and
    /// clears a stale one**, and `None` is a target that never carries a
    /// sidecar and so leaves the path alone (see [`write_sidecar`]).
    pub cues: Option<Vec<Cue>>,
    /// Which renderer writes it, and everything only that one reads.
    pub render: Render,
    /// What the file says about itself in its header
    /// (`video_coach_core::metadata::file_tags`), written by whichever
    /// renderer runs. An empty field writes no tag, so
    /// [`FileTags::default`](video_coach_core::metadata::FileTags::default) is
    /// an untagged file.
    pub tags: FileTags,
}

/// What one entry needs beside its `PlanEntry`, which carries the edit but
/// neither the files nor the drawings.
#[derive(Debug, Clone)]
pub struct EntryMedia {
    /// The game video this entry's frames come from — **the file, not an
    /// index**: `PlanEntry::source_index` stays the project-local index the
    /// board and the highlights are keyed by, and nothing here has to map it.
    pub source: PathBuf,
    /// `None` exactly for an entry with no clip (`PlanEntry::clip_id`): the
    /// PiP filler, no drawings, no commentary.
    pub clip: Option<ClipMedia>,
    /// The match this entry's footage belongs to. One value per contributing
    /// project, **shared by `Arc`** between that project's entries: a
    /// twenty-piece cut from three matches holds three `ScoreboardContext`s,
    /// not twenty.
    pub match_media: Arc<MatchMedia>,
}

/// What one entry's clip carries: the commentary, and the drawings over it.
#[derive(Debug, Clone)]
pub struct ClipMedia {
    /// The commentary recording, under its project's `recordings/`: the
    /// picture-in-picture's video.
    pub recording: PathBuf,
    /// The clip, for the drawings the overlay replays.
    pub clip: Clip,
}

/// Everything an entry needs from the match it came from rather than from its
/// clip: which is to say, everything that is a property of a project.
///
/// Built once per contributing project when the run starts, and — like every
/// [`ScoreboardContext`] — **never reused across a source add, move, remove or
/// relink**, which a job being a snapshot already guarantees.
#[derive(Debug, Clone, Default)]
pub struct MatchMedia {
    /// The match clock and score to burn in, or `None` when the project has no
    /// scoreboard configured, or carries it beside the file instead.
    pub scoreboard: Option<ScoreboardContext>,
    /// The project's player highlights, a snapshot taken when the run starts.
    /// They belong to the footage rather than to a clip (spec H1), so an entry
    /// with no clip — a reel piece — gets them too.
    pub highlights: Vec<PlayerHighlight>,
    /// The project's avatar image, or `None` for a project that records on
    /// camera. One image for the project, so one path here: every entry of it
    /// whose clip `shows_avatar` draws this one (spec A1, I6).
    pub avatar: Option<PathBuf>,
}

/// What a running export reports, on its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportMessage {
    /// Output frames pushed so far, sent each time the whole percent of them
    /// changes.
    ///
    /// **Frames, not the percent** (spec E5): the run's estimate divides
    /// remaining frames by a rate, and a percent of one target can't be added
    /// across the targets of a compilation run. The percent is only the
    /// throttle, so a long export doesn't send a message per frame.
    Progress(usize),
    /// Sent exactly once, last.
    Finished(Result<ExportDone, ExportError>),
}

/// A finished export.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportDone {
    pub path: PathBuf,
    /// Factory name of the H.264 encoder, e.g. `vah264lpenc`.
    pub encoder: String,
    /// The decode side's path for the first entry's source, as the player logs
    /// it. Every source runs the same graph, so one of them says whether the
    /// run was zero-copy.
    pub diagnostics: Diagnostics,
    /// Whether the file got its chapters, one per entry, or why not. A skip
    /// never fails the export.
    pub chapters: ChapterOutcome,
    /// The scoreboard sidecar written beside the file, or `None` — the job
    /// carried no cues, or the write failed, which is reported and never
    /// fatal.
    pub sidecar: Option<PathBuf>,
    /// The pasteable chapter list written beside the file, or `None` — the
    /// plan's chapters make no list YouTube would read (a stale one is then
    /// removed), or the write failed, which is reported and never fatal.
    pub chapter_list: Option<PathBuf>,
    /// What was left of the `moov` reserve at EOS, for the log line: see
    /// [`reserve_remaining`].
    pub reserve_remaining: f64,
}

/// A running export. It owns its thread, and every GStreamer object it
/// creates lives and dies on that thread. Dropping it cancels and joins.
pub struct Exporter {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Exporter {
    /// Starts exporting `job`. `on_message` is called on the export thread:
    /// [`ExportMessage::Progress`] as frames go out, then exactly one
    /// [`ExportMessage::Finished`]. `job` must have frames: the bus refuses
    /// an empty target.
    pub fn start(
        job: ExportJob,
        on_message: impl FnMut(ExportMessage) + Send + 'static,
    ) -> Exporter {
        Self::spawn(job, on_message, None)
    }

    /// [`Exporter::start`], with `inject` (a launch-string element) spliced in
    /// before the encoder. Tests use it to fail the graph mid-stream.
    fn spawn(
        job: ExportJob,
        mut on_message: impl FnMut(ExportMessage) + Send + 'static,
        inject: Option<&'static str>,
    ) -> Exporter {
        debug_assert!(!job.compilation.frames.is_empty(), "an export needs frames");
        let entries = job.compilation.plan.entries.len();
        match &job.render {
            Render::Encode(encode) => debug_assert_eq!(
                encode.entries.len(),
                entries,
                "every plan entry needs its files"
            ),
            Render::Copy(files) => debug_assert_eq!(
                files.len(),
                entries,
                "every plan entry needs the file it is copied from"
            ),
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("export".into())
            .spawn({
                let cancel = cancel.clone();
                move || {
                    let result = run(&job, &cancel, inject, &mut on_message);
                    on_message(ExportMessage::Finished(result));
                }
            })
            .expect("spawn the export thread");
        Exporter {
            cancel,
            thread: Some(thread),
        }
    }

    /// Asks the export to stop. It notices within about 10 ms, deletes its
    /// `.part` and finishes with [`ExportError::Cancelled`] — unless it had
    /// already finished, in which case its own result stands.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Exporter {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The output frame size for `resolution` (spec E4). 2160p stays in the
/// project format but the sheet doesn't offer it: it runs at 0.56× realtime
/// and only upscales the user's 1440p footage.
fn output_size(resolution: Resolution) -> (i32, i32) {
    match resolution {
        Resolution::R720 => (1280, 720),
        Resolution::R1080 => (1920, 1080),
        Resolution::R2160 => (3840, 2160),
    }
}

/// The constant quantizer each encoder gets for `quality` (spec E4): the VA
/// encoder's `qpi`/`qpp`, and `x264enc`'s constant-quality level.
///
/// **Quality is a quantizer, because nothing else is on offer.** On the
/// reference driver `vah264lpenc`'s `rate-control` enum has exactly one
/// member, `cqp` — `rate-control=cbr` and `=vbr` fail to even parse — so there
/// is no bitrate to target and no VBV to cap the peaks. `bitrate`,
/// `target-usage` and `b-frames` are all present as properties and all
/// measurably no-ops (`b-frames=2` and `target-usage=1` reproduce the CQP
/// stream byte for byte); `trellis=true` makes the file **twice** as big.
///
/// **The ladder is measured on real match footage** — 1080p30 Trace video of a
/// whole pitch, a quiet 20 s and a busy 24 s of the same half, both through
/// this graph, with SSIM against a near-lossless encode of the same composited
/// frames. Per level, quiet passage → busy passage:
///
/// | level  | VA QP | Mbit/s      | SSIM   | 56-min match |
/// |--------|-------|-------------|--------|--------------|
/// | Low    | 30    | 3.3 – 5.0   | 0.965  | ~1.8 GB      |
/// | Medium | 26    | 5.5 – 7.7   | 0.982  | ~2.7 GB      |
/// | High   | 22    | 9.0 – 12.6  | 0.990  | ~4.5 GB      |
///
/// The old ladder was 28/24/20, and QP 20 put a 56-minute match at 19 Mbit/s —
/// nearly 4× its own ~5 Mbit/s source — for +0.004 SSIM over QP 22. Every
/// level moved two steps up; the file a coach shares halved and nothing on the
/// scoreboard or the ball is softer to look at.
///
/// **`x264enc` is four steps lower for the same picture.** Constant quality on
/// `veryfast` matches the VA encoder's SSIM within 0.001 at QP − 4 (0.9898 vs
/// 0.9897, 0.9825 vs 0.9815, 0.9686 vs 0.9647) — the low-power VA path just
/// spends more bits for it. One number for both would make the software
/// fallback quietly worse than the hardware it stands in for.
fn quantizers(quality: Quality) -> (u32, u32) {
    match quality {
        Quality::Low => (30, 26),
        Quality::Medium => (26, 22),
        Quality::High => (22, 18),
    }
}

/// The `moov` reserve for an output of `frames` output frames, in
/// nanoseconds: the whole file plus a tenth plus a second.
///
/// **Both renderers reserve it, by this one formula.** `mp4mux` writes the
/// `moov` first, in space reserved up front, with no temp file — `faststart`
/// writes the whole `mdat` to `$TMPDIR`, which a crash leaks — and that layout
/// is what [`chapters::splice`] needs. The reserve must cover the whole file,
/// so it gets a margin.
pub(super) fn reserved_duration(frames: usize) -> u64 {
    let duration = frame_time(frames as u64);
    (duration + duration / 10 + gst::ClockTime::SECOND).nseconds()
}

/// `<path>.part`: where the output is written until it is complete.
fn part_path(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// What only the renderer that wrote the file can say. Everything else an
/// [`ExportDone`] carries is the same whichever one ran, and [`finish`] adds
/// it.
pub(super) struct Rendered {
    pub(super) encoder: String,
    pub(super) diagnostics: Diagnostics,
    pub(super) reserve_remaining: f64,
}

/// What `mux` has left of the `moov` space [`reserved_duration`] asked for, in
/// seconds of its own accounting (spec L4, E7); `0.0` while it has accounted
/// for none of it.
///
/// **Read on both paths**, because both reserve by the same formula and both
/// need the room for the same chapter splice: the standing check that the
/// margin is still ample on a longer match is a number in the log, not a
/// multi-gigabyte test.
pub(super) fn reserve_remaining(mux: &gst::Element) -> f64 {
    gst::ClockTime::try_from(mux.property::<u64>("reserved-duration-remaining"))
        .map(seconds)
        .unwrap_or(0.0)
}

/// Writes the `.part` file with the renderer `job` asks for, chapters it and
/// renames it into place, or deletes it.
///
/// **The `.part` contract has one implementation.** The copy and the encode
/// differ only in how the file's bytes are made; the temporary name, the
/// chapters, the rename and the delete-on-failure are here, once (spec X1).
fn run(
    job: &ExportJob,
    cancel: &AtomicBool,
    inject: Option<&str>,
    on_message: &mut impl FnMut(ExportMessage),
) -> Result<ExportDone, ExportError> {
    let part = part_path(&job.path);
    // The pipelines are NULL by the time either renderer returns, so nothing
    // holds the file open.
    let result = match &job.render {
        Render::Encode(encode) => export(job, encode, &part, cancel, inject, on_message),
        Render::Copy(files) => super::copy::copy(job, files, &part, cancel, on_message),
    }
    .and_then(|rendered| finish(job, &part, rendered));
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// The chapters, the rename, then the sidecar: what every renderer owes once
/// its file is written.
fn finish(job: &ExportJob, part: &Path, rendered: Rendered) -> Result<ExportDone, ExportError> {
    // A skip keeps the file whole and is only reported. An I/O error may
    // leave a half-written `moov`, which is a corrupt file: it fails.
    let titles: Vec<(f64, &str)> = job
        .compilation
        .plan
        .chapters
        .iter()
        .map(|(at, title)| (*at, title.as_str()))
        .collect();
    let chapters = chapters::splice(part, &titles)
        .map_err(|e| ExportError::Failed(format!("could not write the chapters: {e}")))?;
    std::fs::rename(part, &job.path)
        .map_err(|e| ExportError::Failed(format!("could not move the export into place: {e}")))?;
    Ok(ExportDone {
        path: job.path.clone(),
        encoder: rendered.encoder,
        diagnostics: rendered.diagnostics,
        chapters,
        sidecar: write_sidecar(job),
        chapter_list: write_chapter_list(job),
        reserve_remaining: rendered.reserve_remaining,
    })
}

/// The scoreboard beside the finished file: `job.cues` as SRT at
/// `<output>.srt` (spec T6).
///
/// **Writing and removing are one step**, and only for a target that carries a
/// sidecar at all: the file that belongs beside *this* output is this string,
/// or nothing. Otherwise a coach exports with a scoreboard, deletes it,
/// exports again, and their player plays the old score over the new film. A
/// target that never writes one leaves the path alone, because a `.srt` beside
/// a clip is the coach's own file and no export's business.
///
/// **A failure is reported, never fatal.** It runs only after the rename, so a
/// cancelled or failed run neither writes nor removes anything.
fn write_sidecar(job: &ExportJob) -> Option<PathBuf> {
    let cues = job.cues.as_ref()?;
    let path = job.path.with_extension("srt");
    if cues.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => eprintln!(
                "export: could not remove the old scoreboard {}: {e}",
                path.display()
            ),
            _ => {}
        }
        return None;
    }
    match std::fs::write(&path, cues_to_srt(cues)) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!(
                "export: could not write the scoreboard {}: {e}",
                path.display()
            );
            None
        }
    }
}

/// The plan's chapters beside the finished file, as the block of text a
/// YouTube description takes: `<output>.chapters.txt`, from
/// [`chapter_list`].
///
/// **A YouTube upload can't read the chapters inside the file** — `chpl` is
/// for `ffprobe`, mpv and VLC — so the same list goes beside it as something
/// to paste. `.chapters.txt` rather than a second use of the output's own
/// extension slot: it says what it is, it sorts next to its video, and it
/// can't be mistaken for (or collide with) the `.srt`. Like the `.srt` it is
/// built from `job.path`, so it inherits the run's name cleaning and its
/// `" (2)"` de-duplication.
///
/// **Every target that has chapters gets one**, not only the whole match: a
/// compilation's chapters are its clips and a reel's are its goals, which are
/// just as pasteable. There is no `None` case as [`ExportJob::cues`] has,
/// because `.chapters.txt` is a name of ours: whatever is at that path beside
/// an export of ours is an export of ours, so a run that has no list to write
/// **removes** the stale one rather than leaving a chapter list that no
/// longer describes the film. That is also what a plan whose chapters break
/// YouTube's rules does ([`chapter_list`] returns `None`).
///
/// **A failure is reported, never fatal**, and like the `.srt` it runs only
/// after the rename, so a cancelled or failed run neither writes nor removes
/// anything.
fn write_chapter_list(job: &ExportJob) -> Option<PathBuf> {
    let path = job.path.with_extension("chapters.txt");
    let Some(text) = chapter_list(&job.compilation.plan.chapters) else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => eprintln!(
                "export: could not remove the old chapter list {}: {e}",
                path.display()
            ),
            _ => {}
        }
        return None;
    };
    match std::fs::write(&path, text) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!(
                "export: could not write the chapter list {}: {e}",
                path.display()
            );
            None
        }
    }
}

fn export(
    job: &ExportJob,
    encode: &Encode,
    part: &Path,
    cancel: &AtomicBool,
    inject: Option<&str>,
    on_message: &mut impl FnMut(ExportMessage),
) -> Result<Rendered, ExportError> {
    let gl = Gl::shared()?;
    let watch = Watch {
        cancel,
        error: Arc::default(),
    };
    let (out_w, out_h) = output_size(encode.resolution);
    let plan = &job.compilation.plan;
    // The avatars and their pulse, before anything is pushed: one decode and
    // one upload per *distinct image* some entry asks for, and one pass over
    // each avatar entry's commentary (spec D5, E2). Both belong here, beside
    // the audio edit's regions, and neither belongs beside `Pip::open`, which
    // runs inside the loop. And both only where an entry asks for one: an
    // avatar project exporting a compilation of camera clips decodes nothing
    // and reports nothing.
    //
    // **Keyed on the path**, as the decoders below are: two matches sharing
    // one image — this coach's own case — hold one texture and one GL upload.
    // An image that will not open is remembered as `None`, so it is reported
    // once for the run rather than once per entry that wanted it.
    let mut avatars: HashMap<PathBuf, Option<AvatarInset>> = HashMap::new();
    for path in encode.entries.iter().filter_map(wanted_avatar) {
        avatars
            .entry(path.clone())
            .or_insert_with(|| AvatarInset::open(path, &gl, &watch, (out_w, out_h)));
    }
    let schedule = Schedule::new(
        job.compilation.frames.clone(),
        pulse_levels(job, encode, &avatars, cancel),
        plan.entries.len(),
    );

    // One decoder per distinct source **file**, so a compilation that walks
    // one match video over and over opens it once: reopening it per entry
    // would cost a preroll each time.
    //
    // **And closed as soon as no entry from the current one on reads it**, the
    // rule the audio mixer already applies to its readers, for the same reason
    // (`composite::audio::Mixer::block`): "Kept open, a compilation of two
    // hundred clips would hold two hundred pipelines at once, where the
    // picture holds one recording at a time." A cut whose pieces come from
    // thirty matches would otherwise hold thirty decode pipelines, their
    // threads and their fds at once. A single-match compilation keeps its one
    // decoder from the first entry to the last, exactly as it always did.
    let mut sources: HashMap<PathBuf, Decoder> = HashMap::new();
    // **The first entry's decoder, read as it is opened**, not after the loop:
    // the first file the run opens is the first entry's, and by the end it may
    // be closed, which would leave the zero-copy line empty.
    let mut diagnostics: Option<Diagnostics> = None;
    let mut mixer = Mixer::new(encode);
    let mut overlays = OverlayRenderer::new();
    // Before any decoding: the encode side depends on nothing the pump
    // produces, so a missing encoder is reported in the moment the run starts
    // rather than after the first source has been opened and seeked.
    let encoder = Encoder::start(part, &schedule, job, encode, &gl, inject, &watch)?;
    // The entry the layout and the caps are currently for, and its PiP, which
    // is opened and closed with it.
    let mut laid_out: Option<usize> = None;
    let mut pip = Pip::filler();
    // The current entry's picture rect, which the overlay maps strokes into.
    let mut picture = (0, 0, out_w, out_h);

    let total = job.compilation.frames.len();
    let mut percent = 0;
    for (n, frame) in job.compilation.frames.iter().enumerate() {
        let entry = &plan.entries[frame.entry];
        let media = &encode.entries[frame.entry];
        if laid_out != Some(frame.entry) {
            close_unread(&mut sources, &encode.entries, frame.entry);
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = sources.entry(media.source.clone())
        {
            let decoder = slot.insert(Decoder::start(&media.source, &gl, &watch)?);
            diagnostics.get_or_insert_with(|| decoder.diagnostics());
        }
        let decoder = sources
            .get_mut(&media.source)
            .expect("opened just above, and not closed until a later entry");
        let sample = decoder.frame_at(seconds_to_clock(frame.source_time), &watch)?;
        if laid_out != Some(frame.entry) {
            let caps = source_caps(sample)?;
            let info = gst_video::VideoInfo::from_caps(&caps)
                .map_err(|e| ExportError::Failed(format!("unusable decoded caps {caps}: {e}")))?;
            picture = fit_rect(&info, out_w, out_h);
            let avatar = media
                .match_media
                .avatar
                .as_ref()
                .and_then(|path| avatars.get(path)?.as_ref());
            pip = Pip::open(media, avatar, &gl, cancel, (out_w, out_h));
            // Before the push, so the pad probes find it (see `Schedule`).
            schedule.set_layout(
                frame.entry,
                Layout {
                    picture,
                    pip: pip.rect,
                },
            );
            set_caps(&encoder.src, &caps);
            laid_out = Some(frame.entry);
        }

        let record_time = entry.record_time(n);
        // **The displayed frame's source time**, not a per-clip constant plus
        // the record time: that sum is exactly the macOS bug that put the
        // match clock ahead of the footage after every pause (BACKLOG #27).
        //
        // **The entry's own match's board**, reached through the entry rather
        // than through an index into a per-job list, so a piece from another
        // match cannot draw this one's score (spec J3).
        let scoreboard = media.match_media.scoreboard.as_ref().and_then(|context| {
            let state = context.state_at(entry.source_index, frame.source_time)?;
            Some((context.config(), state))
        });
        // A highlight lives in the footage, so it is keyed by the displayed
        // frame's source time too, and mapped through that frame's own zoom.
        // Core owns the geometry; the overlay only draws what comes back.
        let highlights = highlight_shapes(
            &media.match_media.highlights,
            entry.source_index,
            frame.source_time,
            frame.zoom,
            f64::from(picture.2),
            f64::from(picture.3),
        );
        let overlay = overlays.render(
            &OverlayFrame {
                clip: media.clip.as_ref().map(|c| &c.clip),
                record_time,
                picture,
                highlights: &highlights,
                text: &entry.text,
                scoreboard,
            },
            out_w as u32,
            out_h as u32,
        );
        encoder.push(
            Frame {
                n: n as u64,
                sample,
                record_time,
                overlay,
            },
            &mut pip,
            &mut mixer,
            &watch,
        )?;

        let now = (n + 1) * 100 / total;
        if now != percent {
            percent = now;
            on_message(ExportMessage::Progress(n + 1));
        }
    }
    encoder.finish(&watch)?;
    let name = encoder.name();
    let reserve = reserve_remaining(&encoder.mux);
    // To NULL, so the muxer's file is closed before `run` chapters it.
    drop(encoder);
    Ok(Rendered {
        encoder: name.to_owned(),
        diagnostics: diagnostics.unwrap_or_default(),
        reserve_remaining: reserve,
    })
}

/// Closes every open source neither the entry before `from` nor any entry from
/// `from` on reads.
///
/// The rule — and the reason for it — is the audio mixer's, applied to the
/// picture: `Mixer::block` closes a reader "nothing later reads", because
/// "kept open, a compilation of two hundred clips would hold two hundred
/// pipelines at once". Here the scan is over the entries still to come rather
/// than over the regions still to arrive, which is the same shape as the
/// mixer's `active.chain(order[next..])`.
///
/// **The window keeps the previous entry**, which is why it starts one back.
/// This is called as the run crosses into `from`, and at that moment up to
/// [`QUEUED`] of the previous entry's frames are still in flight downstream
/// (the base `appsrc` is `max-buffers=QUEUED`): taking their decoder to NULL
/// would free the DMABuf pool those buffers were allocated from. CI's llvmpipe
/// does not import DMABufs and would never show it. One extra decoder, so at
/// most two are open.
///
/// **Nothing is ever reopened**, which is what makes this free: a file is only
/// closed once no entry that could ask for it is left.
fn close_unread<T>(open: &mut HashMap<PathBuf, T>, entries: &[EntryMedia], from: usize) {
    let live = &entries[from.saturating_sub(1)..];
    open.retain(|path, _| live.iter().any(|media| media.source == *path));
}

/// The avatar image `media` draws, or `None` — its clip doesn't show one, it
/// has no clip at all, or its match has no image.
///
/// **One expression, two readers**: the pre-pass that opens the textures, and
/// [`pulse_levels`], which takes what that pre-pass managed to open as its real
/// gate. There is no per-job gate beside it — with the image in the condition, a
/// run nobody asks an avatar for opens nothing and pulses nothing by itself.
fn wanted_avatar(media: &EntryMedia) -> Option<&PathBuf> {
    media
        .clip
        .as_ref()
        .filter(|c| c.clip.shows_avatar())
        .and(media.match_media.avatar.as_ref())
}

/// `sample`'s caps with the output frame rate on them, which is what the base
/// `appsrc` is fed at.
fn source_caps(sample: &gst::Sample) -> Result<gst::Caps, ExportError> {
    let caps = sample
        .caps()
        .ok_or_else(|| ExportError::Failed("a decoded frame has no caps".into()))?;
    let mut caps = caps.to_owned();
    caps.make_mut()
        .set("framerate", gst::Fraction::new(OUTPUT_FPS as i32, 1));
    Ok(caps)
}

/// Sets `appsrc`'s caps unless they are already `caps`.
///
/// Caps are safe to set from the pushing thread: the change lands on exactly
/// the frame pushed after it (measured). Geometry is not — see the module
/// comment.
fn set_caps(appsrc: &gst_app::AppSrc, caps: &gst::Caps) {
    if appsrc.caps().as_ref() != Some(caps) {
        appsrc.set_caps(Some(caps));
    }
}

/// One entry's inset: what the pad carries for it, and where that lands.
///
/// The pad is fed every frame whatever happens here, because an unfed pad
/// stalls the whole export (measured). `show_pip` off, an avatar whose image
/// is gone, a recording that isn't there, one with no video, one that stops
/// decoding mid-entry: each of them ends up pushing the 1×1 transparent filler,
/// and the export goes on.
struct Pip {
    source: Source,
    /// The pad's rect at full size, from the recording's **probed** display
    /// aspect or the avatar's square box — never from the pushed caps, whose
    /// 1×1 filler would make the inset square by accident. The pulse scales it
    /// per frame, in the pad's own probe (`Schedule::inset`).
    rect: PadRect,
}

/// What the inset pad carries for an entry.
enum Source {
    /// The entry's webcam recording, decoded frame by frame.
    Camera {
        decoder: Decoder,
        /// The recording's own errors, kept off the export's [`Watch`]: a
        /// recording that gives up costs the inset, not the run. Only this arm
        /// has a file to give up.
        errors: Arc<Mutex<Option<String>>>,
    },
    /// The project's avatar: the one texture the run uploaded, re-stamped
    /// every frame, sized by the pulse in the pad's rect (spec E2, E3).
    Avatar(Texture),
    /// Nothing to show: the 1×1 transparent filler, every frame.
    Empty,
}

impl Pip {
    /// No inset: the pad takes the filler for every frame of the entry.
    fn filler() -> Pip {
        Pip {
            source: Source::Empty,
            rect: FILLER_RECT,
        }
    }

    /// What this entry's inset is, from the two predicates `core` states it in
    /// (spec B3, E5) — **before the probe**, which would call an avatar
    /// recording's missing video track a missing picture-in-picture.
    ///
    /// A webcam recording that will not open falls back to the filler, saying
    /// on stderr why: a missing inset is a smaller loss than a failed export of
    /// an hour of video. An entry with no clip, a clip with `show_pip` off and
    /// an avatar whose image is gone take the filler silently — the image was
    /// reported once per image, by [`AvatarInset::open`].
    ///
    /// `avatar` is this entry's **match's** texture, already looked up by the
    /// caller.
    fn open(
        media: &EntryMedia,
        avatar: Option<&AvatarInset>,
        gl: &Gl,
        cancel: &AtomicBool,
        (out_w, out_h): (i32, i32),
    ) -> Pip {
        let Some(ClipMedia { recording, clip }) = media.clip.as_ref() else {
            return Pip::filler();
        };
        if clip.shows_avatar() {
            return match avatar {
                Some(avatar) => Pip {
                    // A reference to the run's one texture, not a copy of it.
                    source: Source::Avatar(avatar.texture.clone()),
                    rect: avatar.rect,
                },
                None => Pip::filler(),
            };
        }
        if !clip.shows_camera_pip() {
            return Pip::filler();
        }
        let refuse = |why: String| {
            eprintln!(
                "export: no picture-in-picture for {}: {why}",
                recording.display()
            );
            Pip::filler()
        };
        // The probe is also the check that the file is there and has video,
        // before a decoder is built on it.
        let aspect = match crate::probe::probe(recording) {
            Ok(probe) => probe.display_aspect,
            Err(e) => return refuse(e.to_string()),
        };
        let errors: Arc<Mutex<Option<String>>> = Arc::default();
        let watch = Watch {
            cancel,
            error: errors.clone(),
        };
        match Decoder::start(recording, gl, &watch) {
            Ok(decoder) => Pip {
                source: Source::Camera { decoder, errors },
                rect: rounded(pip_rect(f64::from(out_w), f64::from(out_h), aspect)),
            },
            Err(e) => refuse(e.to_string()),
        }
    }

    /// The inset's buffer for output frame `n`, stamped, with the caps it must
    /// be pushed under.
    ///
    /// `record_time` is where the frame sits in the **recording**, which is
    /// the entry's own timeline. Past the recording's end `Decoder::frame_at`
    /// holds its last frame, which is what `repeat-after-eos` would have done
    /// on a pad that could EOS — a pumped one never does.
    fn frame(
        &mut self,
        n: u64,
        record_time: f64,
        cancel: &AtomicBool,
        filler: &Texture,
    ) -> (gst::Buffer, gst::Caps) {
        if let Source::Avatar(texture) = &self.source {
            // The same texture every frame; only the pad's rect moves.
            return texture.stamped(n);
        }
        if let Some(decoded) = self.decode(n, record_time, cancel) {
            return decoded;
        }
        // Whatever went wrong won't get better: the rest of the entry takes
        // the filler rather than retrying the recording once a frame.
        self.source = Source::Empty;
        filler.stamped(n)
    }

    /// The recording's frame at `record_time`, or `None` once there is no
    /// recording or it has given up.
    fn decode(
        &mut self,
        n: u64,
        record_time: f64,
        cancel: &AtomicBool,
    ) -> Option<(gst::Buffer, gst::Caps)> {
        let Source::Camera { decoder, errors } = &mut self.source else {
            return None;
        };
        let watch = Watch {
            cancel,
            error: errors.clone(),
        };
        match decoder.frame_at(seconds_to_clock(record_time), &watch) {
            Ok(sample) => sample
                .caps()
                .map(|caps| (stamp(sample, n), caps.to_owned())),
            // Cancellation is the run ending, and the loop's own `Watch` is
            // about to see it too.
            Err(CompositeError::Cancelled) => None,
            Err(CompositeError::Failed(e)) => {
                eprintln!("export: the picture-in-picture stopped: {e}");
                None
            }
        }
    }
}

/// One still RGBA image **in GL memory**, uploaded once and re-stamped for
/// every frame that shows it: the inset pad's 1×1 transparent filler
/// ([`Texture::filler`]), and the project's avatar ([`AvatarInset`]). The pad
/// scales it to whatever rect it has — a transparent pixel is invisible
/// however big (measured), and the avatar's rect is the pulse.
///
/// **It has to be GL memory, because the pad's caps feature may not change.**
/// The recording decodes to GL, so a system-memory filler made the branch's
/// `glupload` take GL frames and then a system-memory one, which it refuses
/// ("Failed to upload buffer"): any target whose first entry has no inset and
/// whose second has one died there (reproduced). Uploaded here, the pad
/// carries GL memory from the first frame to the last, and only the caps
/// change — which GL to GL takes. A mixed avatar-and-camera compilation is
/// that same case (spec I2).
///
/// **Premultiplied, like the overlay's layer**: the pixels come from a
/// tiny-skia pixmap and the pad is told so (`premultiplied_over`, spec E4).
#[derive(Clone)]
struct Texture {
    buffer: gst::Buffer,
    caps: gst::Caps,
}

impl Texture {
    /// The inset pad's stand-in: one transparent pixel.
    fn filler(gl: &Gl, watch: &Watch) -> Result<Texture, ExportError> {
        Texture::upload(gl, watch, 1, 1, vec![0; 4])
    }

    /// This texture as output frame `n`, with the caps it must be pushed
    /// under: a new buffer header over the **same texture**, never a copy of
    /// the pixels.
    fn stamped(&self, n: u64) -> (gst::Buffer, gst::Caps) {
        let mut buffer = self.buffer.copy();
        stamp_buffer(&mut buffer, n);
        (buffer, self.caps.clone())
    }

    /// Uploads `rgba` (`w`×`h`, tightly packed) on `gl`, through a pipeline of
    /// its own that is gone by the time this returns. The texture outlives it:
    /// the buffer holds it, and `gl`'s context is the process's
    /// ([`Gl::shared`]).
    fn upload(
        gl: &Gl,
        watch: &Watch,
        w: u32,
        h: u32,
        rgba: Vec<u8>,
    ) -> Result<Texture, ExportError> {
        debug_assert_eq!(rgba.len(), (w * h * 4) as usize, "tightly packed RGBA");
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=src format=time is-live=false block=false \
               caps=video/x-raw,format=RGBA,width={w},height={h},framerate={OUTPUT_FPS}/1 \
             ! glupload ! glcolorconvert ! appsink name=out sync=false"
        ))
        .map_err(|e| ExportError::Failed(format!("could not build the upload graph: {e}")))?
        .downcast::<gst::Pipeline>()
        .expect("a multi-element launch string yields a pipeline");
        let by_name = |n: &str| pipeline.by_name(n).expect("named in the launch string");
        let src = by_name("src")
            .downcast::<gst_app::AppSrc>()
            .expect("named as an appsrc in the launch string");
        let sink = by_name("out")
            .downcast::<gst_app::AppSink>()
            .expect("named as an appsink in the launch string");
        sink.set_caps(Some(&gl_caps()));
        gl.install(&pipeline, watch, |_| {});
        let pipeline = Stopper(pipeline);
        if pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure("could not start the upload graph"));
        }

        let mut buffer = gst::Buffer::from_mut_slice(rgba);
        stamp_buffer(&mut buffer, 0);
        src.push_buffer(buffer)
            .map_err(|e| watch.failure(format!("uploading a {w}x{h} still: {e:?}")))?;
        let _ = src.end_of_stream();
        loop {
            watch.check()?;
            if let Some(sample) = sink.try_pull_sample(POLL) {
                let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
                    return Err(ExportError::Failed(
                        "an uploaded still came back bare".into(),
                    ));
                };
                return Ok(Texture {
                    buffer: buffer.copy(),
                    caps: caps.to_owned(),
                });
            }
            if sink.is_eos() {
                return Err(watch.failure(format!("a {w}x{h} still did not upload")));
            }
        }
    }
}

/// The project's avatar, ready for the inset pad: one texture for the whole
/// run, and the rect it fills at its loudest.
struct AvatarInset {
    texture: Texture,
    /// The avatar's box — `avatar_box` of the square `layout::pip_rect` —
    /// which its circle is drawn in (spec A5). The pulse scales it per frame
    /// (`Schedule::inset`).
    rect: PadRect,
}

impl AvatarInset {
    /// Decodes, pre-scales and uploads the project's avatar, or says on stderr
    /// why there is none.
    ///
    /// **Once per distinct image, and a failure costs the inset rather than
    /// the export** (spec A4) — one line, not one per entry. An image that has
    /// gone under the project is exactly as fatal as a picture-in-picture that
    /// will not open, which is to say not at all: the entries that wanted it
    /// take the filler and the file is written.
    fn open(
        path: &Path,
        gl: &Gl,
        watch: &Watch,
        (out_w, out_h): (i32, i32),
    ) -> Option<AvatarInset> {
        let avatar = avatar::open_reported(path, f64::from(out_w), f64::from(out_h))?;
        let (w, h) = (avatar.image.width(), avatar.image.height());
        match Texture::upload(gl, watch, w, h, avatar.image.data().to_vec()) {
            Ok(texture) => Some(AvatarInset {
                texture,
                rect: rounded(avatar.rect),
            }),
            Err(e) => {
                eprintln!("export: the avatar did not upload: {e}");
                None
            }
        }
    }
}

/// One pulse level per output frame of the run: an avatar entry's from its own
/// commentary, and `1.0` everywhere else — which `pulsed` maps to exactly the
/// inset rect, so nothing but an avatar moves (spec E3).
///
/// Only for an entry that asks for an avatar ([`wanted_avatar`]) **and whose
/// image opened**, which is what `avatars` says: every other one keeps its
/// level at 1.0, which is the pad's own rect — and for an entry that ends up on
/// the filler that matters, since scaling a 1×1 rect would round it away. An
/// entry whose image did not open is exactly such an entry ([`Pip::open`]), so
/// gating on the asking alone would decode its whole commentary to fill a table
/// nothing then reads.
fn pulse_levels(
    job: &ExportJob,
    encode: &Encode,
    avatars: &HashMap<PathBuf, Option<AvatarInset>>,
    cancel: &AtomicBool,
) -> Vec<f64> {
    let opened = |media: &EntryMedia| {
        wanted_avatar(media).is_some_and(|path| avatars.get(path).is_some_and(Option::is_some))
    };
    let mut levels = vec![1.0; job.compilation.frames.len()];
    for (entry, media) in job.compilation.plan.entries.iter().zip(&encode.entries) {
        let Some(clip) = media.clip.as_ref().filter(|_| opened(media)) else {
            continue;
        };
        let table = avatar::pulse_table(&clip.recording, entry.frames, cancel);
        for (slot, level) in levels.iter_mut().skip(entry.start_frame).zip(table) {
            *slot = level;
        }
    }
    levels
}

/// The H.264 encoders export can use, in preference order, with their
/// launch-string settings for `quality` ([`quantizers`]). Only encoders
/// someone has run are listed (spec X3).
fn encoders(quality: Quality) -> [(&'static str, String); 2] {
    let (qp, crf) = quantizers(quality);
    [
        (
            "vah264lpenc",
            format!("rate-control=cqp qpi={qp} qpp={qp} key-int-max=60"),
        ),
        // Constant quality: smaller than constant QP at the same quality.
        // `medium` runs at 0.39x realtime; `veryfast` keeps up.
        //
        // **`vbv-buf-capacity=0` is not a detail.** `x264enc` feeds `bitrate`
        // (default 2048 kbit/s) to libx264 as a VBV *maximum* in this mode
        // too, so the quality level was capped at about 1.7 Mbit/s whatever it
        // was set to: constant quality 24, 26 and 28 all came out within 5% of
        // each other and of one another's SSIM (0.890). Zeroing the capacity
        // turns the VBV off, which is what constant quality means.
        (
            "x264enc",
            format!(
                "pass=qual quantizer={crf} speed-preset=veryfast key-int-max=60 \
                 vbv-buf-capacity=0"
            ),
        ),
    ]
}

/// One output frame's own inputs. The [`Pip`], the [`Mixer`] and the
/// [`Watch`] belong to the run rather than the frame, so they stay arguments
/// of their own.
struct Frame<'a> {
    /// The output frame index: its PTS is `n/30`.
    n: u64,
    /// The decoded source frame to show.
    sample: &'a gst::Sample,
    /// Where `n` sits in the entry's recording — the picture-in-picture's
    /// cursor, and the clock the overlay was drawn at.
    record_time: f64,
    /// The drawings and the text bar, rasterized at the output size.
    overlay: gst::Buffer,
}

struct Encoder {
    /// Held to go to NULL with the encoder.
    _pipeline: Stopper,
    /// The muxer, for the `moov` reserve it has left at EOS (spec L4, E7).
    mux: gst::Element,
    /// The pumped source frames: pad 0.
    src: gst_app::AppSrc,
    /// The picture-in-picture: pad 1.
    pip: gst_app::AppSrc,
    /// The drawings and the text bar, at the output size: pad 2.
    overlay: gst_app::AppSrc,
    /// The mixed sound, straight into the muxer's AAC branch.
    audio: gst_app::AppSrc,
    /// What the PiP pad takes whenever there is no inset to show.
    filler: Texture,
    name: &'static str,
    /// Set when the file is complete.
    eos: Arc<AtomicBool>,
}

impl Encoder {
    /// Builds the three-pad graph writing to `part` and sets it PLAYING. The
    /// zoom and the moving pads read `schedule`; its errors reach `watch`.
    /// `inject` is spliced in before the encoder (tests).
    ///
    /// The encoder is the first of [`encoders`] installed. A presence check
    /// only: one that fails at start fails the export (BACKLOG #39).
    fn start(
        part: &Path,
        schedule: &Arc<Schedule>,
        job: &ExportJob,
        encode: &Encode,
        gl: &Gl,
        inject: Option<&str>,
        watch: &Watch,
    ) -> Result<Encoder, ExportError> {
        let (out_w, out_h) = output_size(encode.resolution);
        let (name, settings) = encoders(encode.quality)
            .into_iter()
            .find(|(name, _)| gst::ElementFactory::find(name).is_some())
            .ok_or_else(|| {
                ExportError::Failed(
                    "no H.264 encoder: install gst-plugins-ugly (x264enc) or VA drivers".into(),
                )
            })?;
        // `avenc_aac` is rank none, so it is never auto-plugged and has to be
        // named — which also means a missing gst-libav shows up as a parse
        // failure unless it is checked for by name first.
        if gst::ElementFactory::find("avenc_aac").is_none() {
            return Err(ExportError::Failed(
                "no AAC encoder: install gstreamer1.0-libav (avenc_aac)".into(),
            ));
        }
        let filler = Texture::filler(gl, watch)?;
        let inject = inject.map(|i| format!("{i} ! ")).unwrap_or_default();
        // The readback before the encoder is required, and so is the queue.
        //
        // **The PiP branch has no `glupload`**: everything that reaches it is
        // already GL memory, the recording's frames and the [`Filler`] alike.
        //
        // **The audio appsrc alone is unbounded** (`max-buffers=0`) and the
        // pump never waits for room on it. Bounding it at 0.27 s deadlocked
        // the pump: the encoder keeps the muxer about 0.43 s behind the pushed
        // video, and the right bound depends on the encoder's latency, so
        // there is no number to tune (measured).
        let description = format!(
            "{head} \
             ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 \
             ! gldownload ! video/x-raw,format=NV12 ! queue \
             ! {inject}{name} {settings} \
             ! h264parse ! video/x-h264,profile=high,stream-format=avc,alignment=au \
             ! mp4mux name=mux ! filesink name=out \
             appsrc name=audio format=time is-live=false block=false \
               max-buffers=0 max-bytes=0 max-time=0 caps={audio_caps} \
             ! audioconvert ! avenc_aac bitrate=192000 ! aacparse ! mux. \
             appsrc name=pip format=time is-live=false block=false \
               max-buffers={QUEUED} max-bytes=0 max-time=0 \
             ! glcolorconvert ! mix.sink_1 \
             {overlay}",
            head = head(out_w, out_h),
            overlay = overlay_branch(out_w, out_h),
            audio_caps = audio::caps_description(AUDIO_SAMPLE_RATE, audio::CHANNELS)
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| ExportError::Failed(format!("could not build the export graph: {e}")))?
            .downcast::<gst::Pipeline>()
            .expect("a multi-element launch string yields a pipeline");
        let by_name = |n: &str| pipeline.by_name(n).expect("named in the launch string");
        let appsrc = |name: &str| {
            by_name(name)
                .downcast::<gst_app::AppSrc>()
                .expect("named as an appsrc in the launch string")
        };

        let mix = by_name("mix");
        let mix_pad = |name: &str| {
            mix.static_pad(name)
                .expect("requested in the launch string")
        };
        // The base and the inset move with the entry — and the inset with the
        // pulse besides — so their rects come from the schedule keyed on each
        // buffer's PTS. The overlay is the whole output frame for the whole
        // run.
        install_geometry(&mix_pad("sink_0"), schedule, 0, Schedule::picture);
        let inset_pad = mix_pad("sink_1");
        // Over the picture and under the overlay (`install_overlay_pad`).
        install_geometry(&inset_pad, schedule, 1, Schedule::inset);
        // The avatar reaching this pad is a premultiplied pixmap; a webcam
        // frame and the filler blend the same either way (spec E4).
        premultiplied_over(&inset_pad);
        install_overlay_pad(&mix, out_w, out_h);
        install_zoom(&by_name("zoom"), &schedule.frames);

        let mux = by_name("mux");
        mux.set_property(
            "reserved-max-duration",
            reserved_duration(job.compilation.frames.len()),
        );
        // Before `PLAYING`: the header is laid out at the first buffer, and
        // the reserve is grown to fit the tags rather than spent on them
        // ([`tags`](super::tags)).
        tags::apply(&mux, &job.tags);
        by_name("out").set_property("location", part);

        let eos = gl.install(&pipeline, watch, |_| {});
        let (src, pip, overlay, audio) =
            (appsrc("src"), appsrc("pip"), appsrc("ov"), appsrc("audio"));
        let pipeline = Stopper(pipeline);
        if pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure("could not start the encoder"));
        }
        Ok(Encoder {
            _pipeline: pipeline,
            mux,
            src,
            pip,
            overlay,
            audio,
            filler,
            name,
            eos,
        })
    }

    /// The encoder's factory name.
    fn name(&self) -> &'static str {
        self.name
    }

    /// Pushes output frame `n` onto all three pads, in z-order, with the sound
    /// it covers.
    ///
    /// The base is a buffer reference, not a pixel copy: a freeze (and every
    /// held source frame) sends the same texture out again.
    ///
    /// **The sound goes first, and never waits.** Its block covers exactly
    /// this frame, so it reaches the muxer at or ahead of the picture, which
    /// is the whole ordering rule (spec E3): pushing it behind the video, or
    /// waiting for room on an appsrc the encoder keeps 0.43 s behind, stalls
    /// the pump.
    fn push(
        &self,
        frame: Frame,
        pip: &mut Pip,
        mixer: &mut Mixer,
        watch: &Watch,
    ) -> Result<(), ExportError> {
        let Frame {
            n,
            sample,
            record_time,
            mut overlay,
        } = frame;
        self.audio
            .push_buffer(mixer.block(n, watch.cancel))
            .map_err(|e| watch.failure(format!("pushing the sound of frame {n}: {e:?}")))?;
        let (inset, caps) = pip.frame(n, record_time, watch.cancel, &self.filler);
        set_caps(&self.pip, &caps);
        stamp_buffer(&mut overlay, n);

        push_buffer(&self.src, stamp(sample, n), &format!("frame {n}"), watch)?;
        push_buffer(&self.pip, inset, &format!("the PiP of frame {n}"), watch)?;
        push_buffer(
            &self.overlay,
            overlay,
            &format!("the overlay of frame {n}"),
            watch,
        )
    }

    /// Ends the streams and waits for the muxer to finish the file.
    fn finish(&self, watch: &Watch) -> Result<(), ExportError> {
        for appsrc in [&self.src, &self.pip, &self.overlay, &self.audio] {
            let _ = appsrc.end_of_stream();
        }
        loop {
            // Read before the check: an error is recorded before any EOS.
            let done = self.eos.load(Ordering::SeqCst);
            watch.check()?;
            if done {
                return Ok(());
            }
            std::thread::sleep(POLL.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use video_coach_core::export::FrameSpec;
    use video_coach_core::project::Inset;
    use video_coach_core::zoom::Zoom;

    use uuid::Uuid;

    use super::*;
    use crate::fixtures::{self, CounterKind};

    /// A clip with no drawings, whose game video is source 0.
    fn clip() -> Clip {
        Clip {
            id: Uuid::nil(),
            name: "c".into(),
            notes: String::new(),
            tags: Vec::new(),
            source_index: 0,
            start_source_seconds: 0.0,
            recording_duration: 2.0,
            recording_filename: "c.mkv".into(),
            events: Vec::new(),
            show_pip: false,
            inset: Inset::Camera,
            sort_index: 0,
            created_at: "2026-09-19T00:00:00Z".into(),
            transcript: String::new(),
        }
    }

    /// Entries reading `files` in turn, with no clips: the shape
    /// [`close_unread`] reads.
    fn entries(files: &[&str]) -> Vec<EntryMedia> {
        files
            .iter()
            .map(|file| EntryMedia {
                source: PathBuf::from(file),
                clip: None,
                match_media: Arc::default(),
            })
            .collect()
    }

    /// The open sources after [`close_unread`] has run at entry `from`, sorted.
    fn left_open(files: &[&str], from: usize) -> Vec<String> {
        let mut open: HashMap<PathBuf, ()> = files
            .iter()
            .take(from + 1)
            .map(|f| (f.into(), ()))
            .collect();
        close_unread(&mut open, &entries(files), from);
        let mut left: Vec<String> = open.keys().map(|p| p.display().to_string()).collect();
        left.sort();
        left
    }

    /// The bound on the decoder cache: a file is closed one entry boundary
    /// **past** the one after its last reader, and only there.
    ///
    /// The window keeps the previous entry, because at the boundary its last
    /// frames are still in flight downstream and freeing their buffer pool is
    /// the hazard the reference laptop has and CI's llvmpipe does not — so
    /// three matches in a row hold two decoders, never one and never three.
    #[test]
    fn a_decoder_is_dropped_when_no_later_entry_reads_it() {
        // Three matches in a row: at each boundary the entry just left is still
        // open and the one before it has gone.
        assert_eq!(left_open(&["a", "b", "c"], 1), ["a", "b"]);
        assert_eq!(left_open(&["a", "b", "c"], 2), ["b", "c"]);
        assert_eq!(left_open(&["a", "b", "c", "d"], 3), ["c", "d"]);
        // A file a later entry comes back to stays open, so nothing is ever
        // reopened.
        assert_eq!(left_open(&["a", "b", "a"], 1), ["a", "b"]);
        assert_eq!(left_open(&["a", "b", "a"], 2), ["a", "b"]);
        // One match, walked over and over: its decoder is opened once and
        // never dropped, exactly as before the cache was bounded.
        assert_eq!(left_open(&["a", "a", "a"], 1), ["a"]);
        assert_eq!(left_open(&["a", "a", "a"], 2), ["a"]);
    }

    #[test]
    fn part_path_appends_to_the_file_name() {
        assert_eq!(
            part_path(Path::new("/a/b c.mp4")),
            PathBuf::from("/a/b c.mp4.part")
        );
    }

    #[test]
    fn the_output_size_follows_the_picker() {
        assert_eq!(output_size(Resolution::R720), (1280, 720));
        assert_eq!(output_size(Resolution::R1080), (1920, 1080));
    }

    /// The measured ladder, and that both encoders are actually told it: the
    /// numbers in [`quantizers`] are a table of bitrates and SSIMs on real
    /// footage, and a typo here is a silent 2× in what a coach downloads.
    #[test]
    fn the_quality_ladder_reaches_both_encoders() {
        assert_eq!(
            [Quality::Low, Quality::Medium, Quality::High].map(quantizers),
            [(30, 26), (26, 22), (22, 18)]
        );
        for quality in [Quality::Low, Quality::Medium, Quality::High] {
            let (qp, crf) = quantizers(quality);
            let [(va, va_settings), (x264, x264_settings)] = encoders(quality);
            assert_eq!(va, "vah264lpenc");
            assert_eq!(
                va_settings,
                format!("rate-control=cqp qpi={qp} qpp={qp} key-int-max=60")
            );
            assert_eq!(x264, "x264enc");
            assert!(
                x264_settings.contains(&format!("quantizer={crf} ")),
                "{quality:?}: {x264_settings}"
            );
            // Without this the level is capped at `bitrate`'s 2048 kbit/s
            // default, whatever quantizer it was given.
            assert!(
                x264_settings.contains("vbv-buf-capacity=0"),
                "{quality:?}: {x264_settings}"
            );
        }
    }

    /// An element erroring mid-stream, downstream of `appsrc`, ends the
    /// export: a blocking push would hang here forever.
    #[test]
    fn a_mid_stream_error_fails_the_export_without_hanging() {
        gst::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = fixtures::counter_video(
            &dir.path().join("src.webm"),
            640,
            360,
            25,
            75,
            CounterKind::Vp8WebmWithAudio,
        );
        let path = dir.path().join("out.mp4");
        let frames = (0..60)
            .map(|n| FrameSpec {
                entry: 0,
                source_time: f64::from(n) / 30.0,
                zoom: Zoom::IDENTITY,
            })
            .collect();
        let clip = clip();
        let job = ExportJob {
            tags: FileTags::default(),
            compilation: fixtures::one_entry(&clip, frames, ""),
            path: path.clone(),
            cues: None,
            render: Render::Encode(Encode {
                entries: vec![EntryMedia {
                    source,
                    clip: Some(ClipMedia {
                        recording: dir.path().join("missing.mkv"),
                        clip,
                    }),
                    match_media: Arc::default(),
                }],
                audio: Vec::new(),
                resolution: Resolution::R720,
                quality: Quality::Medium,
            }),
        };
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let _exporter = Exporter::spawn(
            job,
            move |msg| {
                let _ = tx.send(msg);
            },
            Some("identity error-after=10"),
        );

        let deadline = Duration::from_secs(60);
        let result = loop {
            match rx.recv_timeout(deadline.saturating_sub(started.elapsed())) {
                Ok(ExportMessage::Finished(result)) => break result,
                Ok(ExportMessage::Progress(_)) => {}
                Err(_) => panic!("no Finished within {deadline:?}"),
            }
        };
        assert!(
            matches!(result, Err(ExportError::Failed(_))),
            "expected a failure, got {result:?}"
        );
        assert!(!path.exists());
        assert!(!part_path(&path).exists());
    }

    /// Every export is the shape the live self-view places its inset in
    /// (`layout::pip_rect_over_picture`).
    #[test]
    fn every_resolution_is_the_layout_s_output_aspect() {
        for resolution in [Resolution::R720, Resolution::R1080, Resolution::R2160] {
            let (w, h) = output_size(resolution);
            assert!(
                (f64::from(w) / f64::from(h) - video_coach_core::layout::OUTPUT_ASPECT).abs()
                    < 1e-9,
                "{resolution:?} is {w}×{h}"
            );
        }
    }
}
