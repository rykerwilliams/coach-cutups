//! Playback-timeline replay: source time and play/freeze segmentation.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::project::Clip;
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_core::timeline::{playback_segments, source_time, PlaybackSegment, SegmentKind};
use video_coach_core::zoom::Zoom;

fn clip(start: f64, duration: f64, events: Vec<CommentaryEvent>) -> Clip {
    Clip {
        id: Uuid::nil(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: start,
        recording_duration: duration,
        recording_filename: "c.mkv".into(),
        events,
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

fn play(t: f64, anchor: f64) -> CommentaryEvent {
    CommentaryEvent::new(
        t,
        EventKind::Play {
            source_time: anchor,
        },
    )
}
fn pause(t: f64, anchor: f64) -> CommentaryEvent {
    CommentaryEvent::new(
        t,
        EventKind::Pause {
            source_time: anchor,
        },
    )
}
fn skip(t: f64, delta: f64) -> CommentaryEvent {
    CommentaryEvent::new(t, EventKind::Skip { delta })
}

const DUR: f64 = 1000.0;

// ------------------------------------------------------------- source_time

#[test]
fn source_advances_at_one_x_with_no_events() {
    let c = clip(10.0, 30.0, vec![]);
    assert_eq!(source_time(&c, 0.0, DUR), 10.0);
    assert_eq!(source_time(&c, 5.0, DUR), 15.0);
}

#[test]
fn pause_holds_source_time() {
    let c = clip(10.0, 30.0, vec![pause(2.0, 12.0)]);
    assert_eq!(source_time(&c, 2.0, DUR), 12.0);
    assert_eq!(
        source_time(&c, 9.0, DUR),
        12.0,
        "source must not advance while paused"
    );
}

#[test]
fn play_resumes_from_its_anchor() {
    let c = clip(10.0, 30.0, vec![pause(2.0, 12.0), play(6.0, 12.0)]);
    assert_eq!(source_time(&c, 8.0, DUR), 14.0);
}

#[test]
fn skip_offsets_source_time() {
    let c = clip(10.0, 30.0, vec![skip(2.0, 5.0)]);
    assert_eq!(source_time(&c, 2.0, DUR), 17.0);
}

/// The anchor overrides the wall-clock computation. Player latency makes a
/// computed cursor drift by tens of milliseconds; the captured value pins the
/// frame to what was actually on screen.
#[test]
fn anchor_overrides_wall_clock_drift() {
    // Wall clock would say 10 + 3 = 13; the anchor says the player was at 13.4.
    let c = clip(10.0, 30.0, vec![pause(3.0, 13.4)]);
    assert_eq!(source_time(&c, 3.0, DUR), 13.4);
    assert_eq!(source_time(&c, 20.0, DUR), 13.4);
}

#[test]
fn non_transport_events_do_not_move_source_time() {
    let stroke = Stroke {
        id: Uuid::nil(),
        color: Rgba::RED,
        line_width: 0.01,
        points: vec![StrokePoint {
            x: 0.5,
            y: 0.5,
            t: 0.0,
        }],
        auto_clear_after_seconds: None,
    };
    let c = clip(
        10.0,
        30.0,
        vec![
            CommentaryEvent::new(1.0, EventKind::Stroke(stroke)),
            CommentaryEvent::new(2.0, EventKind::ClearAll),
            CommentaryEvent::new(3.0, EventKind::Zoom(Zoom::new(2.0, 0.0, 0.0))),
        ],
    );
    assert_eq!(source_time(&c, 5.0, DUR), 15.0);
}

/// Clamping is per-mutation, not once on the way out. A return-value clamp
/// would answer 1000 here instead of 990.
#[test]
fn skip_clamp_is_applied_per_mutation() {
    let c = clip(0.0, 30.0, vec![skip(1.0, 1e6), skip(2.0, -10.0)]);
    assert_eq!(source_time(&c, 2.0, DUR), 990.0);
}

// -------------------------------------------------------- playback_segments

#[test]
fn a_clip_with_no_events_is_one_play_segment() {
    let c = clip(10.0, 30.0, vec![]);
    assert_eq!(
        playback_segments(&c, DUR),
        [PlaybackSegment {
            kind: SegmentKind::Play,
            source_start: 10.0,
            out_duration: 30.0
        }]
    );
}

/// Every recording opens with a pause at record time 0, so the first segment of
/// essentially every real clip is a freeze.
#[test]
fn initial_pause_at_zero_makes_the_first_segment_a_freeze() {
    let c = clip(10.0, 10.0, vec![pause(0.0, 10.0), play(4.0, 10.0)]);
    let segs = playback_segments(&c, DUR);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].kind, SegmentKind::Freeze);
    assert_eq!(segs[0].out_duration, 4.0);
    assert_eq!(segs[1].kind, SegmentKind::Play);
    assert_eq!(segs[1].out_duration, 6.0);
}

/// Playing past the end splits into a play tail plus a freeze on the last
/// frame, mirroring a player holding rather than showing nothing.
#[test]
fn playing_past_eof_splits_into_play_tail_and_freeze() {
    let c = clip(95.0, 20.0, vec![]);
    let segs = playback_segments(&c, 100.0);
    assert_eq!(segs.len(), 2);
    assert_eq!(
        segs[0],
        PlaybackSegment {
            kind: SegmentKind::Play,
            source_start: 95.0,
            out_duration: 5.0
        }
    );
    assert_eq!(segs[1].kind, SegmentKind::Freeze);
    assert_eq!(segs[1].out_duration, 15.0);
    assert!(segs[1].source_start <= 100.0 - 0.05);
}

#[test]
fn every_play_range_stays_in_bounds_even_after_ff_past_end() {
    let c = clip(90.0, 30.0, vec![skip(1.0, 500.0)]);
    for s in playback_segments(&c, 100.0) {
        assert!(s.source_start >= 0.0, "negative source_start: {s:?}");
        if s.kind == SegmentKind::Play {
            assert!(
                s.source_start + s.out_duration <= 100.0 + 1e-9,
                "out of bounds: {s:?}"
            );
        } else {
            assert!(s.source_start <= 100.0, "freeze past EOF: {s:?}");
        }
    }
}

/// A sub-50 ms source must not produce a negative freeze anchor — and that is
/// exactly what synthetic test fixtures are.
#[test]
fn sub_fifty_millisecond_source_has_no_negative_freeze_anchor() {
    let c = clip(0.0, 1.0, vec![pause(0.0, 0.0)]);
    for s in playback_segments(&c, 0.02) {
        assert!(s.source_start >= 0.0, "negative freeze anchor: {s:?}");
    }
}

/// A dense zoom track must not split the timeline. A continuous pinch emits
/// ~60 events/second; splitting on each would explode the segment count.
#[test]
fn zoom_events_do_not_split_segments() {
    let bare = clip(0.0, 10.0, vec![pause(0.0, 0.0), play(2.0, 0.0)]);
    let mut events = bare.events.clone();
    for i in 0..600 {
        let t = i as f64 / 60.0;
        events.push(CommentaryEvent::new(
            t,
            EventKind::Zoom(Zoom::new(1.0 + t / 100.0, 0.0, 0.0)),
        ));
    }
    events.sort_by(|a, b| a.record_time.partial_cmp(&b.record_time).unwrap());
    let zoomed = clip(0.0, 10.0, events);

    assert_eq!(
        playback_segments(&bare, DUR).len(),
        playback_segments(&zoomed, DUR).len(),
        "zoom events must not create segments"
    );
}

/// Sub-millisecond gaps produce real segments; callers skip degenerate ones.
/// (This replaces a macOS test asserting that 0.5 ms rounds to zero ticks at
/// timescale 600 — GStreamer's nanosecond timebase has no such rounding.)
#[test]
fn sub_millisecond_segments_are_produced_not_rounded_away() {
    let c = clip(0.0, 1.0, vec![pause(0.0005, 0.0005), play(0.001, 0.0005)]);
    let segs = playback_segments(&c, DUR);
    assert!(
        segs.iter().all(|s| s.out_duration > 0.0),
        "no zero-duration segments"
    );
    assert!(
        segs.iter().any(|s| s.out_duration < 0.001),
        "sub-ms segment should survive"
    );
}

// ------------------------------------------------- the two functions agree

/// The invariant the clamp exists for: `source_time` must match the position
/// implied by walking the segments. Every other test here passes with the two
/// disagreeing, which is why this one exists.
#[test]
fn source_time_agrees_with_the_segment_walk() {
    let c = clip(
        90.0,
        30.0,
        vec![
            pause(0.0, 90.0),
            play(2.0, 90.0),
            skip(12.0, 500.0),
            skip(18.0, -20.0),
        ],
    );
    let segs = playback_segments(&c, 100.0);

    let mut t = 0.0;
    // Segment spans are half-open: `[acc, acc + out_duration)`. At an exact
    // boundary the LATER segment owns the instant, matching `source_time`,
    // where an event at that record time has already applied.
    while t < c.recording_duration {
        let mut acc = 0.0;
        let mut walked = None;
        for s in &segs {
            if t < acc + s.out_duration {
                walked = Some(match s.kind {
                    SegmentKind::Play => s.source_start + (t - acc),
                    SegmentKind::Freeze => s.source_start,
                });
                break;
            }
            acc += s.out_duration;
        }
        let walked = walked.unwrap_or_else(|| panic!("no segment covers t={t}"));
        let direct = source_time(&c, t, 100.0);

        // They agree exactly, except past EOF where the freeze cap pulls the
        // segment answer back by up to FREEZE_EOF_BACKOFF.
        assert!(
            (walked - direct).abs() <= 0.05 + 1e-9,
            "t={t}: segment walk {walked} vs source_time {direct}"
        );
        t += 0.25;
    }
}

/// The 50 ms gap is deliberate: the freeze cap is a decoder-safety pullback,
/// not a semantic answer. Pinned so nobody "fixes" one side and leaks a decoder
/// detail into the match clock.
#[test]
fn the_eof_gap_between_the_two_functions_is_deliberate() {
    let c = clip(998.0, 10.0, vec![skip(1.0, 100.0)]);
    let segs = playback_segments(&c, 1000.0);
    let last = segs.last().unwrap();

    assert_eq!(last.kind, SegmentKind::Freeze);
    assert_eq!(
        last.source_start, 999.95,
        "segment builder caps at duration - 0.05"
    );
    assert_eq!(
        source_time(&c, 5.0, 1000.0),
        1000.0,
        "the clock reads the true end"
    );
}
