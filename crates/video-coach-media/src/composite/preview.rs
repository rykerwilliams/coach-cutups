//! The preview tail (spec P1–P4): the composite on screen, in the same
//! [`FrameMailbox`] the source player fills, paced by the commentary's audio
//! clock.
//!
//! ```text
//! pump:  appsrc name=src -- the source frame, GL memory, PTS n/30
//!        appsrc name=ov  -- its overlay, RGBA, the SAME PTS
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
//! pump's pacing: there is no sleep loop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use video_coach_core::export::{FrameSpec, OUTPUT_FPS};
use video_coach_core::layout::pip_rect;
use video_coach_core::project::Clip;
use video_coach_core::zoom::Zoom;

use super::decode::Decoder;
use super::{
    display_aspect, fit_rect, frame_time, head, install_zoom, place, push_buffer, CompositeError,
    Gl, Stopper, Watch, POLL, QUEUED,
};
use crate::mailbox::{Frame, FrameMailbox};
use crate::player::{gl_caps, seconds_to_clock};
use crate::{now_ns, render_overlay};

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
    /// Frames the sink reported dropping, from its QoS messages.
    pub dropped: u64,
    /// Their steady-state rate, from the first sample to the last.
    pub fps: f64,
}

/// A running preview. It owns its thread, and every GStreamer object it
/// creates lives and dies on that thread. Dropping it closes and joins.
pub struct Preview {
    cancel: Arc<AtomicBool>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<()>>,
}

impl Preview {
    /// Starts previewing `job` into `mailbox`, compositing on `gl` — Slint's
    /// display and context in the app, [`Gl::shared`] with no UI (spec P1:
    /// "no private GL context" is an app rule, not a test rule).
    ///
    /// `on_message` is called on the preview thread. `job` must have frames:
    /// the bus refuses an empty clip.
    pub fn start(
        job: PreviewJob,
        gl: Gl,
        mailbox: FrameMailbox,
        mut on_message: impl FnMut(PreviewMessage) + Send + 'static,
    ) -> Preview {
        debug_assert!(!job.frames.is_empty(), "a preview needs frames");
        let cancel = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let thread = std::thread::Builder::new()
            .name("preview".into())
            .spawn({
                let (cancel, counters) = (cancel.clone(), counters.clone());
                move || {
                    let watch = Watch {
                        cancel: &cancel,
                        error: Arc::default(),
                    };
                    // `Cancelled` is the close the user asked for, and has
                    // nothing to report. Anything else is the graph giving up.
                    if let Err(CompositeError::Failed(e)) =
                        run(&job, &gl, &mailbox, &counters, &watch, &mut on_message)
                    {
                        on_message(PreviewMessage::Failed(e));
                    }
                }
            })
            .expect("spawn the preview thread");
        Preview {
            cancel,
            counters,
            thread: Some(thread),
        }
    }

    /// What the sink has seen so far.
    pub fn stats(&self) -> PreviewStats {
        self.counters.stats()
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

/// Runs the preview until it is closed or fails, reporting
/// [`PreviewMessage::Ended`] when the schedule runs out. It returns only once
/// the preview is over: the pipeline lives on this thread, so the thread has
/// to outlast the picture it is holding.
fn run(
    job: &PreviewJob,
    gl: &Gl,
    mailbox: &FrameMailbox,
    counters: &Arc<Counters>,
    watch: &Watch,
    on_message: &mut impl FnMut(PreviewMessage),
) -> Result<(), CompositeError> {
    let mut decoder = Decoder::start(&job.source, gl, watch)?;
    let mut composite = None;
    for (n, frame) in job.frames.iter().enumerate() {
        let sample = decoder.frame_at(seconds_to_clock(frame.source_time), watch)?;
        // The first frame's caps shape the composite: its size, PAR and
        // memory, and with them the picture rect the overlay is drawn at.
        if composite.is_none() {
            let zooms = job.frames.iter().map(|f| f.zoom).collect();
            composite = Some(Composite::start(
                sample, job, zooms, gl, mailbox, counters, watch,
            )?);
        }
        let composite = composite.as_ref().expect("started above");
        composite.push(n as u64, sample, &job.clip, watch)?;
    }
    let composite =
        composite.ok_or_else(|| CompositeError::Failed("the clip has no frames".into()))?;
    // Freeze on the last frame rather than running on into the recording's
    // tail (spec P3). The sink accounts for every frame it was given as
    // rendered or, when it fell behind the audio clock, dropped, so the pair
    // reaching the schedule's length is the end of the picture. PAUSED then
    // stops the mixer where it stands.
    composite.await_end(job.frames.len() as u64, watch)?;
    composite.pause(watch)?;
    on_message(PreviewMessage::Ended);
    loop {
        watch.check()?;
        std::thread::sleep(POLL.into());
    }
}

/// What the sink counts, shared with [`Preview::stats`].
#[derive(Default)]
struct Counters {
    composited: AtomicU64,
    dropped: AtomicU64,
    /// `now_ns()` at the first sample and at the latest, for the rate.
    first_ns: AtomicU64,
    last_ns: AtomicU64,
}

impl Counters {
    fn sample(&self) {
        let now = now_ns();
        let _ = self
            .first_ns
            .compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst);
        self.last_ns.store(now, Ordering::SeqCst);
        self.composited.fetch_add(1, Ordering::SeqCst);
    }

    fn stats(&self) -> PreviewStats {
        let composited = self.composited.load(Ordering::SeqCst);
        let span = self
            .last_ns
            .load(Ordering::SeqCst)
            .saturating_sub(self.first_ns.load(Ordering::SeqCst));
        PreviewStats {
            composited,
            dropped: self.dropped.load(Ordering::SeqCst),
            // The first sample starts the clock, so it is the gaps between
            // frames that are counted, not the frames.
            fps: match (composited, span) {
                (2.., 1..) => (composited - 1) as f64 * 1e9 / span as f64,
                _ => 0.0,
            },
        }
    }
}

/// The composite pipeline: three mixer pads and the tail into the mailbox.
struct Composite {
    pipeline: Stopper,
    /// The pumped source frames.
    src: gst_app::AppSrc,
    /// Their overlays, rasterized at the picture rect.
    overlay: gst_app::AppSrc,
    /// That rect's size.
    picture: (u32, u32),
    counters: Arc<Counters>,
}

impl Composite {
    /// Builds the graph for source frames shaped like `first` and sets it
    /// PLAYING. Output frame `n` gets `zooms[n]`. Its errors reach `watch`.
    ///
    /// **Never waits for PLAYING:** the graph can't preroll until the pump
    /// pushes, and the pump is the caller.
    fn start(
        first: &gst::Sample,
        job: &PreviewJob,
        zooms: Vec<Zoom>,
        gl: &Gl,
        mailbox: &FrameMailbox,
        counters: &Arc<Counters>,
        watch: &Watch,
    ) -> Result<Composite, CompositeError> {
        let caps = first
            .caps()
            .ok_or_else(|| CompositeError::Failed("a decoded frame has no caps".into()))?;
        let info = gst_video::VideoInfo::from_caps(caps)
            .map_err(|e| CompositeError::Failed(format!("unusable decoded caps {caps}: {e}")))?;
        let picture = fit_rect(&info, OUTPUT_WIDTH, OUTPUT_HEIGHT);
        let (pw, ph) = (picture.2, picture.3);

        // The PiP pad is requested only with `show_pip`, and the recording's
        // video pad is then left unlinked.
        let pip = if job.clip.show_pip {
            "queue name=pipq ! glupload ! glcolorconvert ! mix.sink_1 "
        } else {
            ""
        };
        // The overlay branch is RGBA end to end. GStreamer's `RGBA` means
        // *straight* alpha and `render_overlay` hands over premultiplied
        // pixels; nothing here demultiplies them, because the mixer pad's
        // `blend-function-src-rgb=one` (set below) is premultiplied-over for
        // free on the GPU.
        let description = format!(
            "{head} ! glcolorconvert \
             ! appsink name=out sync=true qos=true max-buffers=1 enable-last-sample=false \
             appsrc name=ov format=time is-live=false block=false \
               max-buffers={QUEUED} max-bytes=0 max-time=0 \
               caps=video/x-raw,format=RGBA,width={pw},height={ph},framerate={OUTPUT_FPS}/1 \
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

        let mix = by_name("mix");
        let mix_pad = |name: &str| {
            mix.static_pad(name)
                .expect("requested in the launch string")
        };
        // Base and overlay share the picture rect, so a drawing lands on the
        // picture and not across the letterbox bars (spec P4). The PiP is
        // chrome in output space and waits for the camera's shape.
        place(&mix_pad("sink_0"), picture, 0);
        let overlay_pad = mix_pad("sink_2");
        place(&overlay_pad, picture, 2);
        overlay_pad.set_property_from_str("blend-function-src-rgb", "one");
        if job.clip.show_pip {
            place_pip(&mix_pad("sink_1"));
        }
        install_zoom(&by_name("zoom"), zooms);

        let out = by_name("out")
            .downcast::<gst_app::AppSink>()
            .expect("`out` is an appsink");
        out.set_caps(Some(&gl_caps()));
        out.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample({
                    let (mailbox, counters) = (mailbox.clone(), counters.clone());
                    move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                        mailbox.put(Frame::from_sample(sample)?);
                        counters.sample();
                        Ok(gst::FlowSuccess::Ok)
                    }
                })
                .build(),
        );

        link_recording(&pipeline, job, &by_name)?;
        // The sink's QoS reports are the only place a dropped frame shows up
        // -- and it does drop, so `qos=true` on the sink is load-bearing: the
        // audio is the clock, and a late picture kept would slide further and
        // further behind the words it belongs to.
        gl.install(&pipeline, watch, {
            let counters = counters.clone();
            move |msg| {
                if let gst::MessageView::Qos(qos) = msg.view() {
                    // `(processed, dropped)`, both cumulative for the sink.
                    let dropped = qos.stats().1.value().max(0) as u64;
                    counters.dropped.store(dropped, Ordering::SeqCst);
                }
            }
        });
        let pipeline = Stopper(pipeline);
        if pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure("could not start the preview"));
        }
        Ok(Composite {
            pipeline,
            src,
            overlay,
            picture: (pw as u32, ph as u32),
            counters: counters.clone(),
        })
    }

    /// Pushes output frame `n`: `sample`'s texture on the base pad and the
    /// clip's overlay at `n/30` on the overlay pad, **both stamped `n/30`**.
    ///
    /// The base is a buffer reference, not a pixel copy: a freeze sends the
    /// same texture out many times.
    fn push(
        &self,
        n: u64,
        sample: &gst::Sample,
        clip: &Clip,
        watch: &Watch,
    ) -> Result<(), CompositeError> {
        let (pts, duration) = (frame_time(n), frame_time(n + 1) - frame_time(n));
        let stamp = |buffer: &mut gst::Buffer| {
            let buffer = buffer.get_mut().expect("a buffer of our own is writable");
            buffer.set_pts(pts);
            buffer.set_dts(gst::ClockTime::NONE);
            buffer.set_duration(duration);
        };
        let mut base = sample
            .buffer()
            .expect("the decoder keeps only samples with a buffer")
            .copy();
        stamp(&mut base);
        // Record time is output time (see the module docs), so the overlay's
        // moment is the output frame's own.
        let (w, h) = self.picture;
        let mut overlay = render_overlay(clip, n as f64 / f64::from(OUTPUT_FPS), w, h);
        stamp(&mut overlay);
        push_buffer(&self.src, base, &format!("frame {n}"), watch)?;
        push_buffer(&self.overlay, overlay, &format!("overlay {n}"), watch)
    }

    /// Waits until the sink has accounted for `n` frames.
    fn await_end(&self, n: u64, watch: &Watch) -> Result<(), CompositeError> {
        let deadline = Instant::now() + STALL;
        loop {
            watch.check()?;
            let counters = &self.counters;
            let seen = counters.composited.load(Ordering::SeqCst)
                + counters.dropped.load(Ordering::SeqCst);
            if seen >= n {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(CompositeError::Failed(
                    "the preview stopped composing frames".into(),
                ));
            }
            std::thread::sleep(POLL.into());
        }
    }

    /// Holds the picture where it is, and with it the recording.
    fn pause(&self, watch: &Watch) -> Result<(), CompositeError> {
        match self.pipeline.set_state(gst::State::Paused) {
            Ok(_) => Ok(()),
            Err(_) => Err(watch.failure("could not pause the preview")),
        }
    }
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
