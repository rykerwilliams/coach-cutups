//! The recording event log. The zoom-capture tests are ported from
//! `RecordingZoomCaptureTests`; the rest are new, covering the deviations the
//! Phase 4 spec (R8) makes: caller-supplied times, events written by
//! construction, and record times that never go backwards.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::recording::RecordingLog;
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_core::zoom::Zoom;

/// t0 is deliberately far from zero, so a test that forgot to subtract it
/// fails.
const T0: u64 = 5_000_000_000_000;

/// `host_ns` for `secs` into the recording.
fn at(secs: f64) -> u64 {
    T0 + (secs * 1e9).round() as u64
}

fn zooms(events: &[CommentaryEvent]) -> Vec<(f64, Zoom)> {
    events
        .iter()
        .filter_map(|e| match e.kind {
            EventKind::Zoom(z) => Some((e.record_time, z)),
            _ => None,
        })
        .collect()
}

fn scale(s: f64) -> Zoom {
    Zoom::new(s, 0.0, 0.0)
}

/// A two-point stroke spanning `duration` seconds.
fn a_stroke(duration: f64) -> Stroke {
    Stroke {
        id: Uuid::nil(),
        color: Rgba::RED,
        line_width: 0.005,
        points: vec![
            StrokePoint {
                x: 0.1,
                y: 0.2,
                t: 0.0,
            },
            StrokePoint {
                x: 0.3,
                y: 0.4,
                t: duration,
            },
        ],
        auto_clear_after_seconds: Some(5.0),
    }
}

/// New. The initial zoom, then the pause at the start position, both at 0.
/// Ported in part from `test_inherit_at_t0_emits_initial_zoom_event`.
#[test]
fn construction_writes_zoom_then_pause_at_zero() {
    let initial = Zoom::new(2.0, 0.1, 0.0);
    let events = RecordingLog::new(T0, initial, 12.5).finish();
    assert_eq!(
        events,
        vec![
            CommentaryEvent::new(0.0, EventKind::Zoom(initial)),
            CommentaryEvent::new(0.0, EventKind::Pause { source_time: 12.5 }),
        ]
    );
}

/// New. Times are the caller's `host_ns` minus t0, and the anchors are the
/// caller's, not anything the log derives.
#[test]
fn play_and_pause_log_caller_times_and_anchors() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 10.0);
    log.play(at(0.5), 10.0);
    log.pause(at(2.25), 11.7);
    let events = log.finish();
    assert_eq!(
        events[2..],
        [
            CommentaryEvent::new(0.5, EventKind::Play { source_time: 10.0 }),
            CommentaryEvent::new(2.25, EventKind::Pause { source_time: 11.7 }),
        ]
    );
}

/// New. The requested delta is logged even if the player clamped it; replay
/// clamps within the source.
#[test]
fn skip_logs_the_requested_delta() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 1.0);
    log.skip(at(1.0), -10.0);
    assert_eq!(
        log.finish()[2],
        CommentaryEvent::new(1.0, EventKind::Skip { delta: -10.0 })
    );
}

/// New. Both carry the caller's time, and the stroke's is its **pen-up** — so
/// `visible_strokes` back-computes a start of 1.5 − 0.75 here.
#[test]
fn stroke_and_clear_all_log_caller_times() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 0.0);
    let s = a_stroke(0.75);
    log.stroke(at(1.5), s.clone());
    log.clear_all(at(2.0));
    assert_eq!(
        log.finish()[2..],
        [
            CommentaryEvent::new(1.5, EventKind::Stroke(s)),
            CommentaryEvent::new(2.0, EventKind::ClearAll),
        ]
    );
}

/// New. A keypress captured just before t0 lands at 0, not negative.
#[test]
fn a_host_time_before_t0_lands_at_zero() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 1.0);
    log.play(T0 - 30_000_000, 1.0);
    assert_eq!(log.finish()[2].record_time, 0.0);
}

/// New. A host time older than the last event is clamped up to it, so the log
/// stays sorted.
#[test]
fn record_times_never_go_backwards() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 1.0);
    log.play(at(2.0), 1.0);
    log.pause(at(1.5), 1.2);
    let events = log.finish();
    assert_eq!(events[3].record_time, 2.0);
}

/// Ported: `test_continuous_capture_emits_keyframes_without_anchor`.
#[test]
fn continuous_capture_emits_keyframes_without_anchor() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 0.0);
    log.zoom(at(0.05), scale(1.5));
    log.zoom(at(0.10), scale(2.0));
    assert_eq!(
        zooms(&log.finish()),
        vec![(0.0, Zoom::IDENTITY), (0.05, scale(1.5)), (0.1, scale(2.0))]
    );
}

/// Ported: `test_appendZoom_capturesEveryDistinctValue_noTimeThrottling`.
#[test]
fn a_dense_zoom_stream_is_kept_whole() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 0.0);
    for i in 1..=100 {
        log.zoom(at(i as f64 / 60.0), scale(1.0 + i as f64 * 0.005));
    }
    assert_eq!(zooms(&log.finish()).len(), 101);
}

/// Ported: `test_appendZoom_dedupsBackToBackIdenticalValues`. A deduped value
/// is not a capture, so the next distinct one, 150 ms after the last capture,
/// still gets an anchor.
#[test]
fn back_to_back_identical_zooms_are_deduped() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 0.0);
    let z = Zoom::new(2.0, 0.1, 0.0);
    log.zoom(at(0.05), z);
    for i in 1..=5 {
        log.zoom(at(0.05 + i as f64 * 0.02), z);
    }
    let z2 = Zoom::new(2.5, 0.1, 0.0);
    log.zoom(at(0.20), z2);
    let got = zooms(&log.finish());
    assert_eq!(
        got.len(),
        4,
        "initial, first distinct, anchor, second distinct"
    );
    assert_eq!(got[2].1, z, "the anchor holds the previous value");
    assert!((got[2].0 - 0.199).abs() < 1e-9);
    assert_eq!(got[3], (0.2, z2));
}

/// Ported: `test_discrete_change_after_quiet_period_emits_anchor_keyframe`.
#[test]
fn a_change_after_a_quiet_period_is_preceded_by_an_anchor() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 0.0);
    log.zoom(at(5.0), scale(2.0));
    let got = zooms(&log.finish());
    assert_eq!(got.len(), 3);
    assert_eq!(
        got[1].1,
        Zoom::IDENTITY,
        "the anchor holds the previous value"
    );
    assert!((got[1].0 - 4.999).abs() < 1e-9);
    assert_eq!(got[2], (5.0, scale(2.0)));
}

/// New. macOS put the anchor at `t − 1 ms` unconditionally, which could land
/// before an event logged in that last millisecond and unsort the log.
#[test]
fn the_anchor_never_lands_before_the_last_event() {
    let mut log = RecordingLog::new(T0, Zoom::IDENTITY, 0.0);
    log.pause(at(4.9995), 3.0);
    log.zoom(at(5.0), scale(2.0));
    let events = log.finish();
    assert_eq!(
        events[3..],
        [
            CommentaryEvent::new(4.9995, EventKind::Zoom(Zoom::IDENTITY)),
            CommentaryEvent::new(5.0, EventKind::Zoom(scale(2.0))),
        ]
    );
    assert!(events
        .windows(2)
        .all(|w| w[0].record_time <= w[1].record_time));
}
