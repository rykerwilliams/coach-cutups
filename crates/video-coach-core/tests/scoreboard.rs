//! The match clock, the score, and the interpretation that drives both.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::compilation_schedule;
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Clip, Inset, Project, SourceRef};
use video_coach_core::scoreboard::{
    chapter_events, format_clock, interpret, labelled_events, scoreboard_state, AbsoluteMatchEvent,
    ClockDisplay, LabelledEvent, MatchEventKind, MatchFormat, PeriodRole, ReelEnd, ReelTrimError,
    ScoreboardConfig, ScoreboardContext, ScoreboardState, TeamConfig,
};
use video_coach_core::stroke::Rgba;

const HALF: f64 = 45.0 * 60.0;

fn soccer() -> ScoreboardConfig {
    config(MatchFormat::default())
}

fn config(format: MatchFormat) -> ScoreboardConfig {
    ScoreboardConfig {
        home: TeamConfig::new("H", Rgba::RED, Rgba::RED),
        away: TeamConfig::new("A", Rgba::RED, Rgba::RED),
        format,
        auto_back_anchor_p1: false,
    }
}

fn periods(count: u32, seconds: u32) -> MatchFormat {
    MatchFormat {
        regulation_periods: count,
        regulation_period_seconds: seconds,
        ..MatchFormat::default()
    }
}

fn event(kind: MatchEventKind, at: f64) -> AbsoluteMatchEvent {
    AbsoluteMatchEvent {
        id: Some(Uuid::new_v4()),
        kind,
        abs_seconds: at,
    }
}

/// Start/stops at `times`, in tag order.
fn starts(times: &[f64]) -> Vec<AbsoluteMatchEvent> {
    times
        .iter()
        .map(|&t| event(MatchEventKind::StartStop, t))
        .collect()
}

fn clock(now: f64, config: &ScoreboardConfig, events: &[AbsoluteMatchEvent]) -> ClockDisplay {
    scoreboard_state(now, config, events)
        .unwrap_or_else(|| panic!("no scoreboard at {now}"))
        .clock
}

fn running(seconds: f64) -> ClockDisplay {
    ClockDisplay::Running { seconds }
}

// ------------------------------------------------------------ match format

#[test]
fn a_format_counts_its_periods_and_the_start_stops_they_need() {
    let soccer = MatchFormat::default();
    assert_eq!(soccer.total_periods(), 2);
    assert_eq!(soccer.expected_start_stop_events(), 4);

    let quarters = periods(4, 12 * 60);
    assert_eq!(quarters.total_periods(), 4);
    assert_eq!(quarters.expected_start_stop_events(), 8);
}

#[test]
fn overtime_periods_follow_the_regulation_ones_and_keep_their_own_length() {
    let f = MatchFormat {
        overtime_periods: 1,
        ..MatchFormat::default()
    };
    assert_eq!(f.total_periods(), 3);
    assert_eq!(f.expected_start_stop_events(), 6);
    assert!(!f.is_overtime(1));
    assert!(f.is_overtime(2));
    assert_eq!(f.period_seconds(1), HALF);
    assert_eq!(f.period_seconds(2), 15.0 * 60.0);
}

/// Two regulation periods are halves; anything else is numbered periods, and
/// overtime is always OT.
#[test]
fn period_names_follow_the_format() {
    let soccer = MatchFormat::default();
    assert_eq!(soccer.period_name(0), "1H");
    assert_eq!(soccer.period_name(1), "2H");

    let quarters = periods(4, 12 * 60);
    assert_eq!(quarters.period_name(0), "P1");
    assert_eq!(quarters.period_name(3), "P4");
    assert_eq!(periods(1, 60 * 60).period_name(0), "P1");

    let with_ot = MatchFormat {
        overtime_periods: 2,
        ..MatchFormat::default()
    };
    assert_eq!(with_ot.period_name(2), "OT1");
    assert_eq!(with_ot.period_name(3), "OT2");
}

#[test]
fn only_soccer_s_first_break_is_half_time() {
    assert_eq!(MatchFormat::default().break_label(0), "HT");
    let quarters = periods(4, 12 * 60);
    for period in 0..3 {
        assert_eq!(quarters.break_label(period), "BREAK");
    }
}

// ------------------------------------------------------------ clock labels

#[test]
fn the_clock_reads_mm_ss_and_drops_the_fraction() {
    for (seconds, main) in [
        (0.0, "00:00"),
        (5.0, "00:05"),
        (125.0, "02:05"),
        (125.9, "02:05"),
    ] {
        let labels = format_clock(running(seconds));
        assert_eq!(labels.main, main);
        assert_eq!(labels.trailing, "");
    }
}

#[test]
fn stoppage_holds_the_period_s_end_and_counts_up_in_the_tail() {
    let labels = format_clock(ClockDisplay::Stoppage {
        base: 2700.0,
        plus: 47.0,
    });
    assert_eq!(
        (labels.main.as_str(), labels.trailing.as_str()),
        ("45:00", "+0:47")
    );

    let labels = format_clock(ClockDisplay::Stoppage {
        base: 5400.0,
        plus: 305.0,
    });
    assert_eq!(
        (labels.main.as_str(), labels.trailing.as_str()),
        ("90:00", "+5:05")
    );
}

#[test]
fn a_break_and_full_time_read_as_words() {
    assert_eq!(format_clock(ClockDisplay::OnBreak("HT")).main, "HT");
    assert_eq!(format_clock(ClockDisplay::OnBreak("BREAK")).main, "BREAK");
    assert_eq!(format_clock(ClockDisplay::Fulltime).main, "FT");
    assert_eq!(format_clock(ClockDisplay::Fulltime).trailing, "");
}

// -------------------------------------------------------------- interpret

/// Positional, and by absolute time rather than tag order: the coach can tag
/// half-time after scrubbing back for a goal.
#[test]
fn start_stops_alternate_start_and_end_in_time_order() {
    let events = starts(&[2900.0, 0.0, 2750.0]);
    let interp = interpret(&events, &soccer());
    assert_eq!(
        interp.iter().map(|e| e.role).collect::<Vec<_>>(),
        [
            PeriodRole::Start(0),
            PeriodRole::End(0),
            PeriodRole::Start(1)
        ]
    );
    // Each role carries the record it came from, so the panel never indexes a
    // second, separately-filtered list.
    assert_eq!(interp[0].id, events[1].id);
    assert_eq!(interp[1].id, events[2].id);
    assert_eq!(interp[2].id, events[0].id);
}

#[test]
fn goals_are_not_interpreted() {
    let mut events = starts(&[0.0]);
    events.push(event(MatchEventKind::HomeGoal, 100.0));
    assert_eq!(interpret(&events, &soccer()).len(), 1);
}

/// Two start/stops on the same frame keep the order they were tagged in.
#[test]
fn events_sharing_an_absolute_time_break_the_tie_on_tag_order() {
    let events = starts(&[10.0, 10.0]);
    let interp = interpret(&events, &soccer());
    assert_eq!(interp[0].id, events[0].id);
    assert_eq!(interp[1].id, events[1].id);
}

/// The format's capacity is total: a fifth start/stop in a two-period match
/// gets no role. (The UI disables the action at the cap; a format shrunk below
/// what is already tagged is how the list gets here.)
#[test]
fn start_stops_past_the_format_s_capacity_get_no_role() {
    let events = starts(&[0.0, 2750.0, 2900.0, 5800.0, 6000.0]);
    let interp = interpret(&events, &soccer());
    assert_eq!(interp.len(), 4);
    assert_eq!(interp.last().unwrap().role, PeriodRole::End(1));
}

// ------------------------------------------------------------- the clock

#[test]
fn there_is_no_scoreboard_before_the_match_starts() {
    assert!(scoreboard_state(100.0, &soccer(), &[]).is_none());
    assert!(scoreboard_state(5.0, &soccer(), &starts(&[10.0])).is_none());
}

#[test]
fn the_first_period_runs_from_its_start_then_enters_stoppage() {
    let events = starts(&[0.0]);
    assert_eq!(clock(100.0, &soccer(), &events), running(100.0));
    assert_eq!(clock(HALF, &soccer(), &events), running(HALF));
    assert_eq!(
        clock(2750.0, &soccer(), &events),
        ClockDisplay::Stoppage {
            base: HALF,
            plus: 50.0
        }
    );
}

#[test]
fn the_clock_reads_half_time_between_the_periods() {
    let events = starts(&[0.0, 2750.0]);
    assert_eq!(
        clock(2750.0, &soccer(), &events),
        ClockDisplay::OnBreak("HT")
    );
    assert_eq!(
        clock(2800.0, &soccer(), &events),
        ClockDisplay::OnBreak("HT")
    );
}

/// The second half counts from 45:00, not from its own start: the clock is
/// match time, and the gap at half-time is not part of it.
#[test]
fn the_second_period_counts_the_first_one_too() {
    let events = starts(&[0.0, 2750.0, 2900.0]);
    assert_eq!(clock(3000.0, &soccer(), &events), running(2800.0));
    assert_eq!(
        clock(5650.0, &soccer(), &events),
        ClockDisplay::Stoppage {
            base: 5400.0,
            plus: 50.0
        }
    );
}

#[test]
fn the_last_period_s_end_is_full_time() {
    let events = starts(&[0.0, 2750.0, 2900.0, 5800.0]);
    assert_eq!(clock(6000.0, &soccer(), &events), ClockDisplay::Fulltime);
}

#[test]
fn a_quarters_format_breaks_between_every_pair_and_accumulates() {
    let quarters = config(periods(4, 12 * 60));
    let events = starts(&[0.0, 720.0, 800.0, 1600.0, 1700.0]);

    assert_eq!(
        clock(750.0, &quarters, &events[..2]),
        ClockDisplay::OnBreak("BREAK")
    );
    // 30 s into Q2: one quarter behind it, so 12:30 of match time.
    assert_eq!(clock(830.0, &quarters, &events[..3]), running(750.0));
    // 50 s past Q3's regulation end: three quarters behind it.
    assert_eq!(
        clock(2470.0, &quarters, &events),
        ClockDisplay::Stoppage {
            base: 2160.0,
            plus: 50.0
        }
    );
    // Every pair tagged: the last end is full time.
    let all = starts(&[0.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 7000.0]);
    assert_eq!(clock(8000.0, &quarters, &all), ClockDisplay::Fulltime);
}

#[test]
fn overtime_carries_on_past_regulation_at_its_own_length() {
    let cfg = config(MatchFormat {
        overtime_periods: 2,
        ..MatchFormat::default()
    });
    // 2H ran to 10800 (deep into stoppage); OT1 starts at 10900. Match time is
    // the two regulation periods plus 60 s, not the wall clock.
    let events = starts(&[0.0, 2700.0, 2800.0, 10800.0, 10900.0]);
    assert_eq!(clock(10960.0, &cfg, &events), running(90.0 * 60.0 + 60.0));

    let all = starts(&[0.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 7000.0]);
    assert_eq!(clock(8000.0, &cfg, &all), ClockDisplay::Fulltime);
}

// -------------------------------------------------------------- the score

#[test]
fn goals_inside_the_match_count_once_they_have_happened() {
    let mut events = starts(&[0.0]);
    events.push(event(MatchEventKind::HomeGoal, 100.0));
    events.push(event(MatchEventKind::HomeGoal, 500.0));
    events.push(event(MatchEventKind::AwayGoal, 700.0));
    events.push(event(MatchEventKind::AwayGoal, 1500.0));

    let s = scoreboard_state(1000.0, &soccer(), &events).unwrap();
    assert_eq!((s.home_score, s.away_score), (2, 1));
}

/// A goal tagged before kick-off or after the final whistle is not part of the
/// match — a warm-up shot, or the next fixture on the same tape.
#[test]
fn goals_outside_the_match_do_not_count() {
    let mut before = starts(&[0.0]);
    before.push(event(MatchEventKind::HomeGoal, -10.0));
    assert_eq!(
        scoreboard_state(1000.0, &soccer(), &before)
            .unwrap()
            .home_score,
        0
    );

    let mut after = starts(&[0.0, 2750.0, 2900.0, 5800.0]);
    after.push(event(MatchEventKind::HomeGoal, 6000.0));
    assert_eq!(
        scoreboard_state(10_000.0, &soccer(), &after)
            .unwrap()
            .home_score,
        0
    );
}

/// The upper bound is open until the start/stops fill the format, so a goal in
/// a half the coach never tagged the end of still counts.
#[test]
fn a_late_goal_counts_while_the_match_is_only_part_tagged() {
    let mut events = starts(&[0.0, 2750.0, 2900.0]);
    events.push(event(MatchEventKind::HomeGoal, 6000.0));
    assert_eq!(
        scoreboard_state(10_000.0, &soccer(), &events)
            .unwrap()
            .home_score,
        1
    );
}

#[test]
fn a_goal_at_half_time_counts() {
    let mut events = starts(&[0.0, 2750.0]);
    events.push(event(MatchEventKind::HomeGoal, 2780.0));
    let s = scoreboard_state(2800.0, &soccer(), &events).unwrap();
    assert_eq!(s.clock, ClockDisplay::OnBreak("HT"));
    assert_eq!(s.home_score, 1);
}

#[test]
fn goals_count_across_every_period_of_a_quarters_match() {
    let quarters = config(periods(4, 12 * 60));
    let mut events = starts(&[0.0, 720.0, 800.0, 1600.0, 1700.0, 2500.0]);
    events.push(event(MatchEventKind::HomeGoal, 300.0));
    events.push(event(MatchEventKind::AwayGoal, 1900.0));

    let s = scoreboard_state(2600.0, &quarters, &events).unwrap();
    assert_eq!((s.home_score, s.away_score), (1, 1));
}

// -------------------------------------------------------- the back anchor

fn back_anchored() -> ScoreboardConfig {
    ScoreboardConfig {
        auto_back_anchor_p1: true,
        ..soccer()
    }
}

/// Before any start/stop is tagged the derived start sits at absolute 0, so
/// there is a clock through the half the coach most wants one — counting from
/// the beginning of the footage.
#[test]
fn a_back_anchored_match_runs_from_the_footage_s_start_until_half_time_is_tagged() {
    assert_eq!(clock(600.0, &back_anchored(), &[]), running(600.0));
    // Nothing tagged means nothing to align to, so stoppage still behaves.
    assert_eq!(
        clock(2750.0, &back_anchored(), &[]),
        ClockDisplay::Stoppage {
            base: HALF,
            plus: 50.0
        }
    );
}

/// Tagging half-time defines it as 45:00 and back-dates kick-off to before the
/// footage: the derived start is a whole period earlier.
#[test]
fn tagging_half_time_back_dates_the_derived_kick_off() {
    let cfg = back_anchored();
    let events = starts(&[38.0 * 60.0]);
    assert_eq!(clock(0.0, &cfg, &events), running(7.0 * 60.0));
    assert_eq!(clock(19.0 * 60.0, &cfg, &events), running(26.0 * 60.0));
    assert_eq!(
        clock(38.0 * 60.0, &cfg, &events),
        ClockDisplay::OnBreak("HT")
    );

    let interp = interpret(&events, &cfg);
    assert_eq!(interp[0].id, None, "the derived start has no record");
    assert_eq!(interp[0].abs_seconds, 38.0 * 60.0 - HALF);
    assert_eq!(interp[1].id, events[0].id);
}

/// The whole point of the anchor: the tagged end **is** the end of the period,
/// so the clock carries exactly one period's worth there and never enters
/// stoppage on the way.
/// macOS offset the displayed number instead, which read 50:00 while still
/// counted as running.
#[test]
fn a_back_anchored_first_period_ends_at_the_period_length_and_never_reaches_stoppage() {
    let cfg = back_anchored();
    let p1_end = 2280.5;
    let events = starts(&[p1_end]);

    // The last instant of the half carries exactly one period, and the next one
    // is the break: there is no room between them for stoppage.
    let ClockDisplay::Running { seconds } = clock(p1_end - 1e-9, &cfg, &events) else {
        panic!("the half should still be running")
    };
    assert!((seconds - HALF).abs() < 1e-6, "got {seconds}");
    assert_eq!(clock(p1_end, &cfg, &events), ClockDisplay::OnBreak("HT"));

    // What the cell actually *displays*: `format_clock` truncates, so the half
    // runs out at 44:59 and turns straight over to the break. No frame of it
    // ever reads 45:00, and pinning the payload alone hid that.
    assert_eq!(
        format_clock(clock(p1_end - 1e-9, &cfg, &events)).main,
        "44:59"
    );
    assert_eq!(format_clock(clock(p1_end, &cfg, &events)).main, "HT");

    let mut now = 0.0;
    while now < p1_end {
        assert!(
            matches!(clock(now, &cfg, &events), ClockDisplay::Running { .. }),
            "stoppage at {now}"
        );
        now += 5.0;
    }
}

/// Half-time tagged *later* than a period length means kick-off was inside the
/// footage: the derived start moves forward, and there is no clock before it.
#[test]
fn a_back_anchored_kick_off_can_land_inside_the_footage() {
    let cfg = back_anchored();
    let events = starts(&[50.0 * 60.0]);
    assert!(scoreboard_state(4.0 * 60.0, &cfg, &events).is_none());
    assert_eq!(clock(5.0 * 60.0, &cfg, &events), running(0.0));
    assert_eq!(clock(10.0 * 60.0, &cfg, &events), running(5.0 * 60.0));
}

/// The anchor is period 1's business only; later periods are read from their
/// own tags, as always.
#[test]
fn the_anchor_does_not_move_the_later_periods() {
    let cfg = back_anchored();
    let events = starts(&[38.0 * 60.0, 50.0 * 60.0, 95.0 * 60.0]);
    assert_eq!(clock(60.0 * 60.0, &cfg, &events), running(55.0 * 60.0));
    assert_eq!(clock(96.0 * 60.0, &cfg, &events), ClockDisplay::Fulltime);
}

/// A back-anchored match is fully tagged with one start/stop fewer — the
/// derived start replaces the kick-off tag — and every tagged event keeps its
/// role. The anchor costs no *stored* slot: it is not a record at all.
#[test]
fn a_fully_tagged_back_anchored_match_keeps_every_tagged_event() {
    let cfg = back_anchored();
    let events = starts(&[38.0 * 60.0, 50.0 * 60.0, 95.0 * 60.0]);
    let interp = interpret(&events, &cfg);
    assert_eq!(interp.len(), cfg.format.expected_start_stop_events());
    assert_eq!(
        interp.iter().map(|e| e.id).collect::<Vec<_>>(),
        [None, events[0].id, events[1].id, events[2].id]
    );
}

/// With the anchor on **and** a kick-off tagged there is one start/stop too
/// many for the format, and the last one gets no role. Assigning it one would
/// name a period the format does not have and leave the clock running past
/// full time; the record survives, so turning the anchor off restores it.
#[test]
fn the_anchor_plus_a_full_set_of_tags_drops_the_last_role_not_the_record() {
    let cfg = back_anchored();
    let events = starts(&[0.0, 2750.0, 2900.0, 5800.0]);
    let interp = interpret(&events, &cfg);

    assert_eq!(interp.len(), 4);
    assert_eq!(interp.last().unwrap().role, PeriodRole::End(1));
    assert_eq!(clock(6000.0, &cfg, &events), ClockDisplay::Fulltime);

    let off = ScoreboardConfig {
        auto_back_anchor_p1: false,
        ..cfg
    };
    assert_eq!(
        interpret(&events, &off)
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        events.iter().map(|e| e.id).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------ the project

fn project_with_sources(durations: &[f64]) -> Project {
    let mut p = Project::new("p");
    for (i, &duration) in durations.iter().enumerate() {
        p.source_videos.push(SourceRef {
            relative_path: format!("{i}.mp4"),
            display_name: format!("{i}"),
            duration_seconds: duration,
            display_aspect: 16.0 / 9.0,
        });
    }
    p.scoreboard = Some(soccer());
    p
}

#[test]
fn tagging_appends_a_record_with_its_own_id_and_deleting_removes_it() {
    let mut p = project_with_sources(&[60.0]);
    let first = p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
    let second = p.append_match_event(MatchEventKind::HomeGoal, 0, 200.0);
    assert_ne!(first, second);
    assert_eq!(p.match_events.len(), 2);
    assert_eq!(p.match_events[1].source_seconds, 200.0);

    assert_eq!(p.delete_match_event(first).unwrap().id, first);
    assert_eq!(p.match_events.len(), 1);
    assert!(p.delete_match_event(first).is_none());
}

// ------------------------------------------------------------ reel trims

fn trims(p: &Project, id: Uuid) -> (Option<f64>, Option<f64>) {
    let m = p.match_events.iter().find(|m| m.id == id).unwrap();
    (m.reel_lead_in, m.reel_tail)
}

/// A trim is set from a position and stored relative to the goal: the lead-in
/// as `goal − at`, the tail as `at − goal`, each side on its own.
#[test]
fn a_reel_trim_is_stored_relative_to_the_goal() {
    let mut p = project_with_sources(&[600.0, 600.0]);
    let goal = p.append_match_event(MatchEventKind::AwayGoal, 1, 100.0);

    p.set_reel_trim(goal, ReelEnd::Start, Some((1, 88.0)))
        .unwrap();
    assert_eq!(trims(&p, goal), (Some(12.0), None));
    p.set_reel_trim(goal, ReelEnd::End, Some((1, 104.5)))
        .unwrap();
    assert_eq!(trims(&p, goal), (Some(12.0), Some(4.5)));
}

/// `None` resets one side to the default and leaves the other alone.
#[test]
fn resetting_one_side_of_a_reel_trim_leaves_the_other() {
    let mut p = project_with_sources(&[600.0]);
    let goal = p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
    p.set_reel_trim(goal, ReelEnd::Start, Some((0, 90.0)))
        .unwrap();
    p.set_reel_trim(goal, ReelEnd::End, Some((0, 110.0)))
        .unwrap();

    p.set_reel_trim(goal, ReelEnd::Start, None).unwrap();
    assert_eq!(trims(&p, goal), (None, Some(10.0)));
    p.set_reel_trim(goal, ReelEnd::End, None).unwrap();
    assert_eq!(trims(&p, goal), (None, None));
}

#[test]
fn a_reel_trim_refuses_anything_but_a_goal() {
    let mut p = project_with_sources(&[600.0]);
    let start_stop = p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    for id in [start_stop, Uuid::new_v4()] {
        assert_eq!(
            p.set_reel_trim(id, ReelEnd::Start, Some((0, 5.0))),
            Err(ReelTrimError::NotAGoal)
        );
        assert_eq!(
            p.set_reel_trim(id, ReelEnd::End, None),
            Err(ReelTrimError::NotAGoal)
        );
    }
    assert_eq!(trims(&p, start_stop), (None, None));
}

#[test]
fn a_reel_trim_refuses_a_position_on_another_source() {
    let mut p = project_with_sources(&[600.0, 600.0]);
    let goal = p.append_match_event(MatchEventKind::HomeGoal, 1, 100.0);
    assert_eq!(
        p.set_reel_trim(goal, ReelEnd::Start, Some((0, 90.0))),
        Err(ReelTrimError::OtherSource)
    );
    assert_eq!(
        p.set_reel_trim(goal, ReelEnd::End, Some((0, 110.0))),
        Err(ReelTrimError::OtherSource)
    );
    assert_eq!(trims(&p, goal), (None, None));
}

/// A start must be before the goal and an end after it; the goal's own
/// instant is neither.
#[test]
fn a_reel_trim_refuses_the_wrong_side_of_the_goal() {
    let mut p = project_with_sources(&[600.0]);
    let goal = p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
    for at in [100.0, 101.0, f64::NAN] {
        assert_eq!(
            p.set_reel_trim(goal, ReelEnd::Start, Some((0, at))),
            Err(ReelTrimError::WrongSideOfGoal(ReelEnd::Start))
        );
    }
    for at in [100.0, 99.0, f64::NAN] {
        assert_eq!(
            p.set_reel_trim(goal, ReelEnd::End, Some((0, at))),
            Err(ReelTrimError::WrongSideOfGoal(ReelEnd::End))
        );
    }
    assert_eq!(
        ReelTrimError::WrongSideOfGoal(ReelEnd::End).to_string(),
        "the reel must end after the goal"
    );
    assert_eq!(trims(&p, goal), (None, None));
}

/// The mutator has no cap: the command refuses out loud instead. macOS's
/// silently did nothing, which is worse than a refusal.
#[test]
fn tagging_past_the_cap_still_stores_the_record() {
    let mut p = project_with_sources(&[60.0]);
    for i in 0..6 {
        p.append_match_event(MatchEventKind::StartStop, 0, f64::from(i) * 10.0);
    }
    assert_eq!(p.match_events.len(), 6);
}

/// One cap rule for the command that refuses and the panel that disables:
/// records only, so the derived back-anchor never takes a place, and no cap at
/// all without a format to cap against.
#[test]
fn the_cap_counts_stored_start_stops_against_the_format() {
    let mut p = project_with_sources(&[60.0]);
    p.scoreboard = Some(ScoreboardConfig {
        auto_back_anchor_p1: true,
        ..soccer()
    });
    for i in 0..3 {
        p.append_match_event(MatchEventKind::StartStop, 0, f64::from(i) * 10.0);
        p.append_match_event(MatchEventKind::HomeGoal, 0, f64::from(i) * 10.0);
        assert!(!p.start_stops_at_cap());
    }
    p.append_match_event(MatchEventKind::StartStop, 0, 40.0);
    assert_eq!(p.start_stop_count(), 4);
    assert!(p.start_stops_at_cap());

    p.scoreboard = None;
    assert!(!p.start_stops_at_cap());
}

/// Events are positioned on a source, and the clock runs on the concatenation
/// of all of them.
#[test]
fn the_context_projects_events_onto_the_concat_timeline() {
    let mut p = project_with_sources(&[60.0, 60.0]);
    p.append_match_event(MatchEventKind::StartStop, 1, 5.0);
    let ctx = ScoreboardContext::for_project(&p).unwrap();

    assert_eq!(ctx.state_at(1, 10.0).unwrap().clock, running(5.0));
    // Kick-off is 65 s into the concatenation, so nothing on source 0 has one.
    assert!(ctx.state_at(0, 30.0).is_none());
    assert_eq!(ctx.config().home.name, "H");
}

#[test]
fn there_is_no_context_without_a_scoreboard() {
    let mut p = project_with_sources(&[60.0]);
    p.scoreboard = None;
    assert!(ScoreboardContext::for_project(&p).is_none());
}

// --------------------------------------------------------- the pause test

fn clip_with_events(events: Vec<CommentaryEvent>, recording_duration: f64) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: 100.0,
        recording_duration,
        recording_filename: "c.mkv".into(),
        events,
        show_pip: true,
        inset: Inset::Camera,
        sort_index: 0,
        created_at: "2026-09-20T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// The frame shown `record_time` seconds into the first entry.
fn state_at_record_time(
    ctx: &ScoreboardContext,
    compilation: &video_coach_core::export::Compilation,
    record_time: f64,
) -> ScoreboardState {
    let entry = &compilation.plan.entries[0];
    let frame = entry.start_frame + (record_time * 30.0).round() as usize;
    let spec = compilation.frames[frame];
    assert_eq!(spec.entry, 0);
    ctx.state_at(entry.source_index, spec.source_time)
        .expect("the match has started")
}

/// **The pause test.** The match clock is the *source* video's time, so a clip
/// that pauses for 20 s shows the same clock before and after the pause. macOS
/// computed it as a per-clip constant plus the commentary's wall clock, which
/// put the clock that far ahead of the footage — and since every recording
/// opens with a pause, that was nearly always (BACKLOG #27).
#[test]
fn a_pause_inside_a_clip_holds_the_match_clock() {
    let mut p = project_with_sources(&[1000.0]);
    // Kick-off 10 s into the source, so the clock reads 90 s at source 100.
    p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    p.clips.push(clip_with_events(
        vec![
            CommentaryEvent::new(2.0, EventKind::Pause { source_time: 102.0 }),
            CommentaryEvent::new(22.0, EventKind::Play { source_time: 102.0 }),
        ],
        30.0,
    ));

    let ctx = ScoreboardContext::for_project(&p).unwrap();
    let compilation = compilation_schedule(&p, &ExportTarget::AllClips);

    let at_pause = state_at_record_time(&ctx, &compilation, 2.0);
    assert_eq!(at_pause.clock, running(92.0));
    // 20 s of commentary later, the footage has not moved and neither has the
    // clock.
    assert_eq!(
        state_at_record_time(&ctx, &compilation, 22.0).clock,
        at_pause.clock
    );
    // Once it plays on, the clock advances with the footage again.
    assert_eq!(
        state_at_record_time(&ctx, &compilation, 27.0).clock,
        running(97.0)
    );
}

/// A goal the coach scrubbed *back* to is in the past of the frame on screen,
/// so it counts — the score follows the footage too, not the commentary.
#[test]
fn the_score_follows_the_footage_across_a_skip() {
    let mut p = project_with_sources(&[1000.0]);
    p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    p.append_match_event(MatchEventKind::HomeGoal, 0, 50.0);
    p.clips.push(clip_with_events(
        vec![CommentaryEvent::new(2.0, EventKind::Skip { delta: -60.0 })],
        10.0,
    ));

    let ctx = ScoreboardContext::for_project(&p).unwrap();
    let compilation = compilation_schedule(&p, &ExportTarget::AllClips);

    assert_eq!(state_at_record_time(&ctx, &compilation, 1.0).home_score, 1);
    // Skipped back to source 41 s: the goal at 50 s has not happened yet.
    assert_eq!(state_at_record_time(&ctx, &compilation, 3.0).home_score, 0);
}

// ------------------------------------------------------- the two wordings

/// The labels of `events`, in match order.
fn labels(events: Vec<LabelledEvent<'_>>) -> Vec<String> {
    events.into_iter().map(|e| e.label).collect()
}

/// The same events, worded twice on purpose (spec W3): the Match panel's
/// tagging vocabulary, and the file's chapter names.
#[test]
fn a_chapter_names_the_moment_where_a_panel_row_names_the_tag() {
    let mut p = project_with_sources(&[3000.0, 3000.0]);
    p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
    p.append_match_event(MatchEventKind::AwayGoal, 0, 200.0);
    p.append_match_event(MatchEventKind::StartStop, 0, 2800.0);
    p.append_match_event(MatchEventKind::StartStop, 1, 100.0);
    p.append_match_event(MatchEventKind::StartStop, 1, 2900.0);

    assert_eq!(
        labels(labelled_events(&p)),
        [
            "1H start",
            "Home goal",
            "Away goal",
            "1H end",
            "2H start",
            "2H end"
        ]
    );
    assert_eq!(
        labels(chapter_events(&p)),
        [
            "Kick-off",
            "H goal 1-0",
            "A goal 1-1",
            "Half time",
            "Second half",
            "Full time"
        ]
    );
}

/// Quarters, and a period end that is neither half time nor full time.
#[test]
fn a_chapter_follows_the_configured_format() {
    let mut p = project_with_sources(&[3000.0]);
    p.scoreboard = Some(config(periods(4, 12 * 60)));
    for at in [10.0, 700.0, 800.0, 1500.0] {
        p.append_match_event(MatchEventKind::StartStop, 0, at);
    }

    assert_eq!(
        labels(chapter_events(&p)),
        [
            "Kick-off",
            "First quarter ends",
            "Second quarter",
            "Half time"
        ]
    );
}

/// With no scoreboard there are no periods and no team names, so a chapter
/// reads exactly as the panel's row does.
#[test]
fn a_chapter_falls_back_to_the_plain_wording_with_no_scoreboard() {
    let mut p = project_with_sources(&[3000.0]);
    p.scoreboard = None;
    p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);

    assert_eq!(labels(chapter_events(&p)), ["Start/stop", "Home goal"]);
    assert_eq!(labels(chapter_events(&p)), labels(labelled_events(&p)));
}

/// A goal before any kick-off has no score to carry, and says so by leaving
/// it out rather than by claiming 0-0.
#[test]
fn a_chapter_drops_the_score_where_the_scoreboard_has_none() {
    let mut p = project_with_sources(&[3000.0]);
    p.append_match_event(MatchEventKind::AwayGoal, 0, 100.0);

    assert_eq!(labels(chapter_events(&p)), ["A goal"]);
}

// ------------------------------------------------------- editing an event

/// An edit moves the record; it never replaces it. The id is what the rows,
/// the scrubber's marks and Go all key on, and the reel trims hang off the
/// record so they follow the goal.
#[test]
fn editing_an_event_keeps_its_id_and_its_reel_trims_across_the_two_goals() {
    let mut p = project_with_sources(&[600.0, 600.0]);
    let goal = p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
    p.set_reel_trim(goal, ReelEnd::Start, Some((0, 88.0)))
        .unwrap();
    p.set_reel_trim(goal, ReelEnd::End, Some((0, 104.5)))
        .unwrap();

    assert!(p.edit_match_event(goal, MatchEventKind::AwayGoal, 1, 250.0));
    assert_eq!(p.match_events.len(), 1);
    let record = &p.match_events[0];
    assert_eq!(record.id, goal);
    assert_eq!(record.kind, MatchEventKind::AwayGoal);
    assert_eq!((record.source_index, record.source_seconds), (1, 250.0));
    // Relative trims, so they still mean the same twelve seconds before it.
    assert_eq!(trims(&p, goal), (Some(12.0), Some(4.5)));
}

/// A goal that becomes a start/stop loses its trims: they are meaningless on
/// one, and `set_reel_trim` refuses one.
#[test]
fn a_goal_that_becomes_a_start_stop_loses_its_reel_trims() {
    let mut p = project_with_sources(&[600.0]);
    let goal = p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
    p.set_reel_trim(goal, ReelEnd::Start, Some((0, 88.0)))
        .unwrap();

    assert!(p.edit_match_event(goal, MatchEventKind::StartStop, 0, 100.0));
    assert_eq!(trims(&p, goal), (None, None));
    assert!(!p.edit_match_event(Uuid::new_v4(), MatchEventKind::HomeGoal, 0, 1.0));
}

/// Re-timing an event moves it in match order, and a start/stop's role is its
/// position — so the rows either side change wording as soon as it lands.
#[test]
fn a_re_timed_start_stop_takes_the_role_of_its_new_place() {
    let mut p = project_with_sources(&[3000.0, 3000.0]);
    p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    let second = p.append_match_event(MatchEventKind::StartStop, 0, 2800.0);
    p.append_match_event(MatchEventKind::StartStop, 1, 100.0);

    let label_of = |p: &Project, id: Uuid| {
        labelled_events(p)
            .into_iter()
            .find(|e| e.event.id == id)
            .map(|e| e.label)
            .unwrap()
    };
    assert_eq!(label_of(&p, second), "1H end");
    // Past the third, it is the second half's start and the third is its end.
    assert!(p.edit_match_event(second, MatchEventKind::StartStop, 1, 200.0));
    assert_eq!(label_of(&p, second), "2H start");
    assert_eq!(
        labels(labelled_events(&p)),
        ["1H start", "1H end", "2H start"],
        "the roles are positional, so the list of labels never changes"
    );
}

/// With the back-anchor on, period 1's start is derived from the earliest
/// stored start/stop — so re-timing that one moves every later clock reading,
/// and the clock in every export with it.
#[test]
fn re_timing_the_earliest_start_stop_moves_the_whole_back_anchored_clock() {
    let mut p = project_with_sources(&[3000.0]);
    let mut config = config(periods(2, 600));
    config.auto_back_anchor_p1 = true;
    p.scoreboard = Some(config.clone());
    let first = p.append_match_event(MatchEventKind::StartStop, 0, 700.0);

    // The derived kick-off sits one period before the stored end: 100 s.
    assert_eq!(
        clock(400.0, &config, &p.absolute_match_events()),
        running(300.0)
    );
    assert!(p.edit_match_event(first, MatchEventKind::StartStop, 0, 800.0));
    assert_eq!(
        clock(400.0, &config, &p.absolute_match_events()),
        running(200.0)
    );
}
