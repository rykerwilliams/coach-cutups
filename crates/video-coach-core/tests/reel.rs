//! The goals reel's plan: which goals, each one's span, the clamps and merges
//! between them, and the text bar's line.

use video_coach_core::export::compilation_schedule;
use video_coach_core::plan::{compilation_plan, CompilationPlan, ExportTarget};
use video_coach_core::project::{Project, SourceRef};
use video_coach_core::reel::{REEL_LEAD_IN, REEL_TAIL};
use video_coach_core::scoreboard::{MatchEventKind, ReelEnd, ScoreboardConfig, TeamConfig};
use video_coach_core::stroke::Rgba;
use video_coach_core::timeline::SegmentKind;
use video_coach_core::zoom::Zoom;

const HOME: MatchEventKind = MatchEventKind::HomeGoal;
const AWAY: MatchEventKind = MatchEventKind::AwayGoal;

/// A project over sources of these lengths, with no scoreboard.
fn project(durations: &[f64]) -> Project {
    let mut p = Project::new("p");
    for (i, &duration_seconds) in durations.iter().enumerate() {
        p.source_videos.push(SourceRef {
            relative_path: format!("half{i}.mp4"),
            display_name: format!("half{i}"),
            duration_seconds,
            display_aspect: 16.0 / 9.0,
        });
    }
    p
}

fn with_scoreboard(mut p: Project) -> Project {
    p.scoreboard = Some(ScoreboardConfig {
        home: TeamConfig::new("Rovers", Rgba::RED, Rgba::RED),
        away: TeamConfig::new("United", Rgba::RED, Rgba::RED),
        format: Default::default(),
        auto_back_anchor_p1: false,
    });
    p
}

fn reel(p: &Project) -> CompilationPlan {
    compilation_plan(p, &ExportTarget::Reel)
}

/// Each entry's `(source_index, start, end)`, from its one `Play` segment.
fn spans(plan: &CompilationPlan) -> Vec<(usize, f64, f64)> {
    plan.entries
        .iter()
        .map(|e| {
            assert_eq!(e.segments.len(), 1, "one segment per entry");
            let s = e.segments[0];
            assert_eq!(s.kind, SegmentKind::Play);
            (
                e.source_index,
                s.source_start,
                s.source_start + s.out_duration,
            )
        })
        .collect()
}

fn texts(plan: &CompilationPlan) -> Vec<&str> {
    plan.entries.iter().map(|e| e.text.as_str()).collect()
}

#[test]
fn a_goal_gets_thirty_seconds_before_and_six_after() {
    assert_eq!((REEL_LEAD_IN, REEL_TAIL), (30.0, 6.0));
    let mut p = project(&[1000.0]);
    p.append_match_event(HOME, 0, 100.0);

    let plan = reel(&p);
    assert_eq!(spans(&plan), [(0, 70.0, 106.0)]);
    let entry = &plan.entries[0];
    assert_eq!(entry.clip_id, None, "a reel entry has no clip");
    assert_eq!((entry.start_frame, entry.frames), (0, 36 * 30));
}

#[test]
fn a_span_is_clamped_to_its_source() {
    let mut p = project(&[100.0]);
    p.append_match_event(HOME, 0, 10.0);
    p.append_match_event(AWAY, 0, 97.0);

    assert_eq!(spans(&reel(&p)), [(0, 0.0, 16.0), (0, 67.0, 100.0)]);
}

#[test]
fn a_span_starts_no_earlier_than_the_previous_one_ends_on_its_source() {
    let mut p = project(&[1000.0]);
    p.append_match_event(HOME, 0, 100.0);
    p.append_match_event(AWAY, 0, 120.0);

    let plan = reel(&p);
    assert_eq!(spans(&plan), [(0, 70.0, 106.0), (0, 106.0, 126.0)]);
    assert_eq!(plan.entries[1].start_frame, plan.entries[0].frames);
}

#[test]
fn the_previous_span_on_another_source_does_not_clamp() {
    let mut p = project(&[1000.0, 1000.0]);
    p.append_match_event(HOME, 0, 995.0);
    p.append_match_event(HOME, 1, 50.0);

    assert_eq!(spans(&reel(&p)), [(0, 965.0, 1000.0), (1, 20.0, 56.0)]);
}

/// Its moment is already on screen, so it extends that entry instead of
/// replaying the same footage in one of its own.
#[test]
fn a_goal_inside_the_previous_entry_extends_it() {
    let mut p = project(&[1000.0]);
    p.append_match_event(HOME, 0, 100.0);
    let second = p.append_match_event(AWAY, 0, 103.0);
    // Its own lead-in is ignored: it doesn't pull the start back.
    p.set_reel_trim(second, ReelEnd::Start, Some((0, 1.0)))
        .unwrap();

    let plan = reel(&p);
    assert_eq!(spans(&plan), [(0, 70.0, 109.0)]);
    assert_eq!(
        texts(&plan),
        ["1 / 1 | Home goal"],
        "numbering counts entries"
    );
}

/// A goal inside the previous entry whose own tail ends sooner leaves the
/// entry's end alone.
#[test]
fn a_merged_goal_never_shortens_the_entry() {
    let mut p = project(&[1000.0]);
    let first = p.append_match_event(HOME, 0, 100.0);
    p.set_reel_trim(first, ReelEnd::End, Some((0, 120.0)))
        .unwrap();
    p.append_match_event(AWAY, 0, 110.0);

    assert_eq!(spans(&reel(&p)), [(0, 70.0, 120.0)]);
}

#[test]
fn one_side_of_a_trim_overrides_only_that_side() {
    let mut p = project(&[1000.0]);
    let a = p.append_match_event(HOME, 0, 100.0);
    let b = p.append_match_event(HOME, 0, 500.0);
    p.set_reel_trim(a, ReelEnd::Start, Some((0, 95.0))).unwrap();
    p.set_reel_trim(b, ReelEnd::End, Some((0, 502.0))).unwrap();

    assert_eq!(spans(&reel(&p)), [(0, 95.0, 106.0), (0, 470.0, 502.0)]);
}

#[test]
fn goals_are_in_match_order_across_sources() {
    let mut p = project(&[1000.0, 1000.0]);
    // Stored out of match order: the second half's goal was tagged first.
    p.append_match_event(AWAY, 1, 100.0);
    p.append_match_event(HOME, 0, 900.0);
    p.append_match_event(HOME, 0, 200.0);

    let plan = reel(&p);
    assert_eq!(
        spans(&plan),
        [(0, 170.0, 206.0), (0, 870.0, 906.0), (1, 70.0, 106.0)]
    );
    assert_eq!(
        texts(&plan),
        [
            "1 / 3 | Home goal",
            "2 / 3 | Home goal",
            "3 / 3 | Away goal"
        ]
    );
}

#[test]
fn the_text_carries_the_score_after_the_goal() {
    let mut p = with_scoreboard(project(&[3000.0]));
    p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    p.append_match_event(HOME, 0, 100.0);
    p.append_match_event(AWAY, 0, 500.0);

    assert_eq!(
        texts(&reel(&p)),
        ["1 / 2 | Rovers goal | 1-0", "2 / 2 | United goal | 1-1"]
    );
}

/// No state: a scoreboard is set up but no kick-off is tagged yet.
#[test]
fn the_text_drops_the_score_when_there_is_none() {
    let mut p = with_scoreboard(project(&[1000.0]));
    p.append_match_event(AWAY, 0, 100.0);

    assert_eq!(texts(&reel(&p)), ["1 / 1 | United goal"]);
}

#[test]
fn the_text_says_home_or_away_with_no_scoreboard() {
    let mut p = project(&[1000.0]);
    p.append_match_event(HOME, 0, 100.0);
    p.append_match_event(AWAY, 0, 500.0);

    assert_eq!(texts(&reel(&p)), ["1 / 2 | Home goal", "2 / 2 | Away goal"]);
}

/// A goal tagged after the final whistle doesn't count, but it is the coach's
/// tagging slip to see, not something the reel hides.
#[test]
fn a_goal_the_scoreboard_does_not_count_still_has_an_entry() {
    let mut p = with_scoreboard(project(&[7000.0]));
    for t in [0.0, 2700.0, 3000.0, 5700.0] {
        p.append_match_event(MatchEventKind::StartStop, 0, t);
    }
    p.append_match_event(HOME, 0, 100.0);
    p.append_match_event(HOME, 0, 6000.0);

    assert_eq!(
        texts(&reel(&p)),
        ["1 / 2 | Rovers goal | 1-0", "2 / 2 | Rovers goal | 1-0"]
    );
}

#[test]
fn a_project_with_no_goals_has_an_empty_reel() {
    let mut p = with_scoreboard(project(&[1000.0]));
    assert!(reel(&p).entries.is_empty());
    p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
    assert!(reel(&p).entries.is_empty(), "a start/stop is not a goal");
}

/// Nothing reel-specific in the schedule: the source runs from the span's
/// start at 1x, at identity zoom.
#[test]
fn the_schedule_plays_each_span_at_identity_zoom() {
    let mut p = project(&[1000.0]);
    p.append_match_event(HOME, 0, 100.0);

    let c = compilation_schedule(&p, &ExportTarget::Reel);
    assert_eq!(c.frames.len(), c.plan.total_frames());
    assert_eq!(c.frames[0].source_time, 70.0);
    let last = c.frames.last().unwrap();
    assert!((last.source_time - (106.0 - 1.0 / 30.0)).abs() < 1e-9);
    assert!(c.frames.iter().all(|f| f.zoom == Zoom::IDENTITY));
}
