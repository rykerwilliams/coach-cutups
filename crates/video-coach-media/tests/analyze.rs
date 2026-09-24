//! The analysis passes through the real graph: the GPU where there is one,
//! llvmpipe on CI.
//!
//! What is pinned here is the seam between the decode and core's rules — that
//! the motion series is sampled where it says it is, that it separates a
//! picture that holds from one that changes, and that a job stops when it is
//! told to and finishes exactly once. The rules themselves are pinned in
//! core's own tests, and how well they work on football is the `#[ignore]`d
//! ground-truth run's business.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use video_coach_core::motion::{still_intervals_at, MOTION_HZ, STILL_MIN_SECONDS, THUMBNAIL_HZ};
use video_coach_media::fixtures::{counter_video, solid_video, CounterKind};
use video_coach_media::{analyze, AnalyzeError, AnalyzeMessage, Analyzer};

/// The fixtures' frame rate, six times the rate the pass keeps.
const FPS: u32 = 30;

/// Long enough that the sampling assertions have something to be wrong about,
/// short enough to encode and decode in a test.
const SECONDS: u32 = 6;

/// A hold longer than [`STILL_MIN_SECONDS`](video_coach_core::motion::STILL_MIN_SECONDS),
/// so the stillness rule has something to find at its own constant rather
/// than at one invented for a test. Derived from that constant, because it is
/// measured and has moved once already.
const STILL_SECONDS: u32 = STILL_MIN_SECONDS as u32 + 2;

/// Far beyond any pass here; only a hang reaches it.
const LIMIT: Duration = Duration::from_secs(120);

fn init() {
    gstreamer::init().expect("GStreamer starts");
}

#[test]
fn a_still_picture_barely_moves_and_is_one_long_still_interval() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("still.webm");
    solid_video(&path, 320, 180, FPS, STILL_SECONDS * FPS, 0x20_60_a0, false);

    let series = analyze::motion::series(&path, &AtomicBool::new(false)).expect("the pass runs");

    // One value a frame after the first, at MOTION_HZ — give or take the
    // frame `videorate` keeps at each end.
    let expected = (f64::from(STILL_SECONDS) * MOTION_HZ) as usize;
    assert!(
        series.motion.len().abs_diff(expected - 1) <= 1,
        "{} motion values for {STILL_SECONDS} s at {MOTION_HZ} Hz",
        series.motion.len()
    );
    let thumbnails = (f64::from(STILL_SECONDS) * THUMBNAIL_HZ) as usize;
    assert!(
        series.thumbnails.len().abs_diff(thumbnails) <= 1,
        "{} thumbnails for {STILL_SECONDS} s at {THUMBNAIL_HZ} Hz",
        series.thumbnails.len()
    );
    let loudest = series.motion.iter().copied().fold(0.0, f32::max);
    assert!(loudest < 1.0, "a flat colour moved by {loudest}");
    // At a threshold spelled out, not at the shipped
    // [`STILL_QUANTILE`](video_coach_core::motion::STILL_QUANTILE): that one is
    // a quantile of the series' own distribution, so on a fixture that holds
    // still from end to end it marks half the frames as moving by
    // construction. Which threshold ports is core's question and the
    // ground-truth run's; what this pins is that the decode hands core a
    // series its rules can read.
    let still = still_intervals_at(&series.motion, MOTION_HZ, 1.0, STILL_MIN_SECONDS);
    assert_eq!(still.len(), 1, "{still:?}");
}

#[test]
fn a_changing_picture_moves_and_holds_still_nowhere() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("counting.webm");
    // Every frame is a different pattern of black and white blocks, so
    // consecutive thumbnails differ over most of the picture.
    counter_video(
        &path,
        640,
        360,
        FPS,
        SECONDS * FPS,
        CounterKind::Vp8WebmWithAudio,
    );

    let series = analyze::motion::series(&path, &AtomicBool::new(false)).expect("the pass runs");
    let quietest = series.motion.iter().copied().fold(f32::MAX, f32::min);
    assert!(quietest > 5.0, "a counter's quietest step was {quietest}");
    assert!(still_intervals_at(&series.motion, MOTION_HZ, 1.0, STILL_MIN_SECONDS).is_empty());
}

#[test]
fn a_cancelled_pass_stops_without_reading_the_file() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("still.webm");
    solid_video(&path, 320, 180, FPS, SECONDS * FPS, 0x40_40_40, false);

    let cancelled = AtomicBool::new(true);
    let started = Instant::now();
    let outcome = analyze::motion::series(&path, &cancelled);
    assert_eq!(outcome.err(), Some(AnalyzeError::Cancelled));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn an_analyzer_reports_progress_and_finishes_exactly_once() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("counting.webm");
    counter_video(
        &path,
        640,
        360,
        FPS,
        SECONDS * FPS,
        CounterKind::Vp8WebmWithAudio,
    );

    let (tx, rx) = mpsc::channel();
    let analyzer = Analyzer::start(path, move |msg| {
        let _ = tx.send(msg);
    });
    // `rx.iter()` ends when the job thread drops its sender, so the end of
    // this collection is itself the assertion that the thread is over.
    let messages: Vec<AnalyzeMessage> = rx.iter().collect();
    drop(analyzer);

    let terminal: Vec<&AnalyzeMessage> = messages
        .iter()
        .filter(|m| matches!(m, AnalyzeMessage::Finished(_)))
        .collect();
    assert_eq!(terminal.len(), 1, "{messages:?}");
    assert!(
        matches!(messages.last(), Some(AnalyzeMessage::Finished(Ok(_)))),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, AnalyzeMessage::Progress(p) if *p > 50)),
        "{messages:?}"
    );
    let Some(AnalyzeMessage::Finished(Ok(signals))) = messages.last() else {
        unreachable!("checked above")
    };
    assert!(!signals.motion.is_empty());
    assert!(!signals.cheer_excess.is_empty());
    assert!(!signals.thumbnails.is_empty());
    assert!((signals.audio_seconds - f64::from(SECONDS)).abs() < 0.5);
}

#[test]
fn a_cancelled_analyzer_finishes_cancelled_and_at_once() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("counting.webm");
    counter_video(
        &path,
        640,
        360,
        FPS,
        SECONDS * FPS,
        CounterKind::Vp8WebmWithAudio,
    );

    let (tx, rx) = mpsc::channel();
    let analyzer = Analyzer::start(path, move |msg| {
        let _ = tx.send(msg);
    });
    // The first message is sent before the sound is read, so the cancel lands
    // inside the job rather than before it starts.
    let first = rx.recv_timeout(LIMIT).expect("the job says hello");
    assert!(matches!(first, AnalyzeMessage::Progress(0)), "{first:?}");
    analyzer.cancel();
    let started = Instant::now();
    let messages: Vec<AnalyzeMessage> = rx.iter().collect();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "a cancelled analysis took {:?}",
        started.elapsed()
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, AnalyzeMessage::Finished(_)))
            .count(),
        1,
        "{messages:?}"
    );
    assert!(
        matches!(
            messages.last(),
            Some(AnalyzeMessage::Finished(Err(AnalyzeError::Cancelled)))
        ),
        "{messages:?}"
    );
}
