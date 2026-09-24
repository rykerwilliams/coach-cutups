//! The whole-match export's plan (spec W): one whole entry per source video,
//! no captions, and chapters that are the match's own moments rather than its
//! entries.

use video_coach_core::export::OUTPUT_FPS;
use video_coach_core::plan::{compilation_plan, CompilationPlan, ExportTarget};
use video_coach_core::project::{Project, SourceRef};
use video_coach_core::reel::ReelSide;
use video_coach_core::scoreboard::{MatchEventKind, ScoreboardConfig, TeamConfig};
use video_coach_core::stroke::Rgba;
use video_coach_core::timeline::SegmentKind;

const HOME: MatchEventKind = MatchEventKind::HomeGoal;
const STOP: MatchEventKind = MatchEventKind::StartStop;

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

fn whole(p: &Project) -> CompilationPlan {
    compilation_plan(p, &ExportTarget::WholeMatch)
}

fn chapters(plan: &CompilationPlan) -> Vec<(f64, &str)> {
    plan.chapters
        .iter()
        .map(|(at, title)| (*at, title.as_str()))
        .collect()
}

/// Every source, in order, whole: no clip, no caption and one `Play` segment
/// covering the file.
#[test]
fn each_source_is_one_whole_entry_with_no_clip_and_no_caption() {
    let plan = whole(&project(&[100.0, 50.5]));
    assert_eq!(plan.entries.len(), 2);
    for (i, entry) in plan.entries.iter().enumerate() {
        assert_eq!(entry.clip_id, None);
        assert_eq!(entry.source_index, i);
        assert_eq!(entry.text, "", "a whole-match entry has no text bar");
        assert_eq!(entry.segments.len(), 1);
        assert_eq!(entry.segments[0].kind, SegmentKind::Play);
        assert_eq!(entry.segments[0].source_start, 0.0);
    }
    assert_eq!(plan.entries[0].segments[0].out_duration, 100.0);
    assert_eq!(plan.entries[1].segments[0].out_duration, 50.5);
    // Quantized per entry, as every plan is: 50.5 s is 1515 frames.
    assert_eq!(
        (plan.entries[0].start_frame, plan.entries[0].frames),
        (0, 3000)
    );
    assert_eq!(
        (plan.entries[1].start_frame, plan.entries[1].frames),
        (3000, 1515)
    );
    assert_eq!(plan.total_frames(), 4515);
}

/// The chapters are the tagged events at their output times, worded as a
/// film's chapters — not one per entry (spec W3).
#[test]
fn chapters_are_the_matchs_events_not_its_entries() {
    let mut p = with_scoreboard(project(&[100.0, 100.0]));
    p.append_match_event(STOP, 0, 10.0);
    p.append_match_event(HOME, 0, 30.0);
    p.append_match_event(STOP, 0, 90.0);
    p.append_match_event(STOP, 1, 5.0);

    let plan = whole(&p);
    // The second entry starts on frame 3000, so its events are 100 s on.
    assert_eq!(plan.entries[1].start_frame, 100 * OUTPUT_FPS as usize);
    assert_eq!(
        chapters(&plan),
        [
            (10.0, "Kick-off"),
            (30.0, "Rovers goal 1-0"),
            (90.0, "Half time"),
            (105.0, "Second half"),
        ]
    );
}

/// With nothing tagged there is still a chapter per half — but a single
/// source gets none, since one chapter would only repeat the file.
#[test]
fn a_match_with_no_events_gets_one_chapter_per_source() {
    let plan = whole(&project(&[100.0, 50.0]));
    assert_eq!(chapters(&plan), [(0.0, "half0"), (100.0, "half1")]);
    assert!(whole(&project(&[100.0])).chapters.is_empty());
}

/// Other targets keep their chapter per entry (spec C2): a reel's are its
/// goals' captions, whatever the match's events are.
#[test]
fn the_reel_keeps_a_chapter_per_entry() {
    let mut p = project(&[1000.0]);
    p.append_match_event(HOME, 0, 100.0);
    p.append_match_event(HOME, 0, 500.0);
    let reel = compilation_plan(&p, &ExportTarget::Reel(ReelSide::All));
    assert_eq!(
        chapters(&reel),
        [(0.0, "1 / 2 | Home goal"), (26.0, "2 / 2 | Home goal")]
    );
}
