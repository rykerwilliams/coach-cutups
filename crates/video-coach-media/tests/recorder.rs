//! The recorder end to end with test sources: no camera, microphone or
//! display. The encoder is chosen as in production: VA where it exists, x264
//! on CI.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_pbutils as pbutils;
use gstreamer_pbutils::prelude::*;
use gstreamer_video as gst_video;
use video_coach_media::{now_ns, CaptureSources, FrameMailbox, Recorder, RecorderMessage};

const FRAME: f64 = 1.0 / 30.0;

struct Recording {
    recorder: Recorder,
    /// Each message with `now_ns()` when it was sent.
    messages: mpsc::Receiver<(u64, RecorderMessage)>,
    path: PathBuf,
    /// How long `Recorder::start` took.
    started_in: Duration,
    /// Where the self-view's frames arrive.
    self_view: FrameMailbox,
}

fn start(dir: &Path, video_delay: Duration) -> Recording {
    gst::init().unwrap();
    let path = dir.join("rec.mkv");
    let (tx, messages) = mpsc::channel();
    let tx = Mutex::new(tx);
    let self_view = FrameMailbox::default();
    let begun = Instant::now();
    let recorder = Recorder::start(
        CaptureSources::Test { video_delay },
        &path,
        self_view.clone(),
        move |msg| {
            let _ = tx.lock().unwrap().send((now_ns(), msg));
        },
    )
    .unwrap();
    Recording {
        recorder,
        messages,
        path,
        started_in: begun.elapsed(),
        self_view,
    }
}

/// Waits up to `timeout` for a message matching `want`, skipping others.
fn wait_for(
    rx: &mpsc::Receiver<(u64, RecorderMessage)>,
    timeout: Duration,
    want: impl Fn(&RecorderMessage) -> bool,
) -> Option<(u64, RecorderMessage)> {
    let deadline = Instant::now() + timeout;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok((at, msg)) if want(&msg) => return Some((at, msg)),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Every video and every audio PTS in the file, in seconds, in order, read
/// by demuxing it.
fn pts(path: &Path) -> (Vec<f64>, Vec<f64>) {
    let pipeline = gst::parse::launch(&format!(
        "filesrc location={} ! matroskademux name=demux",
        path.display()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let pts: Arc<Mutex<(Vec<f64>, Vec<f64>)>> = Arc::default();
    let demux = pipeline.by_name("demux").unwrap();
    demux.connect_pad_added({
        let pipeline = pipeline.downgrade();
        let pts = pts.clone();
        move |_, pad| {
            let pipeline = pipeline.upgrade().unwrap();
            let sink = gst::ElementFactory::make("fakesink")
                .property("sync", false)
                // One demux thread feeds both sinks: a sink blocking in
                // preroll would starve the other.
                .property("async", false)
                .build()
                .unwrap();
            pipeline.add(&sink).unwrap();
            sink.sync_state_with_parent().unwrap();
            pad.link(&sink.static_pad("sink").unwrap()).unwrap();
            let video = pad.name().starts_with("video");
            let pts = pts.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(t) = info.buffer().and_then(|b| b.pts()) {
                    let mut pts = pts.lock().unwrap();
                    let list = if video { &mut pts.0 } else { &mut pts.1 };
                    list.push(t.seconds_f64());
                }
                gst::PadProbeReturn::Ok
            });
        }
    });
    pipeline.set_state(gst::State::Playing).unwrap();
    let msg = pipeline.bus().unwrap().timed_pop_filtered(
        gst::ClockTime::from_seconds(10),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    pipeline.set_state(gst::State::Null).unwrap();
    assert_eq!(msg.map(|m| m.type_()), Some(gst::MessageType::Eos));
    let (mut video, mut audio) = std::mem::take(&mut *pts.lock().unwrap());
    // Decode order is not display order once B-frames are in.
    video.sort_by(f64::total_cmp);
    audio.sort_by(f64::total_cmp);
    (video, audio)
}

#[test]
fn records_h264_and_opus_with_the_file_duration() {
    let dir = tempfile::tempdir().unwrap();
    let rec = start(dir.path(), Duration::ZERO);
    std::thread::sleep(Duration::from_secs(2));
    let outcome = rec.recorder.stop(Duration::from_secs(5));
    assert!(outcome.clean);

    let uri = gst::glib::filename_to_uri(&rec.path, None).unwrap();
    let info = pbutils::Discoverer::new(gst::ClockTime::from_seconds(10))
        .unwrap()
        .discover_uri(&uri)
        .unwrap();
    let caps = |s: &pbutils::DiscovererStreamInfo| {
        s.caps().unwrap().structure(0).unwrap().name().to_string()
    };
    let video: Vec<_> = info
        .video_streams()
        .iter()
        .map(|s| caps(s.upcast_ref()))
        .collect();
    let audio: Vec<_> = info
        .audio_streams()
        .iter()
        .map(|s| caps(s.upcast_ref()))
        .collect();
    assert_eq!(video, ["video/x-h264"]);
    assert_eq!(audio, ["audio/x-opus"]);

    let discovered = info.duration().unwrap().seconds_f64();
    assert!(
        (outcome.duration - discovered).abs() <= FRAME,
        "stop said {} s, Discoverer {discovered} s",
        outcome.duration
    );
    assert!(outcome.duration > 1.5, "{} s", outcome.duration);
}

/// A camera that warms up for 0.5 s: `start` returns at once, `FirstVideo`
/// comes 0.5 s after t0, and in the file audio starts at time 0 and video at
/// 0.5 s, since file time 0 is `base_time` (R5).
#[test]
fn delayed_video_starts_at_its_running_time() {
    let dir = tempfile::tempdir().unwrap();
    let rec = start(dir.path(), Duration::from_millis(500));
    assert!(
        rec.started_in < Duration::from_millis(200),
        "start took {:?}: it waited for PLAYING",
        rec.started_in
    );
    let (first_ns, _) = wait_for(&rec.messages, Duration::from_secs(3), |m| {
        *m == RecorderMessage::FirstVideo
    })
    .expect("no FirstVideo");
    let first_at = (first_ns - rec.recorder.t0_ns()) as f64 / 1e9;
    assert!((first_at - 0.5).abs() <= 0.1, "FirstVideo at {first_at} s");
    std::thread::sleep(Duration::from_millis(500));
    assert!(rec.recorder.stop(Duration::from_secs(5)).clean);

    let (video, audio) = pts(&rec.path);
    let video = *video.first().expect("the file has video");
    let audio = *audio.first().expect("the file has audio");
    assert!((video - 0.5).abs() <= 0.040, "first video at {video} s");
    assert!(audio < 0.040, "first audio at {audio} s");
}

/// An EOS that never reaches the mux: `stop` gives up at its timeout, unclean,
/// with what was written so far.
#[test]
fn stop_times_out_when_eos_never_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let rec = start(dir.path(), Duration::ZERO);
    assert!(wait_for(&rec.messages, Duration::from_secs(3), |m| {
        *m == RecorderMessage::FirstVideo
    })
    .is_some());
    std::thread::sleep(Duration::from_millis(300));
    rec.recorder
        .pipeline()
        .by_name("audio-out")
        .unwrap()
        .static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, |_, info| {
            match &info.data {
                // `Handled`, not `Drop`: dropping EOS here makes 1.24's core
                // unref a NULL event (a GStreamer-CRITICAL).
                Some(gst::PadProbeData::Event(ev)) if ev.type_() == gst::EventType::Eos => {
                    gst::PadProbeReturn::Handled
                }
                _ => gst::PadProbeReturn::Ok,
            }
        });

    let begun = Instant::now();
    let outcome = rec.recorder.stop(Duration::from_secs(1));
    let took = begun.elapsed();
    assert!(!outcome.clean);
    assert!(
        took >= Duration::from_millis(950) && took < Duration::from_millis(1500),
        "stop took {took:?}"
    );
    assert!(outcome.duration > 0.0);
}

/// Records 2 s with a self-view that `stall` breaks as soon as its first
/// frame is shown, and checks the recording is the one it would have been
/// without a self-view: no recorder error, `stop` is prompt and clean, and
/// the file holds every frame, with no gap, to the stop.
///
/// Run on a thread of its own, so a recording the self-view has stalled fails
/// the test rather than hanging it: a camera thread stuck in the self-view
/// never returns from the recorder's NULL.
fn records_through_a_broken_self_view(stall: fn(&gst::Pipeline)) {
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let dir = tempfile::tempdir().unwrap();
        let rec = start(dir.path(), Duration::ZERO);
        let deadline = Instant::now() + Duration::from_secs(3);
        let frame = loop {
            if let Some(frame) = rec.self_view.take() {
                break frame;
            }
            assert!(Instant::now() < deadline, "no self-view frame");
            std::thread::sleep(Duration::from_millis(10));
        };
        // Small, RGBA, the camera's shape (320×180 from the test source).
        assert_eq!(
            (frame.info.width(), frame.info.height()),
            (480, 270),
            "{:?}",
            frame.info
        );
        assert_eq!(frame.info.format(), gst_video::VideoFormat::Rgba);

        let view = rec.recorder.self_view_pipeline().unwrap();
        stall(view);
        std::thread::sleep(Duration::from_secs(2));
        // The leaky bound: a stalled self-view holds one frame, not two
        // seconds of them.
        let held: u64 = view
            .by_name("self-view-src")
            .unwrap()
            .property("current-level-buffers");
        let begun = Instant::now();
        let outcome = rec.recorder.stop(Duration::from_secs(5));
        let took = begun.elapsed();
        let errors: Vec<_> = rec
            .messages
            .try_iter()
            .filter(|(_, m)| matches!(m, RecorderMessage::Error(_)))
            .collect();
        let (pts, _) = pts(&rec.path);
        let _ = done.send((held, outcome, took, errors, pts));
    });
    let (held, outcome, took, errors, pts) = match finished.recv_timeout(Duration::from_secs(20)) {
        Ok(results) => results,
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the recording thread panicked"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("the recording stalled: stop never returned")
        }
    };

    assert!(held <= 1, "the stalled self-view holds {held} frames");
    assert!(errors.is_empty(), "{errors:?}");

    assert!(outcome.clean, "stop timed out");
    assert!(took < Duration::from_secs(1), "stop took {took:?}");
    let (first, last) = (pts[0], *pts.last().unwrap());
    assert!(outcome.duration > 2.0, "{} s", outcome.duration);
    assert!(
        (last + FRAME - outcome.duration).abs() <= FRAME,
        "the last frame is at {last} s of {} s",
        outcome.duration
    );
    // Every frame of a steady 30 fps from the first to the last.
    let expected = ((last - first) / FRAME).round() as usize + 1;
    assert_eq!(pts.len(), expected, "frames from {first} s to {last} s");
    let gap = pts.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    assert!(gap < 1.5 * FRAME, "a {gap} s gap in the video");
}

/// The property the self-view must keep above all: a display that stops
/// taking frames altogether (its sink's thread blocked for good) costs the
/// recording nothing.
#[test]
fn a_self_view_that_stops_consuming_never_stalls_the_recording() {
    records_through_a_broken_self_view(|view| {
        sink_pad(view).add_probe(
            gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER,
            |_, _| gst::PadProbeReturn::Ok,
        );
    });
}

/// ... nor does one that fails: its source's next push returns an error, which
/// stops its streaming and posts an ERROR. Neither reaches the recording.
#[test]
fn a_self_view_that_fails_never_ends_the_recording() {
    records_through_a_broken_self_view(|view| {
        view.by_name("self-view-src")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, |_, info| {
                info.flow_res = Err(gst::FlowError::Error);
                gst::PadProbeReturn::Handled
            });
    });
}

fn sink_pad(view: &gst::Pipeline) -> gst::Pad {
    view.by_name("self-view-sink")
        .unwrap()
        .static_pad("sink")
        .unwrap()
}
