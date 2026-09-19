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
use video_coach_media::{CaptureSources, Recorder, RecorderMessage};

const FRAME: f64 = 1.0 / 30.0;

struct Recording {
    recorder: Recorder,
    messages: mpsc::Receiver<(u64, RecorderMessage)>,
    path: PathBuf,
    /// How long `Recorder::start` took.
    started_in: Duration,
}

fn start(dir: &Path, video_delay: Duration, generation: u64) -> Recording {
    gst::init().unwrap();
    let path = dir.join("rec.mkv");
    let (tx, messages) = mpsc::channel();
    let tx = Mutex::new(tx);
    let begun = Instant::now();
    let recorder = Recorder::start(
        CaptureSources::Test { video_delay },
        &path,
        generation,
        move |g, msg| {
            let _ = tx.lock().unwrap().send((g, msg));
        },
    )
    .unwrap();
    Recording {
        recorder,
        messages,
        path,
        started_in: begun.elapsed(),
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
            Ok((g, msg)) if want(&msg) => return Some((g, msg)),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// The first video and audio PTS in the file, in seconds, read by demuxing it.
fn first_pts(path: &Path) -> (Option<f64>, Option<f64>) {
    let pipeline = gst::parse::launch(&format!(
        "filesrc location={} ! matroskademux name=demux",
        path.display()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let firsts: Arc<Mutex<(Option<f64>, Option<f64>)>> = Arc::default();
    let demux = pipeline.by_name("demux").unwrap();
    demux.connect_pad_added({
        let pipeline = pipeline.downgrade();
        let firsts = firsts.clone();
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
            let firsts = firsts.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                let pts = info.buffer().and_then(|b| b.pts()).map(|t| t.seconds_f64());
                let mut firsts = firsts.lock().unwrap();
                let slot = if video { &mut firsts.0 } else { &mut firsts.1 };
                if slot.is_none() {
                    *slot = pts;
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
    let firsts = *firsts.lock().unwrap();
    firsts
}

#[test]
fn records_h264_and_opus_with_the_file_duration() {
    let dir = tempfile::tempdir().unwrap();
    let rec = start(dir.path(), Duration::ZERO, 1);
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

/// A camera that warms up for 0.5 s: `start` returns at once, audio starts at
/// file time 0 and video at 0.5 s, since file time 0 is `base_time` (R5).
#[test]
fn delayed_video_starts_at_its_running_time() {
    let dir = tempfile::tempdir().unwrap();
    let rec = start(dir.path(), Duration::from_millis(500), 1);
    assert!(
        rec.started_in < Duration::from_millis(200),
        "start took {:?}: it waited for PLAYING",
        rec.started_in
    );
    let first = wait_for(&rec.messages, Duration::from_secs(3), |m| {
        *m == RecorderMessage::FirstVideo
    });
    assert!(first.is_some(), "no FirstVideo");
    std::thread::sleep(Duration::from_millis(500));
    assert!(rec.recorder.stop(Duration::from_secs(5)).clean);

    let (video, audio) = first_pts(&rec.path);
    let video = video.expect("the file has video");
    let audio = audio.expect("the file has audio");
    assert!((video - 0.5).abs() <= 0.040, "first video at {video} s");
    assert!(audio < 0.040, "first audio at {audio} s");
}

#[test]
fn level_arrives_with_the_generation() {
    let dir = tempfile::tempdir().unwrap();
    let rec = start(dir.path(), Duration::ZERO, 7);
    let level = wait_for(&rec.messages, Duration::from_secs(1), |m| {
        matches!(m, RecorderMessage::Level { .. })
    });
    let Some((generation, RecorderMessage::Level { peak_db })) = level else {
        panic!("no Level within 1 s");
    };
    assert_eq!(generation, 7);
    assert!(peak_db.is_finite() && peak_db <= 0.0, "{peak_db} dB");
}

/// An EOS that never reaches the mux: `stop` gives up at its timeout, unclean,
/// with what was written so far.
#[test]
fn stop_times_out_when_eos_never_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let rec = start(dir.path(), Duration::ZERO, 1);
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
