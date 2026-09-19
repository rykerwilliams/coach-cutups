//! Skip-press coalescing. All ten cases ported from `SkipCoordinatorTests`.
//!
//! No fake clock appears anywhere in this file, and that is the point: the
//! coordinator holds no clock. The Swift original threads a
//! `nowMonotonicSeconds` parameter through all three methods and reads it in
//! none of them, so it is dropped here.

use std::time::Duration;

use video_coach_core::skip::{SeekParams, SkipCoordinator, SkipDecision, DEFAULT_BURST_WINDOW};

const W: Duration = DEFAULT_BURST_WINDOW;

fn exact(t: f64) -> Option<SeekParams> {
    Some(SeekParams {
        target_seconds: t,
        exact: true,
    })
}
fn coarse(t: f64) -> Option<SeekParams> {
    Some(SeekParams {
        target_seconds: t,
        exact: false,
    })
}

/// A single press must NOT do coarse-then-refine. That pattern lands visibly at
/// a keyframe before the target, then jumps again ~150 ms later — one keypress
/// producing a perceived double-seek.
#[test]
fn a_single_skip_seeks_exact_immediately_with_no_debounce() {
    let mut c = SkipCoordinator::new(W);
    let d = c.request_skip(3.0, 10.0, 0.0..=60.0);
    assert_eq!(d.seek, exact(13.0));
    assert_eq!(d.arm_debounce, None);
}

/// Nothing left to settle to, so the completion is inert.
#[test]
fn a_single_skip_completing_is_a_no_op() {
    let mut c = SkipCoordinator::new(W);
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    assert_eq!(c.seek_completed(), SkipDecision::default());
}

/// A follow-up press during flight issues **no seek** — it only accumulates
/// and re-arms. This is the shape an enum-valued return cannot express.
#[test]
fn a_second_skip_during_flight_accumulates_and_arms_only() {
    let mut c = SkipCoordinator::new(W);
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    let d2 = c.request_skip(3.0, 10.0, 0.0..=60.0);
    assert_eq!(d2.seek, None, "no new seek while one is in flight");
    assert_eq!(d2.arm_debounce, Some(W));
}

/// The follow-up accumulates onto the pending target, not onto the player's
/// current position — so two +3s presses go to 16, not back to 13.
#[test]
fn the_target_accumulates_rather_than_resetting_to_current() {
    let mut c = SkipCoordinator::default();
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    assert_eq!(c.seek_completed().seek, coarse(16.0));
}

/// The full burst sequence, which is where every branch meets.
#[test]
fn a_full_burst_switches_to_coarse_then_settles_exact() {
    let mut c = SkipCoordinator::new(W);

    // 1. Press -> exact at 13, in flight.
    assert_eq!(c.request_skip(3.0, 10.0, 0.0..=60.0).seek, exact(13.0));
    // 2. Press during flight -> target 16, debounce armed, no seek.
    assert_eq!(c.request_skip(3.0, 10.0, 0.0..=60.0).seek, None);

    // 3. The exact lands -> switch to burst mode: coarse to the new target,
    //    with a refreshed debounce.
    let switched = c.seek_completed();
    assert_eq!(switched.seek, coarse(16.0));
    assert_eq!(switched.arm_debounce, Some(W));

    // 4. The coarse lands with no further presses.
    assert_eq!(c.seek_completed().seek, None);

    // 5. The debounce fires -> exact settle on the final target.
    let burst = c.burst_ended();
    assert_eq!(burst.seek, exact(16.0));
    assert_eq!(burst.arm_debounce, None);

    // 6. A second debounce is inert.
    assert_eq!(c.burst_ended(), SkipDecision::default());
}

/// The debounce firing mid-flight does not seek; it arms a settle that the
/// next completion issues.
#[test]
fn a_debounce_during_flight_settles_on_the_next_completion() {
    let mut c = SkipCoordinator::new(W);
    c.request_skip(3.0, 10.0, 0.0..=60.0);

    let mid = c.burst_ended();
    assert_eq!(mid.seek, None, "must not seek while one is in flight");

    assert_eq!(c.seek_completed().seek, exact(13.0));
}

/// A press arriving after the debounce armed a settle cancels it and continues
/// the burst — proven by the next completion firing COARSE, not exact.
#[test]
fn a_skip_during_a_pending_settle_cancels_it_and_continues_the_burst() {
    let mut c = SkipCoordinator::new(W);
    c.request_skip(3.0, 10.0, 0.0..=60.0);

    assert_eq!(c.burst_ended().seek, None, "arms a pending settle");

    let third = c.request_skip(3.0, 10.0, 0.0..=60.0);
    assert_eq!(third.seek, None);
    assert_eq!(third.arm_debounce, Some(W));

    // Coarse, not exact: the pending settle was cleared by the press.
    assert_eq!(c.seek_completed().seek, coarse(16.0));
}

#[test]
fn a_skip_before_zero_clamps_to_zero() {
    let mut c = SkipCoordinator::default();
    assert_eq!(c.request_skip(-10.0, 3.0, 0.0..=60.0).seek, exact(0.0));
}

#[test]
fn a_skip_past_the_end_clamps_to_the_duration() {
    let mut c = SkipCoordinator::default();
    assert_eq!(c.request_skip(100.0, 50.0, 0.0..=60.0).seek, exact(60.0));
}

/// New. While recording, the range is one source inside the concat timeline,
/// so both ends bind, not just 0 and the total.
#[test]
fn a_skip_clamps_to_both_ends_of_a_source_range() {
    let mut c = SkipCoordinator::default();
    assert_eq!(
        c.request_skip(-10.0, 105.0, 100.0..=159.95).seek,
        exact(100.0)
    );

    let mut c = SkipCoordinator::default();
    assert_eq!(
        c.request_skip(10.0, 155.0, 100.0..=159.95).seek,
        exact(159.95)
    );
}

/// New. `target()` is the burst's accumulated intent, which the recording
/// bus logs as the pause anchor; it clears once the burst settles.
#[test]
fn target_reports_the_outstanding_burst() {
    let mut c = SkipCoordinator::default();
    assert_eq!(c.target(), None);
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    assert_eq!(c.target(), Some(16.0));
    c.seek_completed(); // leading exact lands: coarse to 16
    c.burst_ended(); // settle pending
    c.seek_completed(); // coarse lands: exact settle to 16 issued
    assert_eq!(c.target(), None, "handed to the final exact seek");
}

/// After a reset the next press bases off the player's current position, not a
/// stale accumulated target.
#[test]
fn reset_clears_state_and_allows_a_fresh_seek() {
    let mut c = SkipCoordinator::default();
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    c.reset();
    assert_eq!(
        c.request_skip(3.0, 20.0, 0.0..=60.0).seek,
        exact(23.0),
        "base is 20, not a stale 13"
    );
}

/// A completion arriving for a seek issued before a reset must not resurrect
/// anything.
#[test]
fn a_completion_arriving_after_reset_is_a_safe_no_op() {
    let mut c = SkipCoordinator::default();
    c.request_skip(3.0, 10.0, 0.0..=60.0);
    c.reset();
    assert_eq!(c.seek_completed(), SkipDecision::default());
}

/// The range's upper bound derives from a user-editable project.json with no
/// validation. An inverted or NaN one must not take down the UI thread on the
/// first arrow-key press — which `f64::clamp` would, since it asserts
/// `min <= max`.
#[test]
fn an_inverted_or_nan_upper_bound_does_not_panic() {
    let mut c = SkipCoordinator::default();
    assert_eq!(c.request_skip(3.0, 10.0, 0.0..=-1.0).seek, exact(0.0));

    let mut c = SkipCoordinator::default();
    let d = c.request_skip(3.0, 10.0, 0.0..=f64::NAN);
    assert_eq!(d.seek.map(|s| s.target_seconds), Some(0.0));
}
