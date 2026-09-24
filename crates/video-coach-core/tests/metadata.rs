//! What an exported file is called and what its header tags say: the wording
//! of a title, a description, the result and the teams, per export target and
//! for a project that has no scoreboard to draw them from.

use uuid::Uuid;
use video_coach_core::metadata::{file_tags, CalendarDate};
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Project, SourceRef};
use video_coach_core::recording::PendingClip;
use video_coach_core::reel::ReelSide;
use video_coach_core::scoreboard::{MatchEventKind, MatchFormat, ScoreboardConfig, TeamConfig};
use video_coach_core::stroke::Rgba;

fn team(name: &str) -> TeamConfig {
    TeamConfig::new(name, Rgba::RED, Rgba::RED)
}

/// A project of one 600-second game video, with `home` and `away` on the
/// scoreboard when they are given.
fn project(name: &str, teams: Option<(&str, &str)>) -> Project {
    let mut project = Project::new(name);
    project.source_videos.push(SourceRef {
        relative_path: "game.mp4".into(),
        display_name: "game.mp4".into(),
        duration_seconds: 600.0,
        display_aspect: 16.0 / 9.0,
    });
    project.scoreboard = teams.map(|(home, away)| ScoreboardConfig {
        home: team(home),
        away: team(away),
        format: MatchFormat::default(),
        auto_back_anchor_p1: false,
    });
    project
}

/// Kick-off at 0, then `home` home goals and `away` away goals, all inside
/// the first half of the one source.
fn play(project: &mut Project, home: u32, away: u32) {
    project.append_match_event(MatchEventKind::StartStop, 0, 0.0);
    for n in 0..home {
        project.append_match_event(MatchEventKind::HomeGoal, 0, 10.0 + f64::from(n));
    }
    for n in 0..away {
        project.append_match_event(MatchEventKind::AwayGoal, 0, 30.0 + f64::from(n));
    }
}

/// Adds a clip called `name` and returns its id.
fn add_clip(project: &mut Project, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    project.add_recorded_clip(
        PendingClip {
            id,
            source_index: 0,
            start_source_seconds: 0.0,
        },
        5.0,
        Vec::new(),
        String::new(),
    );
    project.clips.last_mut().expect("just added").name = name.into();
    id
}

#[test]
fn the_title_names_the_match_and_then_what_the_file_is() {
    let mut p = project("Saturday league", Some(("TCSC", "T3FC")));
    play(&mut p, 2, 1);
    let title = |target| file_tags(&p, &target, None).title;
    assert_eq!(title(ExportTarget::WholeMatch), "TCSC v T3FC — whole match");
    assert_eq!(title(ExportTarget::AllClips), "TCSC v T3FC — all clips");
    assert_eq!(
        title(ExportTarget::Reel(ReelSide::All)),
        "TCSC v T3FC — all goals"
    );
    assert_eq!(
        title(ExportTarget::Reel(ReelSide::Home)),
        "TCSC v T3FC — TCSC goals"
    );
    assert_eq!(
        title(ExportTarget::Reel(ReelSide::Away)),
        "TCSC v T3FC — T3FC goals"
    );
    assert_eq!(
        title(ExportTarget::Tag("transition".into())),
        "TCSC v T3FC — transition"
    );
}

#[test]
fn a_clip_target_is_titled_with_the_clips_name() {
    let mut p = project("Saturday league", Some(("TCSC", "T3FC")));
    let named_id = add_clip(&mut p, "High press");
    let unnamed_id = add_clip(&mut p, "   ");

    let title = |id| file_tags(&p, &ExportTarget::Clip(id), None).title;
    assert_eq!(title(named_id), "TCSC v T3FC — High press");
    assert_eq!(title(unnamed_id), "TCSC v T3FC — Untitled");
}

#[test]
fn the_description_names_the_project_and_what_the_export_is() {
    let p = project("Saturday league", Some(("TCSC", "T3FC")));
    assert_eq!(
        file_tags(&p, &ExportTarget::WholeMatch, None).description,
        "Whole match, from the Coach Cuts project “Saturday league”."
    );
    assert_eq!(
        file_tags(&p, &ExportTarget::Reel(ReelSide::Home), None).description,
        "TCSC goals, from the Coach Cuts project “Saturday league”."
    );
}

#[test]
fn the_comment_is_the_score_at_the_end_of_the_match() {
    let mut p = project("Saturday league", Some(("TCSC", "T3FC")));
    play(&mut p, 2, 1);
    let tags = file_tags(&p, &ExportTarget::WholeMatch, None);
    assert_eq!(tags.comment, "TCSC 2 - 1 T3FC");
    // Every target reports the same match, not the goals it happens to
    // contain: a tag compilation is still from this match.
    assert_eq!(
        file_tags(&p, &ExportTarget::Tag("transition".into()), None).comment,
        "TCSC 2 - 1 T3FC"
    );
    assert_eq!(tags.keywords, ["TCSC", "T3FC"]);
}

#[test]
fn a_goalless_match_still_reports_its_result() {
    let mut p = project("Saturday league", Some(("TCSC", "T3FC")));
    play(&mut p, 0, 0);
    assert_eq!(
        file_tags(&p, &ExportTarget::WholeMatch, None).comment,
        "TCSC 0 - 0 T3FC"
    );
}

#[test]
fn a_match_with_no_kick_off_tagged_reports_no_result() {
    let mut p = project("Saturday league", Some(("TCSC", "T3FC")));
    p.append_match_event(MatchEventKind::HomeGoal, 0, 10.0);
    let tags = file_tags(&p, &ExportTarget::WholeMatch, None);
    assert_eq!(tags.comment, "");
    // The teams are still named: they are configured, whatever is tagged.
    assert_eq!(tags.keywords, ["TCSC", "T3FC"]);
    assert_eq!(tags.title, "TCSC v T3FC — whole match");
}

#[test]
fn no_scoreboard_means_fewer_tags_not_invented_ones() {
    let p = project("Saturday league", None);
    let tags = file_tags(&p, &ExportTarget::WholeMatch, None);
    assert_eq!(tags.title, "Whole match");
    assert_eq!(
        tags.description,
        "Whole match, from the Coach Cuts project “Saturday league”."
    );
    assert_eq!(tags.comment, "");
    assert!(tags.keywords.is_empty());
    // A reel with no teams still falls back to the side's plain name.
    assert_eq!(
        file_tags(&p, &ExportTarget::Reel(ReelSide::Home), None).title,
        "Home goals"
    );
}

#[test]
fn awkward_team_and_project_names_are_trimmed_and_never_left_half_written() {
    let mut p = project("  ", Some(("  Rovers — B  ", "Ashford “A”")));
    play(&mut p, 1, 0);
    let tags = file_tags(&p, &ExportTarget::WholeMatch, None);
    assert_eq!(tags.title, "Rovers — B v Ashford “A” — whole match");
    assert_eq!(tags.description, "Whole match, from a Coach Cuts project.");
    assert_eq!(tags.comment, "Rovers — B 1 - 0 Ashford “A”");
    assert_eq!(tags.keywords, ["Rovers — B", "Ashford “A”"]);

    // A blank team name is no name at all: the title says what the file
    // is rather than " v Ashford", and there is one keyword, not two.
    let mut blank = project("Saturday league", Some(("   ", "Ashford")));
    play(&mut blank, 1, 0);
    let tags = file_tags(&blank, &ExportTarget::WholeMatch, None);
    assert_eq!(tags.title, "Whole match");
    assert_eq!(tags.comment, "");
    assert_eq!(tags.keywords, ["Ashford"]);

    // Both sides called the same thing (a derby typed twice) is one
    // keyword, not a repeat.
    let derby = project("Saturday league", Some(("Rovers", "Rovers")));
    assert_eq!(
        file_tags(&derby, &ExportTarget::WholeMatch, None).keywords,
        ["Rovers"]
    );
}

#[test]
fn the_encoder_tag_is_the_app_and_its_own_version() {
    let p = project("Saturday league", None);
    let tags = file_tags(&p, &ExportTarget::WholeMatch, None);
    assert_eq!(
        tags.encoder,
        format!("Coach Cuts {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(tags.encoder.starts_with("Coach Cuts "));
}

#[test]
fn the_date_is_whatever_the_caller_read_off_the_footage() {
    let p = project("Saturday league", None);
    let date = CalendarDate {
        year: 2026,
        month: 9,
        day: 21,
    };
    assert_eq!(
        file_tags(&p, &ExportTarget::WholeMatch, Some(date)).date,
        Some(date)
    );
    assert_eq!(file_tags(&p, &ExportTarget::WholeMatch, None).date, None);
}
