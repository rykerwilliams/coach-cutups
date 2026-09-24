//! The kick-off pattern and the confirmation rule (spec D3, D4), on synthetic
//! series.
//!
//! Nothing here decodes anything and nothing here is footage: these pin what
//! the rule *does* with a cheer, a hold and a whistle, so that the `#[ignore]`d
//! ground-truth run's numbers are about the footage rather than about a rule
//! nobody checked. The three cases the spec names — cheer then kick-off, cheer
//! with no kick-off, kick-off with no cheer — are the first three tests.

use video_coach_core::kickoff::{
    dedup, kickoffs, near_misses, suggest, Anchor, GoalTier, KickOff, Rule, Suggestion,
    SuggestionKind, AT_LEAD_SECONDS, CHEER_CLAMP_SECONDS, DEDUP_SECONDS, GOAL_WINDOW_SECONDS,
};
use video_coach_core::motion::{still_theta, MOTION_HZ, STILL_MIN_SECONDS};
use video_coach_core::signals::{Cheer, Whistle};

/// A motion series at [`MOTION_HZ`] from `(seconds, value)` stretches.
fn series(stretches: &[(f64, f32)]) -> Vec<f32> {
    let mut out = Vec::new();
    for &(seconds, value) in stretches {
        out.extend(std::iter::repeat_n(
            value,
            (seconds * MOTION_HZ).round() as usize,
        ));
    }
    out
}

/// The quantile these fixtures read their threshold at.
///
/// It has to sit **under** the share of the half that actually holds still, or
/// the threshold lands in play and the whole half reads as one hold. That is a
/// real property of a quantile threshold and not a quirk of the fixture: the
/// ground-truth sweep is what says which quantile the footage supports.
const QUANTILE: f64 = 0.05;

/// A half that plays, holds still for `hold` seconds at `at`, then plays again
/// to the end.
fn half_with_hold(at: f64, hold: f64, total: f64) -> Vec<f32> {
    series(&[(at, 20.0), (hold, 0.5), (total - at - hold, 20.0)])
}

fn cheer(onset: f64) -> Cheer {
    Cheer {
        onset,
        duration: 1.5,
        peak_db: 20.0,
    }
}

fn whistle(start: f64, duration: f64) -> Whistle {
    Whistle {
        start,
        duration,
        freq: 3_200.0,
        snr_db: 20.0,
        tonality_db: 15.0,
    }
}

/// A kick-off at `k`, as the picture stage would hand it over.
fn kick(k: f64) -> KickOff {
    KickOff {
        seconds: k,
        still: k - 20.0..k,
        anchor: Anchor::StillEnd,
    }
}

/// The rule with the gate off, which is what makes both tiers visible to a
/// test. The shipped value is measured, not assumed — see
/// `CHEER_GATES_CANDIDATES`.
fn ungated() -> Rule {
    Rule {
        cheer_gates: false,
        ..Rule::default()
    }
}

fn goals(found: &[Suggestion]) -> Vec<&Suggestion> {
    found
        .iter()
        .filter(|s| matches!(s.kind, SuggestionKind::Goal { .. }))
        .collect()
}

// --------------------------------------------- the three confirmation cases

#[test]
fn a_cheer_then_a_kick_off_is_a_high_tier_goal() {
    // The cheer stands a measured walk-back before the restart: V-3 timed
    // nine of them at 20.5-46.4 s, so 30 s is the middle of what the footage
    // does rather than a number that fits the window.
    let cheer_at = 600.0 - 30.0;
    let found = suggest(
        &[kick(100.0), kick(600.0)],
        &[cheer(cheer_at)],
        &[],
        ungated(),
    );
    let goals = goals(&found);
    assert_eq!(goals.len(), 1, "{found:?}");
    let SuggestionKind::Goal { tier, window, at } = goals[0].kind else {
        unreachable!("filtered to goals")
    };
    assert_eq!(tier, GoalTier::High);
    assert_eq!(at, Some(cheer_at - AT_LEAD_SECONDS));
    assert!(
        window.0 <= cheer_at && cheer_at <= window.1,
        "the window {window:?} holds the cheer it was made from"
    );
}

#[test]
fn a_cheer_with_no_kick_off_within_w_is_a_near_miss_and_not_a_goal() {
    // The cheer is more than W before the only kick-off, so nothing explains
    // it — the case that stops "every cheer is a goal".
    let far = 600.0 - GOAL_WINDOW_SECONDS - 60.0;
    let found = suggest(&[kick(100.0), kick(600.0)], &[cheer(far)], &[], ungated());
    let goals = goals(&found);
    assert!(
        matches!(goals[0].kind, SuggestionKind::Goal { tier, .. } if tier == GoalTier::Quiet),
        "{found:?}"
    );
    assert_eq!(
        near_misses(&[kick(100.0), kick(600.0)], &[cheer(far)], ungated()),
        vec![far]
    );
}

#[test]
fn a_kick_off_with_no_cheer_is_a_quiet_goal_or_nothing_when_the_gate_is_on() {
    let quiet = suggest(&[kick(100.0), kick(600.0)], &[], &[], ungated());
    let found = goals(&quiet);
    assert_eq!(found.len(), 1);
    assert!(
        matches!(
            found[0].kind,
            SuggestionKind::Goal {
                tier: GoalTier::Quiet,
                at: None,
                ..
            }
        ),
        "a quiet goal has no estimated instant: {quiet:?}"
    );

    let gated = suggest(
        &[kick(100.0), kick(600.0)],
        &[],
        &[],
        Rule {
            cheer_gates: true,
            ..Rule::default()
        },
    );
    assert!(goals(&gated).is_empty(), "{gated:?}");
    assert_eq!(
        gated.len(),
        1,
        "the gate drops goals, never the period start: {gated:?}"
    );
}

// ------------------------------------------------------------- D4's details

#[test]
fn a_cheer_inside_the_clamp_is_the_restarts_own_and_does_not_raise_the_tier() {
    let k = 600.0;
    let found = suggest(
        &[kick(100.0), kick(k)],
        &[cheer(k - CHEER_CLAMP_SECONDS + 1.0)],
        &[],
        ungated(),
    );
    assert!(
        matches!(goals(&found)[0].kind, SuggestionKind::Goal { tier, .. } if tier == GoalTier::Quiet),
        "{found:?}"
    );
}

#[test]
fn the_first_kick_off_is_a_period_start_and_not_a_goal() {
    let found = suggest(&[kick(60.0)], &[cheer(30.0)], &[], ungated());
    assert_eq!(
        found,
        vec![Suggestion {
            seconds: 60.0,
            kind: SuggestionKind::PeriodStart
        }]
    );
}

#[test]
fn a_window_clamps_to_the_previous_kick_off() {
    // At the spec's guessed W = 150 the measured 65 s from a period start to
    // the first goal was clipped every time. At the **measured** W = 60 it no
    // longer is -- a goal has to fall inside a minute of the kick-off for the
    // clamp to reach at all -- so the clamp is now belt-and-braces rather than
    // load-bearing, and this pins it on a pair that still reaches it.
    let start = 52.0;
    let restart = start + GOAL_WINDOW_SECONDS - 20.0;
    let found = suggest(
        &[kick(start), kick(restart)],
        &[cheer(start + 5.0)],
        &[],
        ungated(),
    );
    let SuggestionKind::Goal { window, .. } = goals(&found)[0].kind else {
        unreachable!("filtered to goals")
    };
    assert_eq!(window.0, start, "clamped to K_prev, not to K − W");
}

#[test]
fn one_runs_goal_windows_are_disjoint() {
    let ks = [100.0, 200.0, 260.0, 900.0];
    let found = suggest(
        &ks.map(kick),
        &ks.iter().map(|k| cheer(k - 40.0)).collect::<Vec<_>>(),
        &[],
        ungated(),
    );
    let windows: Vec<(f64, f64)> = goals(&found)
        .iter()
        .map(|s| match s.kind {
            SuggestionKind::Goal { window, .. } => window,
            _ => unreachable!("filtered to goals"),
        })
        .collect();
    for pair in windows.windows(2) {
        assert!(
            pair[0].1 <= pair[1].0,
            "{:?} and {:?} overlap",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn a_gated_kick_off_does_not_clamp_the_window_after_it() {
    // The silent restart at 370 is dropped, so the goal at 400 keeps its full
    // W rather than paying twice for the gate. The three are inside one W of
    // each other on purpose: at the measured 60 s that is what it takes for a
    // dropped candidate to be able to clamp the next one at all.
    let rule = Rule {
        cheer_gates: true,
        ..Rule::default()
    };
    let found = suggest(
        &[kick(100.0), kick(370.0), kick(400.0)],
        &[cheer(360.0)],
        &[],
        rule,
    );
    let SuggestionKind::Goal { window, .. } = goals(&found)[0].kind else {
        unreachable!("filtered to goals")
    };
    assert_eq!(window.0, 400.0 - GOAL_WINDOW_SECONDS);
}

#[test]
fn the_period_ends_on_the_last_long_whistle_at_the_floor_the_rule_was_given() {
    let whistles = [
        whistle(50.0, 0.4),
        whistle(1600.0, 0.5),
        whistle(1700.0, 0.2),
    ];
    let short_floor = Rule {
        long_whistle_seconds: 0.35,
        ..ungated()
    };
    let found = suggest(&[kick(60.0)], &[], &whistles, short_floor);
    assert_eq!(found.last().unwrap().seconds, 1600.0);

    // At the spec's own 0.8 s floor this footage has no long whistle at all,
    // and the rule says nothing rather than guessing.
    let found = suggest(&[kick(60.0)], &[], &whistles, ungated());
    assert!(
        !found.iter().any(|s| s.kind == SuggestionKind::PeriodEnd),
        "{found:?}"
    );
}

// -------------------------------------------------------- the picture stage

#[test]
fn a_hold_that_ends_in_play_is_a_kick_off_and_one_the_file_ends_in_is_not() {
    let motion = half_with_hold(120.0, 20.0, 240.0);
    let theta = still_theta(&motion, QUANTILE);
    let found = kickoffs(&motion, MOTION_HZ, theta, STILL_MIN_SECONDS, &[]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!((found[0].seconds - 140.0).abs() < 1.0, "{found:?}");
    assert_eq!(found[0].anchor, Anchor::StillEnd);

    let ends_still = series(&[(200.0, 20.0), (60.0, 0.5)]);
    let theta = still_theta(&ends_still, QUANTILE);
    assert!(
        kickoffs(&ends_still, MOTION_HZ, theta, STILL_MIN_SECONDS, &[]).is_empty(),
        "play never resumed, so nothing restarted"
    );
}

#[test]
fn play_resuming_is_a_median_and_not_every_frame() {
    // One frame back under θ two seconds into play. Measured over six halves,
    // requiring every frame left no candidate anywhere; the median is what
    // makes the stage a rule rather than a filter.
    let mut motion = half_with_hold(120.0, 20.0, 240.0);
    motion[(142.0 * MOTION_HZ) as usize] = 0.1;
    let theta = still_theta(&motion, QUANTILE);
    assert_eq!(
        kickoffs(&motion, MOTION_HZ, theta, STILL_MIN_SECONDS, &[]).len(),
        1,
        "one dip is not the end of play"
    );

    // Most of the run still: the picture has not restarted, whatever the hold
    // before it looked like.
    let stutter = series(&[
        (120.0, 20.0),
        (20.0, 0.5),
        (1.2, 20.0),
        (1.8, 0.5),
        (100.0, 20.0),
    ]);
    let theta = still_theta(&stutter, QUANTILE);
    let found = kickoffs(&stutter, MOTION_HZ, theta, STILL_MIN_SECONDS, &[]);
    assert!(
        found.iter().all(|k| k.seconds > 143.0),
        "a hold that stutters back into stillness has not ended in play: {found:?}"
    );
}

#[test]
fn a_whistle_near_the_end_of_the_hold_is_the_kick_offs_time() {
    let motion = half_with_hold(120.0, 20.0, 240.0);
    let theta = still_theta(&motion, QUANTILE);
    let found = kickoffs(
        &motion,
        MOTION_HZ,
        theta,
        STILL_MIN_SECONDS,
        &[whistle(138.5, 0.3)],
    );
    assert_eq!(found[0].seconds, 138.5);
    assert_eq!(found[0].anchor, Anchor::Whistle);

    // A whistle a minute away belongs to something else.
    let found = kickoffs(
        &motion,
        MOTION_HZ,
        theta,
        STILL_MIN_SECONDS,
        &[whistle(80.0, 0.3)],
    );
    assert_eq!(found[0].anchor, Anchor::StillEnd);
}

#[test]
fn the_threshold_is_the_halfs_own_distribution_and_not_a_level() {
    // The same half, filmed 30x brighter. An absolute θ finds one of these two
    // and floods the other; the quantile finds the same kick-off in both,
    // which is the whole reason the constant is a quantile.
    let dim = half_with_hold(120.0, 20.0, 240.0);
    let bright: Vec<f32> = dim.iter().map(|m| m * 30.0).collect();
    let at = |motion: &[f32]| {
        kickoffs(
            motion,
            MOTION_HZ,
            still_theta(motion, QUANTILE),
            STILL_MIN_SECONDS,
            &[],
        )
        .iter()
        .map(|k| k.seconds)
        .collect::<Vec<_>>()
    };
    assert_eq!(at(&dim), at(&bright));
}

// --------------------------------------------------------- D7's re-run rule

#[test]
fn a_re_run_drops_a_suggestion_the_coach_already_dealt_with() {
    let kept = [
        Suggestion {
            seconds: 600.0,
            kind: SuggestionKind::Goal {
                tier: GoalTier::High,
                window: (450.0, 600.0),
                at: Some(500.0),
            },
        },
        Suggestion {
            seconds: 60.0,
            kind: SuggestionKind::PeriodStart,
        },
    ];
    let fresh = vec![
        // Within 10 s of the resolved goal, and the same kind: dropped.
        Suggestion {
            seconds: 600.0 + DEDUP_SECONDS - 0.5,
            kind: SuggestionKind::Goal {
                tier: GoalTier::Quiet,
                window: (500.0, 609.5),
                at: None,
            },
        },
        // The same time as the kept period start, but a goal: kept, because a
        // dismissed period row says nothing about a goal.
        Suggestion {
            seconds: 60.0,
            kind: SuggestionKind::Goal {
                tier: GoalTier::High,
                window: (0.0, 60.0),
                at: Some(30.0),
            },
        },
        // Just outside the window: kept.
        Suggestion {
            seconds: 600.0 + DEDUP_SECONDS + 0.5,
            kind: SuggestionKind::Goal {
                tier: GoalTier::High,
                window: (460.0, 610.5),
                at: Some(500.0),
            },
        },
    ];
    let out = dedup(fresh.clone(), &kept);
    assert_eq!(out, vec![fresh[1], fresh[2]]);
}
