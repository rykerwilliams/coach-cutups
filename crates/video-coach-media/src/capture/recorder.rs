//! The commentary recorder: the capture pipeline of spec R1, on the system
//! clock, with file time 0 at its `base_time` (R5).
//!
//! It is a second pipeline next to the `SourcePlayer`, and its messages reach
//! the owner only through `on_message`, tagged with a generation, never
//! through the player's message path (R1).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;

use super::devices::{choose_encoder, Camera, Input};

/// Test sources' frame size: small, so x264 stays cheap on CI, where tests run
/// in parallel.
const TEST_WIDTH: i32 = 320;
const TEST_HEIGHT: i32 = 180;
/// `level`'s posting interval: 100 ms.
const LEVEL_INTERVAL_NS: u64 = 100_000_000;
/// How much encoded audio the queue after `opusenc` holds. The mux holds audio
/// until the first video frame arrives, and a start gives up after 5 s
/// without one (R6).
const AUDIO_QUEUE_NS: u64 = 6_000_000_000;

/// Where a recording's picture and sound come from.
#[derive(Debug, Clone)]
pub enum CaptureSources {
    /// `v4l2src` on the camera's device path, and `pipewiresrc` on the mic's
    /// `node.name`, or PipeWire's default mic for `None`.
    Devices { camera: Camera, mic: Option<String> },
    /// `videotestsrc` and `audiotestsrc`, live. Video buffers before
    /// `video_delay` (running time) are dropped, as a camera warming up.
    Test { video_delay: Duration },
}

/// What a running recorder reports, on GStreamer's threads.
#[derive(Debug, Clone, PartialEq)]
pub enum RecorderMessage {
    /// The first video buffer reached the muxer. Sent once.
    FirstVideo,
    /// The loudest channel's peak over the last 100 ms, in dB (silence reads
    /// far below −60).
    Level { peak_db: f64 },
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
}

type OnMessage = Arc<dyn Fn(u64, RecorderMessage) + Send + Sync>;

impl Recorder {
    /// Builds, sets PLAYING, reads t0 = base_time as soon as set_state returns
    /// (never waits for PLAYING: the mux holds preroll until the camera's
    /// first frame).
    /// On Err the caller deletes `path` (filesink has created it).
    ///
    /// `on_message` is called on GStreamer's threads with `generation`.
    pub fn start(
        sources: CaptureSources,
        path: &Path,
        generation: u64,
        on_message: impl Fn(u64, RecorderMessage) + Send + Sync + 'static,
    ) -> Result<Recorder, String> {
        let on_message: OnMessage = Arc::new(on_message);
        let last_end = Arc::new(AtomicU64::new(0));
        let pipeline = build(&sources, path, generation, &on_message, &last_end)
            .map_err(|e| format!("could not build the recording pipeline: {e}"))?;

        let bus = pipeline.bus().expect("a pipeline has a bus");
        bus.set_sync_handler({
            let on_message = on_message.clone();
            move |_, msg| match msg.view() {
                gst::MessageView::Error(err) => {
                    on_message(generation, RecorderMessage::Error(error_text(err)));
                    // Kept for `stop()` and `start()` to pop.
                    gst::BusSyncReply::Pass
                }
                gst::MessageView::Eos(_) => gst::BusSyncReply::Pass,
                gst::MessageView::Element(el) => {
                    if let Some(peak_db) = el.structure().and_then(level_peak) {
                        on_message(generation, RecorderMessage::Level { peak_db });
                    }
                    gst::BusSyncReply::Drop
                }
                // Nothing else piles up on the bus, which only `stop()` pops.
                _ => gst::BusSyncReply::Drop,
            }
        });

        // Returns `Async`: the pipeline stays PAUSED with PLAYING pending
        // until every mux pad has data, which for video is the camera's first
        // frame. `base_time` is fixed by now all the same.
        let started = pipeline.set_state(gst::State::Playing);
        let t0 = pipeline.base_time();
        match (started, t0) {
            (Ok(_), Some(t0)) => Ok(Recorder {
                pipeline,
                t0_ns: t0.nseconds(),
                last_end,
            }),
            (started, _) => {
                let _ = pipeline.set_state(gst::State::Null);
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

    /// The pipeline, for tests and diagnostics.
    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
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
    }
}

/// The R1 pipeline, in NULL, on the system clock.
fn build(
    sources: &CaptureSources,
    path: &Path,
    generation: u64,
    on_message: &OnMessage,
    last_end: &Arc<AtomicU64>,
) -> Result<gst::Pipeline, glib::BoolError> {
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

    // Video: source ! caps ! queue ! encode chain ! h264parse ! queue ! mux.
    let (video_src, input, width, height) = match sources {
        CaptureSources::Devices { camera, .. } => {
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
        CaptureSources::Test { video_delay } => {
            let src = gst::ElementFactory::make("videotestsrc")
                .property("is-live", true)
                .build()?;
            src.set_property_from_str("pattern", "ball");
            drop_before(&src, *video_delay);
            (src, Input::Raw, TEST_WIDTH, TEST_HEIGHT)
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

    for (queue, template, first_video) in [
        (&video_out, "video_%u", true),
        (&audio_out, "audio_%u", false),
    ] {
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
            first_video.then(|| (generation, on_message.clone())),
        );
    }
    Ok(pipeline)
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
/// its sticky SEGMENT, into `last_end`. With `first`, also sends `FirstVideo`
/// once.
fn track_end(pad: &gst::Pad, last_end: Arc<AtomicU64>, first: Option<(u64, OnMessage)>) {
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
        if let Some((generation, on_message)) = &first {
            if !sent.swap(true, Ordering::SeqCst) {
                on_message(*generation, RecorderMessage::FirstVideo);
            }
        }
        gst::PadProbeReturn::Ok
    });
}

/// The max over channels of a `level` message's `peak`, or `None` for any
/// other element message. `peak` is a `GValueArray`.
fn level_peak(s: &gst::StructureRef) -> Option<f64> {
    if s.name() != "level" {
        return None;
    }
    let peaks = s.get::<glib::ValueArray>("peak").ok()?;
    peaks
        .as_slice()
        .iter()
        .filter_map(|v| v.get::<f64>().ok())
        .reduce(f64::max)
}

fn error_text(err: &gst::message::Error) -> String {
    let from = err
        .src()
        .map(|s| format!("{}: ", s.name()))
        .unwrap_or_default();
    match err.debug() {
        Some(debug) => format!("{from}{} ({debug})", err.error()),
        None => format!("{from}{}", err.error()),
    }
}
