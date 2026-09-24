//! The scorer and the ground-truth reader, on hand-built data.
//!
//! No footage, no network, no GStreamer pipeline: this is the part of the
//! measurement tooling CI can run, and it is what pins the pairing rules the
//! `#[ignore]`d `ground_truth` test reports through.

use video_coach_core::kickoff::{GoalTier, SuggestionKind};
use video_coach_core::signals::Cheer;
use video_coach_harness::score::{
    cheer_coverage, nearest, score, show_rate, Detection, CHEER_TOLERANCE, PERIOD_TOLERANCE,
};
use video_coach_harness::truth::{folders, parse_kickoffs, TruthError, TruthEvent, TruthKind};

fn goal(seconds: f64) -> TruthEvent {
    TruthEvent {
        source_index: 0,
        seconds,
        kind: TruthKind::Goal,
    }
}

fn tag(seconds: f64, kind: TruthKind) -> TruthEvent {
    TruthEvent {
        source_index: 0,
        seconds,
        kind,
    }
}

/// A high-tier goal detection at `k` whose window runs back `width` seconds,
/// with its cheer `at` five seconds before `k`.
fn found_goal(k: f64, width: f64, tier: GoalTier) -> Detection {
    Detection {
        source_index: 0,
        seconds: k,
        kind: SuggestionKind::Goal {
            tier,
            window: (k - width, k),
            at: (tier == GoalTier::High).then_some(k - 5.0),
        },
    }
}

fn found_period(seconds: f64, kind: SuggestionKind) -> Detection {
    Detection {
        source_index: 0,
        seconds,
        kind,
    }
}

#[test]
fn a_goal_in_the_window_is_found_and_one_outside_is_missed() {
    let r = score(&[goal(400.0)], &[found_goal(500.0, 150.0, GoalTier::High)]);
    assert_eq!(r.tally.goals.tp, 1);
    assert_eq!(r.tally.goals.fp, 0);
    assert_eq!(r.tally.goals.misses, 0);

    let r = score(&[goal(200.0)], &[found_goal(500.0, 150.0, GoalTier::High)]);
    assert_eq!(
        (r.tally.goals.tp, r.tally.goals.fp, r.tally.goals.misses),
        (0, 1, 1)
    );
    assert_eq!(r.missed_goals, vec![goal(200.0)]);
}

/// The pairing rule that silently inflates recall when it is wrong.
#[test]
fn two_goals_in_one_window_are_one_hit_and_one_miss() {
    let r = score(
        &[goal(380.0), goal(420.0)],
        &[found_goal(500.0, 150.0, GoalTier::High)],
    );
    assert_eq!(
        r.tally.goals.tp, 1,
        "one window can only account for one goal"
    );
    assert_eq!(r.tally.goals.misses, 1);
    assert_eq!(r.tally.goals.fp, 0);
    assert_eq!(
        r.goal_hits[0].truth_seconds, 380.0,
        "the earlier goal takes the window"
    );
    assert_eq!(r.missed_goals, vec![goal(420.0)]);
}

/// One truth goal cannot be claimed twice. One run's windows are disjoint
/// (D4), so this only bites when a rule change breaks that — which is exactly
/// when a silently doubled recall would be believed.
#[test]
fn two_windows_over_one_goal_are_one_hit_and_one_false_positive() {
    let r = score(
        &[goal(400.0)],
        &[
            found_goal(500.0, 150.0, GoalTier::High),
            found_goal(520.0, 150.0, GoalTier::High),
        ],
    );
    assert_eq!(
        (r.tally.goals.tp, r.tally.goals.fp, r.tally.goals.misses),
        (1, 1, 0)
    );
    assert_eq!(
        r.false_goals[0].seconds, 520.0,
        "the earlier window took it"
    );
}

/// A source is part of the identity: the same seconds on another file is not
/// the same moment.
#[test]
fn a_goal_on_another_source_does_not_match() {
    let mut elsewhere = goal(400.0);
    elsewhere.source_index = 1;
    let r = score(&[elsewhere], &[found_goal(500.0, 150.0, GoalTier::High)]);
    assert_eq!(
        (r.tally.goals.tp, r.tally.goals.fp, r.tally.goals.misses),
        (0, 1, 1)
    );
}

#[test]
fn a_tier_counts_every_truth_goal_it_did_not_find() {
    let r = score(
        &[goal(400.0), goal(900.0)],
        &[
            found_goal(500.0, 150.0, GoalTier::High),
            found_goal(1000.0, 150.0, GoalTier::Quiet),
        ],
    );
    assert_eq!(r.tally.goals.tp, 2);
    assert_eq!((r.tally.goals_high.tp, r.tally.goals_high.misses), (1, 1));
    assert_eq!((r.tally.goals_quiet.tp, r.tally.goals_quiet.misses), (1, 1));
}

#[test]
fn the_period_tolerance_is_inclusive() {
    let at = |error: f64| {
        score(
            &[tag(100.0, TruthKind::PeriodStart)],
            &[found_period(100.0 + error, SuggestionKind::PeriodStart)],
        )
        .tally
        .period_start
    };
    assert_eq!(at(PERIOD_TOLERANCE).tp, 1, "exactly 10.0 s matches");
    assert_eq!(at(-PERIOD_TOLERANCE).tp, 1, "and 10.0 s early does too");
    let just_over = at(PERIOD_TOLERANCE + 0.1);
    assert_eq!((just_over.tp, just_over.fp, just_over.misses), (0, 1, 1));
}

#[test]
fn a_period_start_never_matches_a_period_end() {
    let r = score(
        &[tag(100.0, TruthKind::PeriodEnd)],
        &[found_period(100.0, SuggestionKind::PeriodStart)],
    );
    assert_eq!(r.tally.period_end.misses, 1);
    assert_eq!(r.tally.period_start.fp, 1);
}

#[test]
fn period_pairing_takes_the_nearer_detection() {
    let r = score(
        &[tag(100.0, TruthKind::PeriodStart)],
        &[
            found_period(96.0, SuggestionKind::PeriodStart),
            found_period(102.0, SuggestionKind::PeriodStart),
        ],
    );
    assert_eq!((r.tally.period_start.tp, r.tally.period_start.fp), (1, 1));
    assert_eq!(r.period_hits[0].error, 2.0, "the nearer one paired");
    assert_eq!(r.false_periods[0].seconds, 96.0);
}

/// Each side is used once, so two tags and two detections pair up rather than
/// both crowding onto the nearest.
#[test]
fn period_pairing_uses_each_side_once() {
    let r = score(
        &[
            tag(100.0, TruthKind::PeriodStart),
            tag(106.0, TruthKind::PeriodStart),
        ],
        &[
            found_period(101.0, SuggestionKind::PeriodStart),
            found_period(105.0, SuggestionKind::PeriodStart),
        ],
    );
    assert_eq!(
        (
            r.tally.period_start.tp,
            r.tally.period_start.fp,
            r.tally.period_start.misses
        ),
        (2, 0, 0)
    );
}

#[test]
fn nothing_detected_is_no_precision_rather_than_zero() {
    let r = score(&[goal(400.0)], &[]);
    assert_eq!(r.tally.goals.precision(), None);
    assert_eq!(r.tally.goals.recall(), Some(0.0));
    assert_eq!(show_rate(r.tally.goals.precision()), "n/a");
    assert_eq!(show_rate(r.tally.goals.recall()), "0.00");
}

#[test]
fn the_seek_bar_judges_the_high_tier_only() {
    // `at` is K − 5, so seek is max(K − 15, window start) = K − 15 = 485.
    let high = score(&[goal(490.0)], &[found_goal(500.0, 150.0, GoalTier::High)]);
    assert_eq!(high.goal_hits[0].seek_ok, Some(true));
    assert_eq!(high.seek(), (1, 1));

    // 400 is inside the window but 85 s before the seek point.
    let early = score(&[goal(400.0)], &[found_goal(500.0, 150.0, GoalTier::High)]);
    assert_eq!(early.goal_hits[0].seek_ok, Some(false));
    assert_eq!(early.seek(), (0, 1));

    let quiet = score(&[goal(490.0)], &[found_goal(500.0, 150.0, GoalTier::Quiet)]);
    assert_eq!(quiet.goal_hits[0].seek_ok, None);
    assert_eq!(quiet.seek(), (0, 0), "a quiet goal has no `at` to seek to");
}

#[test]
fn a_restart_is_a_diagnostic_and_is_not_scored() {
    let r = score(&[tag(460.0, TruthKind::Restart), goal(400.0)], &[]);
    assert_eq!(r.tally.goals.misses, 1);
    assert_eq!(r.missed_goals, vec![goal(400.0)]);
}

// -------------------------------------------------------- audio coverage

fn cheer(onset: f64) -> Cheer {
    Cheer {
        onset,
        duration: 1.2,
        peak_db: 30.0,
    }
}

#[test]
fn a_cheer_covers_a_goal_up_to_the_tolerance_and_no_further() {
    // The offset is signed the way the report reads it: positive means the
    // cheer came **after** the tag, which is what a goal's cheer does. A sign
    // flipped here would move every DIAG line and the headline recall with it.
    let (_, offset) = nearest(&[cheer(101.0)], 100.0, |c| c.onset).expect("a cheer");
    assert_eq!(offset, 1.0);

    let goals = [goal(100.0), goal(500.0)];
    let inside = cheer(100.0 + CHEER_TOLERANCE);
    let outside = cheer(500.0 + CHEER_TOLERANCE + 0.1);
    assert_eq!(cheer_coverage(&goals, 0, &[inside, outside]), (1, 2));
    // A goal on another source is not this source's to cover.
    assert_eq!(cheer_coverage(&goals, 1, &[inside, outside]), (0, 0));
}

// ------------------------------------------------------------ kickoffs.txt

#[test]
fn kickoffs_parses_comments_blanks_and_a_missing_line() {
    let restarts = parse_kickoffs(
        "# match A restarts\n\
         1 07:13\n\
         \n\
         # missing\n\
         2 12:04   # the second half\n",
    )
    .expect("a well-formed notes file");
    assert_eq!(restarts.len(), 2);
    assert_eq!((restarts[0].source_index, restarts[0].seconds), (0, 433.0));
    assert_eq!((restarts[1].source_index, restarts[1].seconds), (1, 724.0));
    assert_eq!(
        restarts[1].line, 5,
        "the line number survives for diagnostics"
    );
}

#[test]
fn a_line_waiting_for_its_time_is_not_a_restart_and_not_an_error() {
    // The template hands the coach a video number under each goal and a blank
    // where the time goes. A half-filled file is the normal state of one while
    // a match is being worked through, and it must not stop the run.
    let restarts = parse_kickoffs("1 07:13\n2\n2  \n").expect("a half-filled notes file");
    assert_eq!(restarts.len(), 1);
    assert_eq!(restarts[0].seconds, 433.0);
}

#[test]
fn a_malformed_kickoffs_line_names_its_line() {
    for (text, line) in [
        ("1 07:13\n2 7m13\n", 2),
        ("1 07:13\n\n\n0 07:13\n", 4),
        ("x 07:13\n", 1),
        ("1 07:13 2 08:00\n", 1),
        ("1 7:130\n", 1),
        ("1 07:60\n", 1),
    ] {
        match parse_kickoffs(text) {
            Err(TruthError::Kickoffs { line: got, .. }) => {
                assert_eq!(got, line, "for {text:?}");
            }
            other => panic!("{text:?} should have been refused, got {other:?}"),
        }
    }
}

// ------------------------------------------------------- COACH_GROUND_TRUTH

#[test]
fn folders_name_matches_by_position_unless_labelled() {
    assert_eq!(
        folders("/a:/b:/c").unwrap(),
        vec![
            ("A".to_string(), "/a".into()),
            ("B".to_string(), "/b".into()),
            ("C".to_string(), "/c".into()),
        ]
    );
    assert_eq!(
        folders("B=/one:A=/two:C=/three").unwrap(),
        vec![
            ("B".to_string(), "/one".into()),
            ("A".to_string(), "/two".into()),
            ("C".to_string(), "/three".into()),
        ]
    );
    // Not a label shape, so it stays part of the path and the name is
    // positional — nothing identifying can be smuggled into the report.
    assert_eq!(
        folders("/home/games/a=b").unwrap(),
        vec![("A".to_string(), "/home/games/a=b".into())]
    );
    assert!(folders("A=/one:A=/two").is_err());
    assert!(folders("").is_err());
}

/// The app's readout shows tenths while paused, so a coach reading a restart
/// off it writes down what they see. Whole seconds still parse.
#[test]
fn a_restart_may_carry_the_tenth_the_readout_showed() {
    let restarts = parse_kickoffs("1 11:57.5\n2 3:04\n").expect("both lines parse");
    assert_eq!(restarts[0].seconds, 717.5);
    assert_eq!(restarts[1].seconds, 184.0);

    for bad in ["1 11:57.50", "1 11:5.5", "1 11:60.5"] {
        assert!(parse_kickoffs(bad).is_err(), "{bad} should be refused");
    }
}
