//! The preview tail (spec P1–P4): the composite on screen, in the same
//! [`FrameMailbox`] the source player fills, paced by the commentary's audio
//! clock.
//!
//! ```text
//! pump:  appsrc name=src -- the source frame, GL memory, PTS n/30
//!        appsrc name=ov  -- its overlay, output-size RGBA, the SAME PTS
//!        appsrc name=pip -- an avatar clip's image, the SAME PTS, at a rect
//!                           the pulse sizes (spec E2); absent otherwise
//! rec:   filesrc ! decodebin3 -- video to the PiP pad (only with a camera
//!                                inset), audio to volume ! autoaudiosink
//! tail:  glvideomixer ! 1280x720 30/1 ! glcolorconvert ! RGBA GL ! appsink
//!                                                                  sync=true
//! ```
//!
//! **Record time is output time.** `playback_segments` emits its durations in
//! the recording's own timeline, so output frame `n` is at `n/30` there too.
//! That is why the recording needs no pump, no re-timestamping and no appsink:
//! it plays natively and the mixer aligns the pads by running time. It is also
//! why the overlay's `record_time` is simply `n/30`.
//!
//! **The bar is export's own** (spec E7). Preview runs the clip's one-entry
//! compilation, so its overlay carries the same text bar the file gets, and
//! the line reads `1 / 1 | <name> | tags` because the target is that one clip.
//! The scoreboard comes off the same per-frame call the export makes. What the
//! coach checks here is what the export shows.
//!
//! **One pump, every appsrc, one PTS.** `glvideomixer` waits indefinitely on
//! every pad, so frame `n`'s overlay — and an avatar's image — goes out with
//! frame `n` or the mixer starves. For the same reason the inset pad is
//! requested **only** when there is something to feed it: the recording's
//! video pad with `shows_camera_pip` **and a video track to show it**
//! ([`camera_inset`]), the avatar's appsrc with `shows_avatar`, and neither
//! otherwise. An unlinked video pad `decodebin3` tolerates without stalling its
//! branch (measured).
//!
//! **The audio sink is the clock** (measured: `GstPulseSinkClock`), so the
//! composite follows the commentary — the track the coach hears. The video
//! appsink syncs to that clock, and its backpressure is the whole of the
//! pump's pacing: there is no sleep loop, and PAUSED stops the pump through
//! that same backpressure (measured), so pausing is a state change and
//! nothing else.
//!
//! **Transport runs through the pipeline, not around it** (spec P3). A seek is
//! one pipeline seek: the recording branch seeks natively, and every pumped
//! appsrc, being `stream-type=seekable`, answers `seek-data` by moving the
//! pump's [`Cursor`]. Everything the owner steers — state, seeks, volume — goes
//! through [`Control`], because the graph itself lives and dies on the pump's
//! thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use video_coach_core::export::{Compilation, OUTPUT_FPS};
use video_coach_core::highlight::{highlight_shapes, HighlightShape, PlayerHighlight};
use video_coach_core::layout::pip_rect;
use video_coach_core::project::Clip;
use video_coach_core::scoreboard::{ScoreboardConfig, ScoreboardContext, ScoreboardState};

use super::decode::Decoder;
use super::{
    avatar, display_aspect, fit_rect, frame_index, frame_time, head, install_overlay_pad,
    install_zoom, level_at, overlay_branch, place, premultiplied_over, pulsed, rounded, stamp,
    stamp_buffer, wait_for_room, CompositeError, Gl, PadRect, Stopper, Watch, POLL, QUEUED,
};
use crate::mailbox::FrameMailbox;
use crate::overlay::{OverlayFrame, OverlayRenderer};
use crate::player::{fill_mailbox, gain, gl_caps, seconds_to_clock};

/// The preview's output size. Measured (spec P1): of the sizes tried, 720p
/// gave the best UI frame time by a wide margin, and a bigger composite buys
/// nothing on a picture the window is showing at that size anyway.
const OUTPUT_WIDTH: i32 = 1280;
const OUTPUT_HEIGHT: i32 = 720;

/// How long the composite may go without producing the frame waited for
/// before it is declared stuck. Nothing here waits without a bound.
const STALL: Duration = Duration::from_secs(5);

/// What to preview: a snapshot, so later edits don't reach a running preview.
#[derive(Debug, Clone)]
pub struct PreviewJob {
    /// The game video.
    pub source: PathBuf,
    /// The commentary recording, under the project's `recordings/`.
    pub recording: PathBuf,
    /// The clip itself, for its drawings and its `show_pip`.
    pub clip: Clip,
    /// The clip as a **one-entry compilation**
    /// (`video_coach_core::export::compilation_schedule` on
    /// `ExportTarget::Clip`): the frames to pump, and the entry whose `text`
    /// the bar draws — `1 / 1 | <name> | tags`, since the target is this one
    /// clip (spec E7). One schedule builds preview and export, so the picture
    /// the coach checks is the picture the file gets.
    pub compilation: Compilation,
    /// The commentary's volume, the project's `preview_commentary_volume`, in
    /// the volume slider's `0..=1` space.
    pub commentary_volume: f64,
    /// The match clock and score to draw, or `None` when the project has no
    /// scoreboard configured. Built once by the bus, and **never reused across
    /// a source add, move, remove or relink** — see [`ScoreboardContext`].
    pub scoreboard: Option<ScoreboardContext>,
    /// The project's player highlights, a snapshot like the clip itself. They
    /// belong to the footage rather than to the clip (spec H1), so the preview
    /// shows every one the clip's span crosses.
    pub highlights: Vec<PlayerHighlight>,
    /// The project's avatar image, or `None` for a project that records on
    /// camera. Read only by a clip whose `inset` is the avatar (spec A1, B2).
    pub avatar: Option<PathBuf>,
}

/// What a running preview reports, on its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum PreviewMessage {
    /// The schedule ran out. The preview holds its last frame and stays open
    /// until it is dropped; the recording's tail does not play on (spec P3).
    Ended,
    /// The preview stopped early, with this message for the user.
    Failed(String),
}

/// What a preview's sink saw. A composite that can't hold 30 fps is the one
/// failure the picture doesn't show, so this is logged when a preview closes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewStats {
    /// Frames out of the mixer.
    pub composited: u64,
    /// Frames the sink reported dropping, from its QoS messages. **Since the
    /// last flush**: the sink restarts its own statistics at every seek.
    pub dropped: u64,
}

/// Where a preview is, in output frames — the counter the pump stores and the
/// UI's 30 Hz tick reads, so a preview needs no position event of its own
/// (spec P3). The owner holds it, as it holds the mailbox, and passes it to
/// each preview it starts.
///
/// It is the frame the pump last pushed (or the one a seek asked for), which
/// leads the picture by whatever is queued (at most [`super::QUEUED`] frames,
/// 0.13 s), and reaches the schedule's length when it ends.
#[derive(Debug, Clone, Default)]
pub struct PreviewPosition(Arc<AtomicU64>);

impl PreviewPosition {
    pub fn seconds(&self) -> f64 {
        self.0.load(Ordering::SeqCst) as f64 / f64::from(OUTPUT_FPS)
    }

    fn store(&self, frame: u64) {
        self.0.store(frame, Ordering::SeqCst);
    }
}

/// A running preview. It owns its thread, and every GStreamer object it
/// creates lives and dies on that thread. Dropping it closes and joins.
pub struct Preview {
    cancel: Arc<AtomicBool>,
    shared: Arc<Shared>,
    /// The schedule's length, for clamping a seek.
    frames: u64,
    thread: Option<JoinHandle<()>>,
}

impl Preview {
    /// Starts previewing `job` into `mailbox`, compositing on `gl` — Slint's
    /// display and context in the app, [`Gl::shared`] with no UI (spec P1:
    /// "no private GL context" is an app rule, not a test rule). `position`
    /// is where the pump publishes, for whoever draws the readout; it starts
    /// again at the top of the clip.
    ///
    /// `on_message` is called on the preview thread. `job` must have frames:
    /// the bus refuses an empty clip.
    pub fn start(
        job: PreviewJob,
        gl: Gl,
        mailbox: FrameMailbox,
        position: PreviewPosition,
        mut on_message: impl FnMut(PreviewMessage) + Send + 'static,
    ) -> Preview {
        debug_assert!(!job.compilation.frames.is_empty(), "a preview needs frames");
        let cancel = Arc::new(AtomicBool::new(false));
        let frames = job.compilation.frames.len() as u64;
        let shared = Arc::new(Shared {
            counters: Counters::default(),
            cursor: Mutex::new(Cursor::default()),
            control: Mutex::new(Control {
                graph: None,
                playing: true,
                gain: gain(job.commentary_volume),
                pending_seek: None,
            }),
            position,
        });
        shared.position.store(0);
        let thread = std::thread::Builder::new()
            .name("preview".into())
            .spawn({
                let (cancel, shared) = (cancel.clone(), shared.clone());
                move || {
                    let watch = Watch {
                        cancel: &cancel,
                        error: Arc::default(),
                    };
                    let result = run(&job, &gl, &mailbox, &shared, &watch, &mut on_message);
                    // Nothing outside may steer a graph that has stopped.
                    shared.control().graph = None;
                    // `Cancelled` is the close the user asked for, and has
                    // nothing to report. Anything else is the graph giving up.
                    if let Err(CompositeError::Failed(e)) = result {
                        on_message(PreviewMessage::Failed(e));
                    }
                }
            })
            .expect("spawn the preview thread");
        Preview {
            cancel,
            shared,
            frames,
            thread: Some(thread),
        }
    }

    /// What the sink has seen so far.
    pub fn stats(&self) -> PreviewStats {
        self.shared.counters.stats()
    }

    /// Plays or holds the picture, through the pipeline's state: PAUSED stops
    /// the pump through the appsrcs' backpressure, and the recording branch
    /// and the audio clock stop with it.
    pub fn set_playing(&self, playing: bool) {
        // There is nothing to play on from the end: the schedule has run out
        // and the picture is frozen on its last frame, so a play there starts
        // the clip again.
        if playing && self.shared.cursor().frame >= self.frames {
            self.seek(0.0);
        }
        let state = match playing {
            true => gst::State::Playing,
            false => gst::State::Paused,
        };
        // The lock is held across the state change so it can't cross the
        // graph being published, which starts it in `playing`'s state -- and
        // the graph isn't built until the first source frame is decoded, so
        // that is a real couple of hundred milliseconds.
        let mut control = self.shared.control();
        control.playing = playing;
        if let Some(graph) = &control.graph {
            if graph.pipeline.set_state(state).is_err() {
                eprintln!("preview: could not go to {state:?}");
            }
        }
    }

    /// Seeks to `seconds` into the clip, frame-accurately (spec P3: 25–40 ms
    /// a tick, so a scrub needs no keyframe tolerance).
    ///
    /// One pipeline seek: the recording branch seeks natively and both
    /// appsrcs answer `seek-data` by moving the pump.
    pub fn seek(&self, seconds: f64) {
        let frame = ((seconds.max(0.0) * f64::from(OUTPUT_FPS)).round() as u64)
            .min(self.frames.saturating_sub(1));
        // Where the preview is, from here on: a skip reads it back, and the
        // readout must not show the frame the seek left behind -- a pipeline
        // that hasn't taken the seek yet pushes nothing for a while.
        self.shared.position.store(frame);
        // The lock is held across the seek so it can't cross the graph being
        // published. A seek the graph won't take yet -- before it exists, or
        // before it has prerolled -- is left for the pump to retry, since
        // only the pump can get it to preroll.
        let mut control = self.shared.control();
        let taken = control
            .graph
            .as_ref()
            .is_some_and(|graph| seek_to(&graph.pipeline, frame));
        control.pending_seek = (!taken).then_some(frame);
    }

    /// Sets the commentary's volume from a slider value in `0..=1`. A live
    /// property set on the `volume` element (spec P2), which is how the bus
    /// mutes the drag of a scrub.
    pub fn set_volume(&self, linear: f64) {
        let mut control = self.shared.control();
        control.gain = gain(linear);
        if let Some(graph) = &control.graph {
            graph.volume.set_property("volume", control.gain);
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the preview's owner and its thread share. Two locks, never held at
/// once except by a seek, which takes `control` and then `cursor` through
/// `seek-data`.
struct Shared {
    counters: Counters,
    cursor: Mutex<Cursor>,
    control: Mutex<Control>,
    position: PreviewPosition,
}

impl Shared {
    fn cursor(&self) -> std::sync::MutexGuard<'_, Cursor> {
        self.cursor.lock().expect("the cursor isn't poisoned")
    }

    fn control(&self) -> std::sync::MutexGuard<'_, Control> {
        self.control.lock().expect("the control isn't poisoned")
    }
}

/// Where the pump is, and which seek put it there.
///
/// **One mutex over both, and the generation is load-bearing.** `seek-data`
/// arrives on the *seeking* thread, not the pump's, so the pump re-reads the
/// generation under this lock immediately before it pushes: a push that
/// crosses a `FLUSH_STOP` is accepted silently (measured), and would put a
/// frame from before the seek onto the segment after it.
#[derive(Default)]
struct Cursor {
    /// The next output frame to push.
    frame: u64,
    /// Bumped by every seek. A pipeline seek reaches both appsrcs, so one
    /// seek may bump it twice; only a change matters.
    generation: u64,
    /// Where the last seek left the pump: the first frame the mixer has
    /// produced since, which is what [`Composite::end`] counts from.
    resumed: u64,
}

impl Cursor {
    fn read(&self) -> (u64, u64) {
        (self.frame, self.generation)
    }

    fn seek_to(&mut self, frame: u64) {
        self.frame = frame;
        self.generation += 1;
        self.resumed = frame;
    }
}

/// The graph, once the pump has built it, and what the owner asked for before
/// then. The graph isn't built until the first source frame has been decoded,
/// so a state or volume change made meanwhile is remembered here and applied
/// when it starts.
struct Control {
    graph: Option<Graph>,
    playing: bool,
    gain: f64,
    /// A seek the graph hasn't taken yet, retried by the pump. It has to be
    /// a real pipeline seek and not a nudge of the [`Cursor`]: the recording
    /// branch moves with the pump, and `appsrc` answers its own start-up
    /// `seek-data` at 0, which would undo one.
    pending_seek: Option<u64>,
}

/// What the owner reaches into the running graph for.
struct Graph {
    pipeline: gst::Pipeline,
    volume: gst::Element,
}

/// Runs the preview until it is closed or fails, reporting
/// [`PreviewMessage::Ended`] when the schedule runs out. It returns only once
/// the preview is over: the pipeline lives on this thread, so the thread has
/// to outlast the picture it is holding.
fn run(
    job: &PreviewJob,
    gl: &Gl,
    mailbox: &FrameMailbox,
    shared: &Arc<Shared>,
    watch: &Watch,
    on_message: &mut impl FnMut(PreviewMessage),
) -> Result<(), CompositeError> {
    let total = job.compilation.frames.len() as u64;
    // The avatar, before the pump starts (spec D5): its image decoded and
    // pre-scaled once, and one pulse level per output frame read out of the
    // recording's own sound. An audio decode between two pushed frames would
    // stall the pump — and here it would stall the picture.
    let avatar = AvatarInset::open(job, watch.cancel);
    // What the inset pad will carry, decided before the graph asks for it: the
    // avatar if this clip has one, else the recording's own video if it has
    // some to give (see [`camera_inset`]), else no pad at all.
    let inset = match &avatar {
        Some(avatar) => InsetSource::Avatar(avatar),
        None if camera_inset(job) => InsetSource::Camera,
        None => InsetSource::Nothing,
    };
    let mut decoder = Decoder::start(&job.source, gl, watch)?;
    let mut overlays = OverlayRenderer::new();
    let mut composite: Option<Composite> = None;
    // Set once the schedule has run out and the tail has been flushed, and
    // cleared by a seek back into the schedule.
    let mut ended = false;
    loop {
        watch.check()?;
        if let Some(composite) = &composite {
            composite.retry_pending_seek();
        }
        let (n, generation) = shared.cursor().read();
        if n >= total {
            if !ended {
                let composite = composite
                    .as_ref()
                    .ok_or_else(|| CompositeError::Failed("the clip has no frames".into()))?;
                // A seek during the drain leaves the schedule unfinished, and
                // the pump picks it up from the top of the loop instead.
                if !composite.end(total, generation, watch)? {
                    continue;
                }
                shared.position.store(total);
                on_message(PreviewMessage::Ended);
                ended = true;
            }
            // The picture is held, and the thread with it, until the preview
            // is closed or seeked back inside the schedule (spec P3).
            std::thread::sleep(POLL.into());
            continue;
        }
        ended = false;
        let frame = &job.compilation.frames[n as usize];
        let sample = decoder.frame_at(seconds_to_clock(frame.source_time), watch)?;
        let entry = job.compilation.plan.entries.get(frame.entry);
        // The scoreboard's clock is the **displayed** frame's source time, so
        // a pause in the commentary leaves it where it was (BACKLOG #27).
        let scoreboard = job
            .scoreboard
            .as_ref()
            .zip(entry)
            .and_then(|(context, entry)| {
                let state = context.state_at(entry.source_index, frame.source_time)?;
                Some((context.config(), state))
            });
        // The first frame's caps shape the composite: its size, PAR and
        // memory, and with them the picture rect the overlay is drawn at.
        if composite.is_none() {
            composite = Some(Composite::start(
                sample, job, inset, gl, mailbox, shared, watch,
            )?);
        }
        let composite = composite.as_ref().expect("started above");
        // Highlights are keyed by the displayed frame's source time as well,
        // and mapped through its own zoom. Core owns that geometry; the
        // overlay only draws what comes back.
        let highlights = entry.map_or_else(Vec::new, |entry| {
            highlight_shapes(
                &job.highlights,
                entry.source_index,
                frame.source_time,
                frame.zoom,
                f64::from(composite.picture.2),
                f64::from(composite.picture.3),
            )
        });
        composite.push(
            Frame {
                n,
                generation,
                sample,
                clip: &job.clip,
                highlights: &highlights,
                scoreboard,
            },
            &mut overlays,
            watch,
        )?;
    }
}

/// What the sink counts, shared with [`Preview::stats`].
///
/// **The end of the schedule is a count, not a timestamp**, because a late
/// frame is dropped rather than delivered: rendered plus dropped is the only
/// complete account of what the mixer produced. The account is kept *since
/// the last flush*, so a seek doesn't leave [`Composite::end`] waiting for
/// frames that were never going to be composited -- and the sink resets its
/// own QoS statistics on a flush anyway, so `dropped` is that span too.
#[derive(Default)]
struct Counters {
    composited: AtomicU64,
    dropped: AtomicU64,
    rendered_since_flush: AtomicU64,
}

impl Counters {
    fn sample(&self) {
        self.composited.fetch_add(1, Ordering::SeqCst);
        self.rendered_since_flush.fetch_add(1, Ordering::SeqCst);
    }

    /// A flush: the sink's own QoS statistics restart here, so this account
    /// does too.
    fn flushed(&self) {
        self.rendered_since_flush.store(0, Ordering::SeqCst);
        self.dropped.store(0, Ordering::SeqCst);
    }

    /// Frames the mixer has produced since the last flush, rendered or
    /// dropped.
    fn since_flush(&self) -> u64 {
        self.rendered_since_flush.load(Ordering::SeqCst) + self.dropped.load(Ordering::SeqCst)
    }

    fn stats(&self) -> PreviewStats {
        PreviewStats {
            composited: self.composited.load(Ordering::SeqCst),
            dropped: self.dropped.load(Ordering::SeqCst),
        }
    }
}

/// One output frame's own inputs, as `export.rs`'s `Frame` is for the export.
/// The renderer and the [`Watch`] belong to the run rather than the frame, so
/// they stay arguments of their own.
struct Frame<'a> {
    /// The output frame index: its PTS is `n/30`.
    n: u64,
    /// The seek generation this frame was prepared under.
    generation: u64,
    /// The decoded source frame to show.
    sample: &'a gst::Sample,
    /// The clip, for the drawings the overlay replays.
    clip: &'a Clip,
    /// The rings showing at this frame, in picture pixels.
    highlights: &'a [HighlightShape],
    /// The board at this frame, from the job's context, or `None` when the
    /// project has no scoreboard or nothing has been tagged yet.
    scoreboard: Option<(&'a ScoreboardConfig, ScoreboardState)>,
}

/// A preview's avatar inset: the image the pad carries, where it sits at full
/// size, and one pulse level per output frame.
///
/// Built once, before the pump (spec D5). `None` for a camera clip, for a clip
/// with no inset at all, and for one whose project has lost its image — which
/// costs the inset and not the preview (spec A4), as it does an export.
struct AvatarInset {
    /// The pre-scaled, premultiplied, circular pixmap as one system-memory
    /// buffer, pushed with a new PTS every frame through the branch's own
    /// `glupload` (spec E2). Nothing else shares this pad in a preview — a
    /// preview is one clip — so it needs no upload of its own.
    buffer: gst::Buffer,
    width: i32,
    height: i32,
    /// The avatar's box — `avatar_box` of the square `layout::pip_rect` —
    /// which its circle is drawn in, at the preview's output size. The pulse
    /// scales it per frame, in the pad's probe.
    rect: PadRect,
    /// One pulse level per output frame of the clip, from the recording's own
    /// commentary.
    levels: Arc<[f64]>,
}

impl AvatarInset {
    fn open(job: &PreviewJob, cancel: &AtomicBool) -> Option<AvatarInset> {
        if !job.clip.shows_avatar() {
            return None;
        }
        let path = job.avatar.as_deref()?;
        let avatar =
            avatar::open_reported(path, f64::from(OUTPUT_WIDTH), f64::from(OUTPUT_HEIGHT))?;
        let levels = avatar::pulse_table(&job.recording, job.compilation.frames.len(), cancel);
        Some(AvatarInset {
            width: avatar.image.width() as i32,
            height: avatar.image.height() as i32,
            buffer: gst::Buffer::from_mut_slice(avatar.image.data().to_vec()),
            rect: rounded(avatar.rect),
            levels: levels.into(),
        })
    }
}

/// What a preview's inset pad carries, decided before the graph is built.
///
/// One value rather than two flags: the three cases are exclusive, and the pad
/// is requested, placed, fed and linked from this one answer (spec F), which is
/// what keeps those four sites from disagreeing.
#[derive(Clone, Copy)]
enum InsetSource<'a> {
    /// The recording's own video, played natively onto the pad.
    Camera,
    /// The project's avatar, pushed from an `appsrc` of its own.
    Avatar(&'a AvatarInset),
    /// Nothing: no pad is requested at all.
    Nothing,
}

/// The composite pipeline: three mixer pads and the tail into the mailbox.
struct Composite {
    pipeline: Stopper,
    /// The pumped source frames.
    src: gst_app::AppSrc,
    /// Their overlays, rasterized at the **output** size with the strokes
    /// mapped into the picture rect (spec E2).
    overlay: gst_app::AppSrc,
    /// An avatar clip's image on the inset pad, and the appsrc it goes into:
    /// the same buffer re-stamped every frame, sized by the pulse in the pad's
    /// own probe (spec E2, E3). `None` for every other clip.
    avatar: Option<(gst_app::AppSrc, gst::Buffer)>,
    /// The picture rect the strokes are mapped into.
    picture: PadRect,
    /// The bar's line, from the compilation's one entry: fixed for the run,
    /// since the entry is.
    text: String,
    shared: Arc<Shared>,
}

impl Composite {
    /// Builds the graph for source frames shaped like `first` and starts it in
    /// the state the owner has asked for. Output frame `n` gets the zoom of
    /// `job.frames[n]`. Its errors reach `watch`.
    ///
    /// **Never waits for PLAYING:** the graph can't preroll until the pump
    /// pushes, and the pump is the caller.
    fn start(
        first: &gst::Sample,
        job: &PreviewJob,
        inset: InsetSource,
        gl: &Gl,
        mailbox: &FrameMailbox,
        shared: &Arc<Shared>,
        watch: &Watch,
    ) -> Result<Composite, CompositeError> {
        let (camera, avatar) = match inset {
            InsetSource::Camera => (true, None),
            InsetSource::Avatar(avatar) => (false, Some(avatar)),
            InsetSource::Nothing => (false, None),
        };
        let caps = first
            .caps()
            .ok_or_else(|| CompositeError::Failed("a decoded frame has no caps".into()))?;
        let info = gst_video::VideoInfo::from_caps(caps)
            .map_err(|e| CompositeError::Failed(format!("unusable decoded caps {caps}: {e}")))?;
        let picture = fit_rect(&info, OUTPUT_WIDTH, OUTPUT_HEIGHT);

        // The inset pad is requested only when there is something to feed it:
        // the recording's video for a camera clip that has some, the avatar's
        // own appsrc for an avatar one. With neither, the pad is never asked
        // for and the recording's video pad (if it has one) is left unlinked.
        let pip = match (camera, avatar) {
            (true, _) => "queue name=pipq ! glupload ! glcolorconvert ! mix.sink_1 ".to_owned(),
            (false, Some(avatar)) => format!(
                "appsrc name=pip format=time is-live=false block=false \
                   max-buffers={QUEUED} max-bytes=0 max-time=0 \
                   caps=video/x-raw,format=RGBA,width={w},height={h},\
                     framerate={OUTPUT_FPS}/1 \
                 ! glupload ! glcolorconvert \
                 ! video/x-raw(memory:GLMemory),format=RGBA ! mix.sink_1 ",
                w = avatar.width,
                h = avatar.height,
            ),
            (false, None) => String::new(),
        };
        let description = format!(
            "{head} ! glcolorconvert \
             ! appsink name=out sync=true qos=true max-buffers=1 enable-last-sample=false \
             {overlay} \
             {pip}\
             queue name=audioq ! audioconvert ! audioresample \
             ! volume name=vol ! autoaudiosink",
            head = head(OUTPUT_WIDTH, OUTPUT_HEIGHT),
            overlay = overlay_branch(OUTPUT_WIDTH, OUTPUT_HEIGHT),
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| CompositeError::Failed(format!("could not build the preview graph: {e}")))?
            .downcast::<gst::Pipeline>()
            .expect("a multi-element launch string yields a pipeline");
        let by_name = |n: &str| pipeline.by_name(n).expect("named in the launch string");

        let mut caps = caps.to_owned();
        caps.make_mut()
            .set("framerate", gst::Fraction::new(OUTPUT_FPS as i32, 1));
        let appsrc = |name: &str| {
            by_name(name)
                .downcast::<gst_app::AppSrc>()
                .expect("named as an appsrc in the launch string")
        };
        let src = appsrc("src");
        src.set_caps(Some(&caps));
        let overlay = appsrc("ov");
        let avatar_src = avatar.map(|_| appsrc("pip"));
        // Every pumped appsrc is seekable, and answers a pipeline seek by
        // moving the pump (spec P3). `format=time`, so `seek-data`'s offset is
        // nanoseconds on the output timeline. One seek reaches all of them, so
        // it bumps the generation once per pad; only a change matters.
        for appsrc in [&src, &overlay].into_iter().chain(avatar_src.as_ref()) {
            appsrc.set_stream_type(gst_app::AppStreamType::Seekable);
            appsrc.set_callbacks(
                gst_app::AppSrcCallbacks::builder()
                    .seek_data({
                        let shared = shared.clone();
                        move |_, offset| {
                            shared
                                .cursor()
                                .seek_to(frame_index(gst::ClockTime::from_nseconds(offset)));
                            shared.counters.flushed();
                            true
                        }
                    })
                    .build(),
            );
        }

        let mix = by_name("mix");
        let mix_pad = |name: &str| {
            mix.static_pad(name)
                .expect("requested in the launch string")
        };
        // The base takes the picture rect; the overlay is the whole output
        // frame, with the strokes mapped into that same rect inside it (spec
        // E2), so a drawing lands on the picture and not across the letterbox
        // bars while the bar and the scoreboard keep the frame. The PiP is
        // chrome in output space and waits for the camera's shape.
        let base_pad = mix_pad("sink_0");
        place(&base_pad, picture, 0);
        let overlay_pad = install_overlay_pad(&mix, OUTPUT_WIDTH, OUTPUT_HEIGHT);
        let avatar_pad = avatar.map(|avatar| {
            let pad = mix_pad("sink_1");
            // A premultiplied pixmap, like the overlay's layer (spec E4).
            premultiplied_over(&pad);
            place_avatar(&pad, avatar.rect, avatar.levels.clone());
            pad
        });
        // The pumped pads are sent EOS at the end of the schedule (see
        // `Composite::end`), and an EOS pad is otherwise not drawn at all:
        // with the recording still running on pad 1, the freeze would be on
        // a black frame (measured -- the composite test caught one).
        for pad in [&base_pad, &overlay_pad]
            .into_iter()
            .chain(avatar_pad.as_ref())
        {
            pad.set_property("repeat-after-eos", true);
        }
        if camera {
            place_pip(&mix_pad("sink_1"));
        }
        // One entry, laid out once above rather than per entry, so the zoom is
        // all the preview's pads read out of the schedule.
        install_zoom(&by_name("zoom"), &job.compilation.frames.clone().into());

        let out = by_name("out")
            .downcast::<gst_app::AppSink>()
            .expect("`out` is an appsink");
        out.set_caps(Some(&gl_caps()));
        // The preroll half of this is what puts a frame up when a scrub lands
        // while the preview is paused.
        fill_mailbox(&out, mailbox.clone(), {
            let shared = shared.clone();
            move || shared.counters.sample()
        });

        link_recording(&pipeline, job, camera, &by_name)?;
        // The sink's QoS reports are the only place a dropped frame shows up
        // -- and it does drop, so `qos=true` on the sink is load-bearing: the
        // audio is the clock, and a late picture kept would slide further and
        // further behind the words it belongs to.
        gl.install(&pipeline, watch, {
            let shared = shared.clone();
            move |msg| {
                if let gst::MessageView::Qos(qos) = msg.view() {
                    // `(processed, dropped)`, both cumulative for the sink.
                    let dropped = qos.stats().1.value().max(0) as u64;
                    shared.counters.dropped.store(dropped, Ordering::SeqCst);
                }
            }
        });

        let volume = by_name("vol");
        let pipeline = Stopper(pipeline);
        // The owner may already have asked for a state and a volume, so the
        // graph is published and started under one lock.
        let started = {
            let mut control = shared.control();
            volume.set_property("volume", control.gain);
            let state = match control.playing {
                true => gst::State::Playing,
                false => gst::State::Paused,
            };
            control.graph = Some(Graph {
                pipeline: pipeline.0.clone(),
                volume,
            });
            pipeline.set_state(state)
        };
        if started.is_err() {
            return Err(watch.failure("could not start the preview"));
        }
        Ok(Composite {
            pipeline,
            src,
            overlay,
            avatar: avatar_src.zip(avatar.map(|a| a.buffer.clone())),
            picture,
            text: job
                .compilation
                .plan
                .entries
                .first()
                .map_or_else(String::new, |entry| entry.text.clone()),
            shared: shared.clone(),
        })
    }

    /// Pushes output frame `frame.n`: its texture on the base pad and its
    /// overlay at `n/30` on the overlay pad, **both stamped `n/30`**. A seek
    /// that landed since `frame.generation` was read discards both.
    ///
    /// The base is a buffer reference, not a pixel copy: a freeze sends the
    /// same texture out many times.
    fn push(
        &self,
        frame: Frame,
        overlays: &mut OverlayRenderer,
        watch: &Watch,
    ) -> Result<(), CompositeError> {
        let Frame {
            n,
            generation,
            sample,
            clip,
            highlights,
            scoreboard,
        } = frame;
        let base = stamp(sample, n);
        // Record time is output time (see the module docs), so the overlay's
        // moment is the output frame's own, and its stamp the frame's own.
        let mut overlay = overlays.render(
            &OverlayFrame {
                clip: Some(clip),
                record_time: n as f64 / f64::from(OUTPUT_FPS),
                picture: self.picture,
                highlights,
                text: &self.text,
                scoreboard,
            },
            OUTPUT_WIDTH as u32,
            OUTPUT_HEIGHT as u32,
        );
        stamp_buffer(&mut overlay, n);
        // A new header over the same pixels, never a copy of them.
        let inset = self.avatar.as_ref().map(|(appsrc, buffer)| {
            let mut buffer = buffer.copy();
            stamp_buffer(&mut buffer, n);
            (appsrc, buffer)
        });

        // Room on every pad first, so the cursor's lock is never held across a
        // wait: `seek-data` runs on the seeking thread, which must not queue
        // behind the pump. PAUSED is what makes this wait the pump's pause.
        wait_for_room(&self.src, watch)?;
        wait_for_room(&self.overlay, watch)?;
        if let Some((appsrc, _)) = &inset {
            wait_for_room(appsrc, watch)?;
        }
        let mut cursor = self.shared.cursor();
        if cursor.generation != generation {
            // A seek landed while this frame was being prepared. See `Cursor`.
            return Ok(());
        }
        // Every appsrc has room, so no push waits under the lock.
        let push = |appsrc: &gst_app::AppSrc, buffer: gst::Buffer, what: &str| {
            appsrc
                .push_buffer(buffer)
                .map_err(|e| watch.failure(format!("pushing {what}: {e:?}")))
        };
        push(&self.src, base, &format!("frame {n}"))?;
        push(&self.overlay, overlay, &format!("overlay {n}"))?;
        if let Some((appsrc, buffer)) = inset {
            push(appsrc, buffer, &format!("the avatar of frame {n}"))?;
        }
        cursor.frame = n + 1;
        drop(cursor);
        self.shared.position.store(n);
        Ok(())
    }

    /// Issues the seek the graph wouldn't take when it was asked for. A
    /// pipeline that hasn't prerolled refuses one, and only the pump can get
    /// it to preroll, so this is the pump's.
    fn retry_pending_seek(&self) {
        let mut control = self.shared.control();
        if control
            .pending_seek
            .is_some_and(|frame| seek_to(&self.pipeline, frame))
        {
            control.pending_seek = None;
        }
    }

    /// The schedule is over: flush the tail, then hold the picture there.
    /// Returns whether it really ended — a seek landing meanwhile takes the
    /// preview back into the schedule, and nothing of the end applies.
    ///
    /// **Every pumped appsrc goes EOS first,** which is what tells the aggregator those
    /// pads are done rather than merely quiet. Phase 7's Task 2 measured the
    /// last seven frames -- the ones in flight -- arriving about a second
    /// after the rest on the reference laptop, taking the freeze with them;
    /// this is the signal that should stop `glvideomixer` waiting. It is not
    /// reproducible on a graph whose latency is zero, where those seven
    /// frames drain in their own 0.23 s either way, so the gain is unverified
    /// and the hands-on pass on real footage is what confirms it.
    fn end(&self, total: u64, generation: u64, watch: &Watch) -> Result<bool, CompositeError> {
        // The frames the mixer owes since the last seek, read with the
        // generation that says the seek is still the one the caller saw: a
        // seek landing here would leave the pump owing frames it is no longer
        // going to push, and the wait below would run out and fail the
        // preview on a scrub the user is allowed to make.
        let owed = {
            let cursor = self.shared.cursor();
            if cursor.generation != generation {
                return Ok(false);
            }
            total - cursor.resumed.min(total)
        };
        let _ = self.src.end_of_stream();
        let _ = self.overlay.end_of_stream();
        if let Some((appsrc, _)) = &self.avatar {
            let _ = appsrc.end_of_stream();
        }
        // Then wait for those frames. PAUSED stops the mixer where it stands,
        // rather than running on into the recording's tail (spec P3).
        if !self.await_end(owed, generation, watch)? {
            return Ok(false);
        }
        self.pause(watch)?;
        Ok(true)
    }

    /// Waits until the sink has accounted for `n` frames since the last
    /// flush. False if a seek landed meanwhile: the frames it flushed are
    /// never coming, and the schedule is running again.
    fn await_end(&self, n: u64, generation: u64, watch: &Watch) -> Result<bool, CompositeError> {
        let deadline = Instant::now() + STALL;
        loop {
            watch.check()?;
            if self.shared.cursor().generation != generation {
                return Ok(false);
            }
            if self.shared.counters.since_flush() >= n {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Err(CompositeError::Failed(
                    "the preview stopped composing frames".into(),
                ));
            }
            std::thread::sleep(POLL.into());
        }
    }

    /// Holds the picture where it is, and with it the recording. The owner's
    /// wish is updated too, so its next play is a real state change.
    fn pause(&self, watch: &Watch) -> Result<(), CompositeError> {
        let mut control = self.shared.control();
        control.playing = false;
        match self.pipeline.set_state(gst::State::Paused) {
            Ok(_) => Ok(()),
            Err(_) => Err(watch.failure("could not pause the preview")),
        }
    }
}

/// Seeks the whole graph to output frame `frame`, frame-accurately (spec P3:
/// 25-40 ms a tick, so a scrub needs no keyframe tolerance).
///
/// One seek does both branches: the recording seeks natively, and the two
/// appsrcs, being `stream-type=seekable`, answer `seek-data` by moving the
/// pump's [`Cursor`].
/// Returns whether the graph took it.
fn seek_to(pipeline: &gst::Pipeline, frame: u64) -> bool {
    pipeline
        .seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            frame_time(frame),
        )
        .is_ok()
}

/// Whether the inset pad carries this clip's recording: `shows_camera_pip`
/// **and** a video track to fill it with.
///
/// **The probe is not optional.** An avatar take's file has no video track
/// (spec B4), and neither has a webcam take whose camera died or whose file was
/// truncated; a mixer pad requested and never fed produces nothing and stalls
/// the whole preview, with no error (measured, Phase 8). Export probes here too
/// and falls back to its filler (`Pip::open`); preview has no pad at all
/// instead, which is the same outcome — no inset, and a preview that plays.
fn camera_inset(job: &PreviewJob) -> bool {
    if !job.clip.shows_camera_pip() {
        return false;
    }
    match crate::probe::probe(&job.recording) {
        Ok(_) => true,
        Err(e) => {
            eprintln!(
                "preview: no picture-in-picture for {}: {e}",
                job.recording.display()
            );
            false
        }
    }
}

/// Adds `filesrc ! decodebin3` for the recording and links its streams: video
/// to the PiP queue when there is one (`camera`), audio to the volume chain.
///
/// In Rust rather than the launch string because `decodebin3`'s pads are
/// dynamic: parse-launch would link whichever appeared first to whichever
/// queue, both of which accept anything.
fn link_recording(
    pipeline: &gst::Pipeline,
    job: &PreviewJob,
    camera: bool,
    by_name: &impl Fn(&str) -> gst::Element,
) -> Result<(), CompositeError> {
    let make = |factory: &str| {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|e| CompositeError::Failed(format!("{factory} is missing: {e}")))
    };
    let filesrc = make("filesrc")?;
    filesrc.set_property("location", &job.recording);
    let decodebin = make("decodebin3")?;
    pipeline
        .add_many([&filesrc, &decodebin])
        .expect("add the recording's elements");
    filesrc
        .link(&decodebin)
        .expect("link filesrc to decodebin3");

    let sink_pad =
        |element: gst::Element| element.static_pad("sink").expect("a queue has a sink pad");
    let video = camera.then(|| sink_pad(by_name("pipq")));
    let audio = sink_pad(by_name("audioq"));
    decodebin.connect_pad_added(move |_, pad| {
        let name = pad.name();
        let target = if name.starts_with("video_") {
            video.as_ref()
        } else if name.starts_with("audio_") {
            Some(&audio)
        } else {
            None
        };
        if let Some(target) = target.filter(|p| !p.is_linked()) {
            let _ = pad.link(target);
        }
    });
    Ok(())
}

/// Places the avatar's pad for each frame as its buffer arrives: `rect`, its
/// footprint at full size, scaled by that frame's pulse level.
///
/// **Keyed on the buffer's PTS, never set from the pushing thread**, which is
/// the rule every moving rect in the composite follows: with frames queued, a
/// direct property set lands up to `QUEUED` frames early (measured, Phase 8).
/// The same `pulsed` the export's `Schedule` uses, so the two tails place the
/// avatar identically at every level.
fn place_avatar(pad: &gst::Pad, rect: PadRect, levels: Arc<[f64]>) {
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        if let Some(pts) = info.buffer().and_then(|b| b.pts()) {
            place(pad, pulsed(rect, level_at(&levels, pts)), 2);
        }
        gst::PadProbeReturn::Ok
    });
}

/// Places the PiP pad once the recording's caps say the camera's shape.
///
/// It is chrome in **output** space — the coach never drew it, so nothing ties
/// it to the picture — and its height comes from the camera's display aspect,
/// so the inset is never stretched (`core::layout::pip_rect`). Both insets sit
/// at z 2, over the overlay whose bar they land on (`install_overlay_pad`).
fn place_pip(pad: &gst::Pad) {
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, |pad, info| {
        let Some(gst::PadProbeData::Event(event)) = &info.data else {
            return gst::PadProbeReturn::Ok;
        };
        let gst::EventView::Caps(caps) = event.view() else {
            return gst::PadProbeReturn::Ok;
        };
        let Ok(info) = gst_video::VideoInfo::from_caps(caps.caps()) else {
            return gst::PadProbeReturn::Ok;
        };
        // The mixer pad is the one place the sub-pixel layout is rounded.
        place(
            pad,
            rounded(pip_rect(
                f64::from(OUTPUT_WIDTH),
                f64::from(OUTPUT_HEIGHT),
                display_aspect(&info),
            )),
            2,
        );
        gst::PadProbeReturn::Remove
    });
}
