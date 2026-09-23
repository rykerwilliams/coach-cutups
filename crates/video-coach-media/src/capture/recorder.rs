//! The commentary recorder: the capture pipeline of spec R1, on the system
//! clock, with file time 0 at its `base_time` (R5).
//!
//! It is a second pipeline next to the `SourcePlayer`, and its messages reach
//! the owner only through `on_message`, never through the player's message
//! path (R1).
//!
//! The live self-view is a third pipeline, started with this one and fed
//! from the camera's caps without being part of it (see `self_view.rs`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;

use super::devices::{choose_encoder, Camera, Input};
use super::self_view;
use crate::error_text;
use crate::mailbox::FrameMailbox;

/// Test sources' frame size: small, so x264 stays cheap on CI, where tests run
/// in parallel.
const TEST_WIDTH: i32 = 320;
const TEST_HEIGHT: i32 = 180;
/// `level`'s posting interval: 100 ms. Public because it is the `dt` the live
/// pulse estimator is smoothed at (avatar spec D1) — the app derives its step
/// from this rather than restating the number.
pub const LEVEL_INTERVAL_NS: u64 = 100_000_000;
/// How much encoded audio the queue after `opusenc` holds. The mux holds audio
/// until the first video frame arrives, and a start gives up after 5 s
/// without one (R6). With no video pad the mux holds nothing, so the queue
/// never fills; it stays all the same, since removing it would mean two audio
/// branches.
const AUDIO_QUEUE_NS: u64 = 6_000_000_000;

/// Where a recording's picture and sound come from. No picture on either arm
/// is avatar mode (avatar spec C1): sound alone, and no camera is opened.
#[derive(Debug, Clone)]
pub enum CaptureSources {
    /// `v4l2src` on the camera's device path, and `pipewiresrc` on the mic's
    /// `node.name`, or PipeWire's default mic for `None`.
    Devices {
        camera: Option<Camera>,
        mic: Option<String>,
    },
    /// `videotestsrc` and `audiotestsrc`, live. Video buffers before the
    /// delay (running time) are dropped, as a camera warming up.
    Test { video: Option<Duration> },
}

/// What a running recorder reports, on GStreamer's threads.
#[derive(Debug, Clone, PartialEq)]
pub enum RecorderMessage {
    /// The first buffer reached the muxer, so there is a file worth keeping.
    /// Sent once, from the video pad where there is one and the audio pad
    /// otherwise.
    FirstBuffer,
    /// The loudest channel's peak and RMS over the last 100 ms, in dB
    /// (silence reads far below −60). The meter draws the peak; the avatar
    /// pulses on the RMS, since speech's crest factor would peg a peak-driven
    /// one (avatar spec D1).
    Level { peak_db: f64, rms_db: f64 },
    /// An `ERROR` on the pipeline.
    Error(String),
}

/// How a [`Recorder::stop`] went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StopOutcome {
    /// Seconds from t0 to the end of the latest buffer that reached the
    /// muxer: the file's duration after a clean EOS, and an estimate of what
    /// was written otherwise.
    pub duration: f64,
    /// EOS reached the pipeline's bus within the timeout.
    pub clean: bool,
}

/// A recording in progress. Dropping it sets the pipeline to NULL, which is
/// also how a start is aborted: the file is left for the caller to delete.
pub struct Recorder {
    pipeline: gst::Pipeline,
    t0_ns: u64,
    /// Running time, in ns, of the latest buffer end at any mux pad.
    last_end: Arc<AtomicU64>,
    /// The live self-view's pipeline, if it started.
    self_view: Option<gst::Pipeline>,
}

type OnMessage = Arc<dyn Fn(RecorderMessage) + Send + Sync>;

impl Recorder {
    /// Builds, sets PLAYING, reads t0 = base_time as soon as set_state returns
    /// (never waits for PLAYING: the mux holds preroll until the camera's
    /// first frame).
    /// On Err the caller deletes `path` (filesink has created it).
    ///
    /// The camera is shown live in `self_view` until the recording stops:
    /// small RGBA frames in system memory, `FrameMailbox`'s latest-wins
    /// handoff, from the camera's first frame, which may be before
    /// `FirstBuffer`. It can't disturb the recording: it only ever gets a
    /// reference to a camera frame, through a queue that drops rather than
    /// waits, in a pipeline of its own (`self_view.rs`). A self-view that
    /// fails, to start or later, is logged, and the recording goes ahead.
    /// With no camera there is no self-view pipeline at all: what the corner
    /// shows then is the app's business.
    ///
    /// `on_message` is called on GStreamer's threads.
    pub fn start(
        sources: CaptureSources,
        path: &Path,
        self_view: FrameMailbox,
        on_message: impl Fn(RecorderMessage) + Send + Sync + 'static,
    ) -> Result<Recorder, String> {
        let on_message: OnMessage = Arc::new(on_message);
        let last_end = Arc::new(AtomicU64::new(0));
        let (pipeline, self_view) = build(&sources, path, self_view, &on_message, &last_end)
            .map_err(|e| format!("could not build the recording pipeline: {e}"))?;

        let bus = pipeline.bus().expect("a pipeline has a bus");
        bus.set_sync_handler({
            let on_message = on_message.clone();
            move |_, msg| match msg.view() {
                gst::MessageView::Error(err) => {
                    on_message(RecorderMessage::Error(error_text(err)));
                    // Kept for `stop()` and `start()` to pop.
                    gst::BusSyncReply::Pass
                }
                gst::MessageView::Eos(_) => gst::BusSyncReply::Pass,
                gst::MessageView::Element(el) => {
                    if let Some((peak_db, rms_db)) = el.structure().and_then(level_dbs) {
                        on_message(RecorderMessage::Level { peak_db, rms_db });
                    }
                    gst::BusSyncReply::Drop
                }
                // Nothing else piles up on the bus, which only `stop()` pops.
                _ => gst::BusSyncReply::Drop,
            }
        });

        // Returns `Async`: the pipeline stays PAUSED with PLAYING pending
        // until every mux pad has data, which for video is the camera's first
        // frame. `base_time` is fixed by now all the same. With no video pad
        // it prerolls on the live mic and returns `NoPreroll` instead; both
        // are `Ok`, which is all the match below asks.
        let started = pipeline.set_state(gst::State::Playing);
        let t0 = pipeline.base_time();
        match (started, t0) {
            (Ok(_), Some(t0)) => {
                // CLOCK_MONOTONIC is never 0 by the time a pipeline runs; a 0
                // would mean the forced clock didn't take.
                debug_assert!(t0.nseconds() > 0);
                Ok(Recorder {
                    pipeline,
                    t0_ns: t0.nseconds(),
                    last_end,
                    self_view,
                })
            }
            (started, _) => {
                let _ = pipeline.set_state(gst::State::Null);
                if let Some(view) = &self_view {
                    let _ = view.set_state(gst::State::Null);
                }
                Err(match bus.pop_filtered(&[gst::MessageType::Error]) {
                    Some(msg) => match msg.view() {
                        gst::MessageView::Error(err) => error_text(err),
                        _ => unreachable!("filtered to errors"),
                    },
                    None if started.is_err() => "the recording pipeline failed to start".into(),
                    None => "the recording pipeline has no base time".into(),
                })
            }
        }
    }

    /// The recording's time 0 on the system clock (see [`super::now_ns`]),
    /// in ns: the file's time 0.
    pub fn t0_ns(&self) -> u64 {
        self.t0_ns
    }

    /// The pipeline. For tests only (they reach into it to simulate
    /// failures); not part of the API.
    #[doc(hidden)]
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    /// The self-view's pipeline, if it started. For tests only (they stall
    /// its consumer); not part of the API.
    #[doc(hidden)]
    pub fn self_view_pipeline(&self) -> Option<&gst::Pipeline> {
        self.self_view.as_ref()
    }

    /// EOS, wait ≤ timeout for EOS/ERROR on the pipeline's own bus, NULL.
    /// An ERROR already on the bus returns at once, unclean.
    pub fn stop(self, timeout: Duration) -> StopOutcome {
        self.pipeline.send_event(gst::event::Eos::new());
        let bus = self.pipeline.bus().expect("a pipeline has a bus");
        let msg = bus.timed_pop_filtered(
            gst::ClockTime::from_nseconds(timeout.as_nanos() as u64),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let clean = msg.is_some_and(|m| m.type_() == gst::MessageType::Eos);
        // NULL before reading `last_end`, so no streaming thread still moves
        // it after a timeout.
        let _ = self.pipeline.set_state(gst::State::Null);
        StopOutcome {
            duration: self.last_end.load(Ordering::SeqCst) as f64 / 1e9,
            clean,
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        // Second: the camera, now stopped, pushes into it no more. Bounded by
        // one frame's decode and scale, since nothing in it waits on anything
        // else, and a sink blocked by a test is unblocked by the flush.
        if let Some(view) = &self.self_view {
            let _ = view.set_state(gst::State::Null);
        }
    }
}

/// The R1 pipeline, in NULL, on the system clock, and its self-view, if that
/// started.
fn build(
    sources: &CaptureSources,
    path: &Path,
    self_view: FrameMailbox,
    on_message: &OnMessage,
    last_end: &Arc<AtomicU64>,
) -> Result<(gst::Pipeline, Option<gst::Pipeline>), glib::BoolError> {
    let pipeline = gst::Pipeline::new();
    // R5: t0 and every event's `host_ns` are on CLOCK_MONOTONIC. `pulsesrc`'s
    // clock was measured ~473,000 s off it.
    pipeline.use_clock(Some(&gst::SystemClock::obtain()));
    let make = |factory: &str| gst::ElementFactory::make(factory).build();

    let mux = gst::ElementFactory::make("matroskamux")
        // The default, set explicitly: R5 needs the file's time 0 to be
        // `base_time`, with the video-less lead-in kept, not shifted to the
        // earliest stream.
        .property("offset-to-zero", false)
        .build()?;
    let sink = gst::ElementFactory::make("filesink")
        .property("location", path.to_string_lossy().as_ref())
        .build()?;
    // Buffered, a `kill -9` left 0 bytes; unbuffered left a playable file.
    sink.set_property_from_str("buffer-mode", "unbuffered");
    pipeline.add_many([&mux, &sink])?;
    mux.link(&sink)?;

    let video = build_video(&pipeline, sources)?;

    // Audio: source ! caps ! queue ! convert ! resample ! level ! opus ! queue ! mux.
    let audio_src = match sources {
        CaptureSources::Devices { mic, .. } => {
            let src = make("pipewiresrc")?;
            if let Some(mic) = mic {
                src.set_property("target-object", mic);
            }
            src
        }
        CaptureSources::Test { .. } => {
            let src = gst::ElementFactory::make("audiotestsrc")
                .property("is-live", true)
                .build()?;
            src.set_property_from_str("wave", "ticks");
            src
        }
    };
    let audio_filter = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("audio/x-raw")
                .field("rate", 48_000)
                .field("channels", 2)
                .build(),
        )
        .build()?;
    let level = gst::ElementFactory::make("level")
        .property("interval", LEVEL_INTERVAL_NS)
        .build()?;
    let opus = gst::ElementFactory::make("opusenc")
        .property("bitrate", 96_000)
        .build()?;
    let audio_out = gst::ElementFactory::make("queue")
        .name("audio-out")
        .property("max-size-time", AUDIO_QUEUE_NS)
        .property("max-size-buffers", 0u32)
        .property("max-size-bytes", 0u32)
        .build()?;
    let audio = [
        &audio_src,
        &audio_filter,
        &make("queue")?,
        &make("audioconvert")?,
        &make("audioresample")?,
        &level,
        &opus,
        &audio_out,
    ];
    pipeline.add_many(audio)?;
    gst::Element::link_many(audio)?;

    // The video pad first where there is one, since `FirstBuffer` goes to the
    // head of this list: the one pad that exists in avatar mode is the audio
    // one, and the flag means what it always meant.
    let mut branches: Vec<(&gst::Element, &str)> = Vec::new();
    if let Some((queue, ..)) = &video {
        branches.push((queue, "video_%u"));
    }
    branches.push((&audio_out, "audio_%u"));
    for (nth, (queue, template)) in branches.into_iter().enumerate() {
        let pad = mux
            .request_pad_simple(template)
            .ok_or_else(|| glib::bool_error!("matroskamux has no {template} pad"))?;
        queue
            .static_pad("src")
            .expect("a queue has a src pad")
            .link(&pad)
            .map_err(|e| glib::bool_error!("linking to the mux: {e:?}"))?;
        track_end(
            &pad,
            last_end.clone(),
            (nth == 0).then(|| on_message.clone()),
        );
    }
    // Last, so nothing after it can fail and leave it running. A failure is
    // the self-view's alone. With no camera there is nothing to show.
    let self_view = video.and_then(|(_, camera, input)| {
        self_view::start(&camera, input, self_view)
            .inspect_err(|e| eprintln!("recorder: the self-view failed: {e}"))
            .ok()
    });
    Ok((pipeline, self_view))
}

/// The video branch — source ! caps ! queue ! encode chain ! h264parse !
/// queue — added to `pipeline` and linked. Returns the queue that feeds the
/// mux, the capsfilter's src pad the self-view taps, and the camera's input
/// format.
///
/// `None` in avatar mode (spec C1): no `v4l2src`, no encoder — so a machine
/// with neither VA-API nor `x264enc` still records commentary — and no
/// `video_%u` pad on the mux.
fn build_video(
    pipeline: &gst::Pipeline,
    sources: &CaptureSources,
) -> Result<Option<(gst::Element, gst::Pad, Input)>, glib::BoolError> {
    let make = |factory: &str| gst::ElementFactory::make(factory).build();
    let (video_src, input, width, height) = match sources {
        CaptureSources::Devices {
            camera: Some(camera),
            ..
        } => {
            let src = gst::ElementFactory::make("v4l2src")
                .property("device", &camera.v4l2_path)
                .build()?;
            // Without it the webcam fell to 7.5 fps in a dark room while its
            // caps still said 30/1. A camera without the control warns and
            // records anyway.
            src.set_property_from_str("extra-controls", "c,exposure_dynamic_framerate=0");
            (
                src,
                camera.mode.input,
                camera.mode.width,
                camera.mode.height,
            )
        }
        CaptureSources::Test {
            video: Some(video_delay),
        } => {
            let src = gst::ElementFactory::make("videotestsrc")
                .property("is-live", true)
                .build()?;
            src.set_property_from_str("pattern", "ball");
            drop_before(&src, *video_delay);
            (src, Input::Raw, TEST_WIDTH, TEST_HEIGHT)
        }
        CaptureSources::Devices { camera: None, .. } | CaptureSources::Test { video: None } => {
            return Ok(None)
        }
    };
    let video_caps = gst::Caps::builder(match input {
        Input::Mjpeg => "image/jpeg",
        Input::Raw => "video/x-raw",
    })
    .field("width", width)
    .field("height", height)
    .field("framerate", gst::Fraction::new(30, 1))
    .build();
    let video_filter = gst::ElementFactory::make("capsfilter")
        .property("caps", video_caps)
        .build()?;
    let video_in = make("queue")?;
    let parse = make("h264parse")?;
    let video_out = make("queue")?;
    pipeline.add_many([&video_src, &video_filter, &video_in, &parse, &video_out])?;
    gst::Element::link_many([&video_src, &video_filter, &video_in])?;
    let has = |f: &str| gst::ElementFactory::find(f).is_some();
    let (head, tail) = choose_encoder(has, input).build(input, pipeline.upcast_ref())?;
    gst::Element::link_many([&video_in, &head])?;
    gst::Element::link_many([&tail, &parse, &video_out])?;
    let camera = video_filter
        .static_pad("src")
        .expect("a capsfilter has a src pad");
    Ok(Some((video_out, camera, input)))
}

/// Drops `src`'s buffers with PTS before `delay`. A live `videotestsrc`'s PTS
/// is its running time, so the first video lands at exactly the delay.
fn drop_before(src: &gst::Element, delay: Duration) {
    if delay.is_zero() {
        return;
    }
    let delay = gst::ClockTime::from_nseconds(delay.as_nanos() as u64);
    src.static_pad("src")
        .expect("a source has a src pad")
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            match info.buffer().and_then(|b| b.pts()) {
                Some(pts) if pts < delay => gst::PadProbeReturn::Drop,
                _ => gst::PadProbeReturn::Ok,
            }
        });
}

/// Tracks the latest buffer end reaching mux pad `pad`, in running time from
/// its sticky SEGMENT, into `last_end`. With `first`, also sends `FirstBuffer`
/// once.
fn track_end(pad: &gst::Pad, last_end: Arc<AtomicU64>, first: Option<OnMessage>) {
    let sent = AtomicBool::new(false);
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let Some(buffer) = info.buffer() else {
            return gst::PadProbeReturn::Ok;
        };
        let segment = pad
            .sticky_event::<gst::event::Segment>(0)
            .and_then(|ev| ev.segment().clone().downcast::<gst::format::Time>().ok());
        let running = buffer
            .pts()
            .or(buffer.dts())
            .and_then(|ts| segment.and_then(|s| s.to_running_time(ts)));
        if let Some(running) = running {
            let end = running + buffer.duration().unwrap_or(gst::ClockTime::ZERO);
            last_end.fetch_max(end.nseconds(), Ordering::SeqCst);
        }
        if let Some(on_message) = &first {
            if !sent.swap(true, Ordering::SeqCst) {
                on_message(RecorderMessage::FirstBuffer);
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// The max over channels of a `level` message's `peak` and of its `rms`, or
/// `None` for any other element message. Both are `GValueArray`s.
fn level_dbs(s: &gst::StructureRef) -> Option<(f64, f64)> {
    if s.name() != "level" {
        return None;
    }
    Some((loudest(s, "peak")?, loudest(s, "rms")?))
}

/// The max over channels of one of `level`'s dB arrays.
fn loudest(s: &gst::StructureRef, field: &str) -> Option<f64> {
    s.get::<glib::ValueArray>(field)
        .ok()?
        .as_slice()
        .iter()
        .filter_map(|v| v.get::<f64>().ok())
        .reduce(f64::max)
}
