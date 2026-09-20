//! The preview tail (spec P1–P4): the composite on screen, in the same
//! [`FrameMailbox`] the source player fills, paced by the commentary's audio
//! clock.
//!
//! ```text
//! pump:  appsrc name=src -- the source frame, GL memory, PTS n/30
//!        appsrc name=ov  -- its overlay, output-size RGBA, the SAME PTS
//! rec:   filesrc ! decodebin3 -- video to the PiP pad (only with show_pip),
//!                                audio to volume ! autoaudiosink
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
//! **One pump, both appsrcs, one PTS.** `glvideomixer` waits indefinitely on
//! every pad, so frame `n`'s overlay goes out with frame `n` or the mixer
//! starves. For the same reason the PiP pad is requested **only** when
//! `show_pip` is on; the recording's video pad is then left unlinked, which
//! `decodebin3` tolerates without stalling its branch (measured).
//!
//! **The audio sink is the clock** (measured: `GstPulseSinkClock`), so the
//! composite follows the commentary — the track the coach hears. The video
//! appsink syncs to that clock, and its backpressure is the whole of the
//! pump's pacing: there is no sleep loop, and PAUSED stops the pump through
//! that same backpressure (measured), so pausing is a state change and
//! nothing else.
//!
//! **Transport runs through the pipeline, not around it** (spec P3). A seek is
//! one pipeline seek: the recording branch seeks natively, and both appsrcs,
//! being `stream-type=seekable`, answer `seek-data` by moving the pump's
//! [`Cursor`]. Everything the owner steers — state, seeks, volume — goes
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
use video_coach_core::export::{FrameSpec, OUTPUT_FPS};
use video_coach_core::layout::pip_rect;
use video_coach_core::project::Clip;

use super::decode::Decoder;
use super::{
    display_aspect, fit_rect, frame_index, frame_time, head, install_zoom, place, stamp,
    stamp_buffer, wait_for_room, CompositeError, Gl, Schedule, Stopper, Watch, POLL, QUEUED,
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
    /// The clip's frame schedule (`video_coach_core::export::frame_schedule`).
    pub frames: Vec<FrameSpec>,
    /// The commentary's volume, the project's `preview_commentary_volume`, in
    /// the volume slider's `0..=1` space.
    pub commentary_volume: f64,
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
/// leads the picture by whatever is queued (at most [`QUEUED`] frames,
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
        debug_assert!(!job.frames.is_empty(), "a preview needs frames");
        let cancel = Arc::new(AtomicBool::new(false));
        let frames = job.frames.len() as u64;
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
    let total = job.frames.len() as u64;
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
        let frame = &job.frames[n as usize];
        let sample = decoder.frame_at(seconds_to_clock(frame.source_time), watch)?;
        // The first frame's caps shape the composite: its size, PAR and
        // memory, and with them the picture rect the overlay is drawn at.
        if composite.is_none() {
            composite = Some(Composite::start(sample, job, gl, mailbox, shared, watch)?);
        }
        let composite = composite.as_ref().expect("started above");
        composite.push(n, generation, sample, &job.clip, &mut overlays, watch)?;
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

/// The composite pipeline: three mixer pads and the tail into the mailbox.
struct Composite {
    pipeline: Stopper,
    /// The pumped source frames.
    src: gst_app::AppSrc,
    /// Their overlays, rasterized at the **output** size with the strokes
    /// mapped into the picture rect (spec E2).
    overlay: gst_app::AppSrc,
    /// The picture rect the strokes are mapped into, `(x, y, w, h)`.
    picture: (i32, i32, i32, i32),
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
        gl: &Gl,
        mailbox: &FrameMailbox,
        shared: &Arc<Shared>,
        watch: &Watch,
    ) -> Result<Composite, CompositeError> {
        let caps = first
            .caps()
            .ok_or_else(|| CompositeError::Failed("a decoded frame has no caps".into()))?;
        let info = gst_video::VideoInfo::from_caps(caps)
            .map_err(|e| CompositeError::Failed(format!("unusable decoded caps {caps}: {e}")))?;
        let picture = fit_rect(&info, OUTPUT_WIDTH, OUTPUT_HEIGHT);

        // The PiP pad is requested only with `show_pip`, and the recording's
        // video pad is then left unlinked.
        let pip = if job.clip.show_pip {
            "queue name=pipq ! glupload ! glcolorconvert ! mix.sink_1 "
        } else {
            ""
        };
        // The overlay branch is RGBA end to end. GStreamer's `RGBA` means
        // *straight* alpha and `OverlayRenderer` hands over premultiplied
        // pixels; nothing here demultiplies them, because the mixer pad's
        // `blend-function-src-rgb=one` (set below) is premultiplied-over for
        // free on the GPU.
        let description = format!(
            "{head} ! glcolorconvert \
             ! appsink name=out sync=true qos=true max-buffers=1 enable-last-sample=false \
             appsrc name=ov format=time is-live=false block=false \
               max-buffers={QUEUED} max-bytes=0 max-time=0 \
               caps=video/x-raw,format=RGBA,width={OUTPUT_WIDTH},height={OUTPUT_HEIGHT},\
                 framerate={OUTPUT_FPS}/1 \
             ! glupload ! glcolorconvert \
             ! video/x-raw(memory:GLMemory),format=RGBA ! mix.sink_2 \
             {pip}\
             queue name=audioq ! audioconvert ! audioresample \
             ! volume name=vol ! autoaudiosink",
            head = head(OUTPUT_WIDTH, OUTPUT_HEIGHT),
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
        // Both are seekable, and answer a pipeline seek by moving the pump
        // (spec P3). `format=time`, so `seek-data`'s offset is nanoseconds on
        // the output timeline.
        for appsrc in [&src, &overlay] {
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
        let overlay_pad = mix_pad("sink_2");
        place(&overlay_pad, (0, 0, OUTPUT_WIDTH, OUTPUT_HEIGHT), 2);
        overlay_pad.set_property_from_str("blend-function-src-rgb", "one");
        // The two pumped pads are sent EOS at the end of the schedule (see
        // `Composite::end`), and an EOS pad is otherwise not drawn at all:
        // with the recording still running on pad 1, the freeze would be on
        // a black frame (measured -- the composite test caught one).
        for pad in [&base_pad, &overlay_pad] {
            pad.set_property("repeat-after-eos", true);
        }
        if job.clip.show_pip {
            place_pip(&mix_pad("sink_1"));
        }
        // One entry, laid out once above rather than per entry, so the
        // schedule here carries only the zoom.
        install_zoom(&by_name("zoom"), &Schedule::new(job.frames.clone(), 1));

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

        link_recording(&pipeline, job, &by_name)?;
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
            picture,
            shared: shared.clone(),
        })
    }

    /// Pushes output frame `n`: `sample`'s texture on the base pad and the
    /// clip's overlay at `n/30` on the overlay pad, **both stamped `n/30`**.
    /// A seek that landed since `generation` was read discards both.
    ///
    /// The base is a buffer reference, not a pixel copy: a freeze sends the
    /// same texture out many times.
    fn push(
        &self,
        n: u64,
        generation: u64,
        sample: &gst::Sample,
        clip: &Clip,
        overlays: &mut OverlayRenderer,
        watch: &Watch,
    ) -> Result<(), CompositeError> {
        let base = stamp(sample, n);
        // Record time is output time (see the module docs), so the overlay's
        // moment is the output frame's own, and its stamp the frame's own.
        // The bar's line is empty until Phase 8's Task 5, which draws
        // `1 / 1` here.
        let mut overlay = overlays.render(
            &OverlayFrame {
                clip,
                record_time: n as f64 / f64::from(OUTPUT_FPS),
                picture: self.picture,
                text: "",
            },
            OUTPUT_WIDTH as u32,
            OUTPUT_HEIGHT as u32,
        );
        stamp_buffer(&mut overlay, n);

        // Room for both first, so the cursor's lock is never held across a
        // wait: `seek-data` runs on the seeking thread, which must not queue
        // behind the pump. PAUSED is what makes this wait the pump's pause.
        wait_for_room(&self.src, watch)?;
        wait_for_room(&self.overlay, watch)?;
        let mut cursor = self.shared.cursor();
        if cursor.generation != generation {
            // A seek landed while this frame was being prepared. See `Cursor`.
            return Ok(());
        }
        // Both appsrcs have room, so neither push waits under the lock.
        let push = |appsrc: &gst_app::AppSrc, buffer: gst::Buffer, what: &str| {
            appsrc
                .push_buffer(buffer)
                .map_err(|e| watch.failure(format!("pushing {what}: {e:?}")))
        };
        push(&self.src, base, &format!("frame {n}"))?;
        push(&self.overlay, overlay, &format!("overlay {n}"))?;
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
    /// **Both appsrcs go EOS first,** which is what tells the aggregator those
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

/// Adds `filesrc ! decodebin3` for the recording and links its streams: video
/// to the PiP queue when there is one, audio to the volume chain.
///
/// In Rust rather than the launch string because `decodebin3`'s pads are
/// dynamic: parse-launch would link whichever appeared first to whichever
/// queue, both of which accept anything.
fn link_recording(
    pipeline: &gst::Pipeline,
    job: &PreviewJob,
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
    let video = job.clip.show_pip.then(|| sink_pad(by_name("pipq")));
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

/// Places the PiP pad once the recording's caps say the camera's shape.
///
/// It is chrome in **output** space — the coach never drew it, so nothing ties
/// it to the picture — and its height comes from the camera's display aspect,
/// so the inset is never stretched (`core::layout::pip_rect`).
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
        let rect = pip_rect(
            f64::from(OUTPUT_WIDTH),
            f64::from(OUTPUT_HEIGHT),
            display_aspect(&info),
        );
        // The mixer pad is the one place the sub-pixel layout is rounded.
        place(
            pad,
            (
                rect.x.round() as i32,
                rect.y.round() as i32,
                rect.w.round() as i32,
                rect.h.round() as i32,
            ),
            1,
        );
        gst::PadProbeReturn::Remove
    });
}
