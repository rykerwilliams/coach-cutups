//! Stroke visibility and partial-draw replay.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::project::Clip;
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_core::stroke_replay::visible_strokes;

fn clip(events: Vec<CommentaryEvent>) -> Clip {
    Clip {
        id: Uuid::nil(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: 0.0,
        recording_duration: 60.0,
        recording_filename: "c.mkv".into(),
        events,
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// A stroke drawn over `duration` seconds with `n` points, logged (as the
/// recorder does) at the moment it FINISHES.
fn stroke_ev(
    finished_at: f64,
    duration: f64,
    n: usize,
    auto_clear: Option<f64>,
) -> CommentaryEvent {
    let points = (0..n)
        .map(|i| {
            let frac = if n > 1 {
                i as f64 / (n - 1) as f64
            } else {
                0.0
            };
            StrokePoint {
                x: 0.1 + frac * 0.8,
                y: 0.5,
                t: frac * duration,
            }
        })
        .collect();
    CommentaryEvent::new(
        finished_at,
        EventKind::Stroke(Stroke {
            id: Uuid::nil(),
            color: Rgba::RED,
            line_width: 0.006,
            points,
            auto_clear_after_seconds: auto_clear,
        }),
    )
}

fn clear_all(t: f64) -> CommentaryEvent {
    CommentaryEvent::new(t, EventKind::ClearAll)
}

// ------------------------------------------------ the back-computed start

/// The event is logged at pen-up, so a 4-second stroke logged at t=10 began at
/// t=6. Treating the event time as the start would make it appear 4 seconds
/// late.
#[test]
fn stroke_start_is_back_computed_from_its_duration() {
    let c = clip(vec![stroke_ev(10.0, 4.0, 5, None)]);
    assert!(
        visible_strokes(&c, 5.9).is_empty(),
        "must not be visible before it began"
    );
    let vis = visible_strokes(&c, 6.0);
    assert_eq!(vis.len(), 1);
    assert_eq!(vis[0].first_point_record_time, 6.0);
}

#[test]
fn a_stroke_draws_progressively_then_completes() {
    let c = clip(vec![stroke_ev(10.0, 4.0, 5, None)]);
    // Points at t = 0, 1, 2, 3, 4 relative to a start of 6.0.
    assert_eq!(visible_strokes(&c, 6.0)[0].drawn_point_count, 1);
    assert_eq!(visible_strokes(&c, 8.0)[0].drawn_point_count, 3);
    assert_eq!(visible_strokes(&c, 10.0)[0].drawn_point_count, 5);
    assert_eq!(
        visible_strokes(&c, 30.0)[0].drawn_point_count,
        5,
        "stays fully drawn"
    );
}

/// Strictly greater in the index search: a point whose `t` exactly equals
/// elapsed IS drawn.
#[test]
fn a_point_exactly_at_elapsed_is_drawn() {
    let c = clip(vec![stroke_ev(10.0, 4.0, 5, None)]);
    // elapsed = 1.0 lands exactly on the second point.
    assert_eq!(visible_strokes(&c, 7.0)[0].drawn_point_count, 2);
}

// ------------------------------------------------------------- auto-clear

/// Inclusive cutoff: hidden once `t >= first_t + auto`.
#[test]
fn auto_clear_boundary_is_inclusive() {
    let c = clip(vec![stroke_ev(10.0, 4.0, 5, Some(3.0))]);
    // Starts at 6.0, auto-clears at 9.0.
    assert_eq!(visible_strokes(&c, 8.999).len(), 1);
    assert!(
        visible_strokes(&c, 9.0).is_empty(),
        "inclusive at exactly first_t + auto"
    );
}

#[test]
fn a_stroke_with_no_auto_clear_persists() {
    let c = clip(vec![stroke_ev(10.0, 4.0, 5, None)]);
    assert_eq!(visible_strokes(&c, 10_000.0).len(), 1);
}

// -------------------------------------------------------------- clear-all

/// **The test that fails a single-pass implementation.** The stroke appears in
/// the event log BEFORE the clear-all that removes it, so an algorithm that
/// clears as it walks forward has already pushed the stroke into the output by
/// the time it sees the clear. A correct one collects every clear-all up to `t`
/// first.
#[test]
fn a_later_clear_all_removes_an_earlier_stroke_despite_forward_order() {
    let c = clip(vec![stroke_ev(2.0, 1.0, 3, None), clear_all(5.0)]);
    assert_eq!(
        visible_strokes(&c, 4.0).len(),
        1,
        "visible before the clear"
    );
    assert!(
        visible_strokes(&c, 6.0).is_empty(),
        "forward-order clear must still apply"
    );
}

#[test]
fn clear_all_affects_earlier_strokes_but_not_later_ones() {
    let c = clip(vec![
        stroke_ev(2.0, 1.0, 3, None),
        clear_all(5.0),
        stroke_ev(8.0, 1.0, 3, None),
    ]);
    let vis = visible_strokes(&c, 9.0);
    assert_eq!(vis.len(), 1, "only the post-clear stroke survives");
    assert_eq!(vis[0].first_point_record_time, 7.0);
}

/// Strictly after: a clear-all landing at the same instant the stroke began
/// does not erase it.
#[test]
fn clear_all_exactly_at_the_stroke_start_does_not_clear_it() {
    // Stroke logged at 5.0 with duration 2.0 -> begins at 3.0. The log is
    // sorted by record_time, so the clear-all at 3.0 precedes the stroke
    // event at 5.0 even though it coincides with the stroke's start.
    let c = clip(vec![clear_all(3.0), stroke_ev(5.0, 2.0, 3, None)]);
    assert_eq!(
        visible_strokes(&c, 6.0).len(),
        1,
        "clear at exactly first_t must not clear"
    );
}

#[test]
fn clear_all_one_tick_after_the_start_does_clear_it() {
    let c = clip(vec![clear_all(3.001), stroke_ev(5.0, 2.0, 3, None)]);
    assert!(visible_strokes(&c, 6.0).is_empty());
}

/// A clear-all in the future must not affect the present.
#[test]
fn a_future_clear_all_is_ignored() {
    let c = clip(vec![stroke_ev(2.0, 1.0, 3, None), clear_all(50.0)]);
    assert_eq!(visible_strokes(&c, 10.0).len(), 1);
}

// ----------------------------------------------------------------- shapes

/// A single-point stroke is legitimate — the compositor renders it as a filled
/// circle, since a zero-length path does not rasterize with a round cap.
#[test]
fn a_single_point_stroke_is_visible_with_one_point() {
    let c = clip(vec![stroke_ev(4.0, 0.0, 1, None)]);
    let vis = visible_strokes(&c, 4.0);
    assert_eq!(vis.len(), 1);
    assert_eq!(vis[0].drawn_point_count, 1);
    assert_eq!(vis[0].first_point_record_time, 4.0);
}

#[test]
fn non_stroke_events_are_ignored() {
    let c = clip(vec![
        CommentaryEvent::new(1.0, EventKind::Play { source_time: 0.0 }),
        CommentaryEvent::new(2.0, EventKind::Skip { delta: 5.0 }),
        stroke_ev(4.0, 1.0, 3, None),
    ]);
    assert_eq!(visible_strokes(&c, 5.0).len(), 1);
}

#[test]
fn strokes_are_returned_in_event_order() {
    let c = clip(vec![
        stroke_ev(2.0, 1.0, 2, None),
        stroke_ev(5.0, 1.0, 2, None),
    ]);
    let vis = visible_strokes(&c, 6.0);
    assert_eq!(vis.len(), 2);
    assert!(vis[0].first_point_record_time < vis[1].first_point_record_time);
}
