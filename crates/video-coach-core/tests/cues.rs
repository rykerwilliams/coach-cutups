//! The scoreboard as a cue list (spec U): one line per stretch of output, and
//! the `.srt` it is written as.

use video_coach_core::cues::{cues_to_srt, scoreboard_cues, Cue};
use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::compilation_schedule;
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Clip, Inset, Project, SourceRef};
use video_coach_core::scoreboard::{
    MatchEventKind, MatchFormat, ScoreboardConfig, ScoreboardContext, TeamConfig,
};
use video_coach_core::stroke::Rgba;

const STOP: MatchEventKind = MatchEventKind::StartStop;
const HOME: MatchEventKind = MatchEventKind::HomeGoal;

/// A project over sources of these lengths, with a scoreboard whose periods
/// are `period_seconds` long — short periods keep a cue list readable.
fn project(durations: &[f64], period_seconds: u32) -> Project {
    let mut p = Project::new("p");
    for (i, &duration_seconds) in durations.iter().enumerate() {
        p.source_videos.push(SourceRef {
            relative_path: format!("half{i}.mp4"),
            display_name: format!("half{i}"),
            duration_seconds,
            display_aspect: 16.0 / 9.0,
        });
    }
    p.scoreboard = Some(ScoreboardConfig {
        home: TeamConfig::new("Rovers", Rgba::RED, Rgba::RED),
        away: TeamConfig::new("United", Rgba::RED, Rgba::RED),
        format: MatchFormat {
            regulation_periods: 2,
            regulation_period_seconds: period_seconds,
            overtime_periods: 0,
            overtime_period_seconds: period_seconds,
        },
        auto_back_anchor_p1: false,
    });
    p
}

fn cues(p: &Project, target: &ExportTarget) -> Vec<Cue> {
    let context = ScoreboardContext::for_project(p).expect("a scoreboard is configured");
    scoreboard_cues(&compilation_schedule(p, target), &context)
}

fn table(cues: &[Cue]) -> Vec<(f64, f64, &str)> {
    cues.iter()
        .map(|c| (c.start, c.end, c.text.as_str()))
        .collect()
}

/// The text of the cue covering output second `t`, or `""` in a gap.
fn text_at(cues: &[Cue], t: f64) -> &str {
    cues.iter()
        .find(|c| c.start <= t && t < c.end)
        .map_or("", |c| c.text.as_str())
}

/// The whole cue list of a two-source match, as a table: nothing before the
/// kick-off, the score turning over on the goal's own frame, one `HT` cue
/// across the break — and across the join between the two files — `FT` after
/// the last period, and every cue ending exactly where the next begins.
#[test]
fn the_cue_list_is_the_scoreboard_run_length_encoded() {
    let mut p = project(&[3.0, 3.0], 2);
    p.append_match_event(STOP, 0, 0.5); // kick-off
    p.append_match_event(HOME, 0, 1.0); // a goal
    p.append_match_event(STOP, 0, 2.5); // half time
    p.append_match_event(STOP, 1, 0.5); // second half
    p.append_match_event(STOP, 1, 2.5); // full time

    let cues = cues(&p, &ExportTarget::WholeMatch);
    assert_eq!(
        table(&cues),
        [
            (0.5, 1.0, "Rovers 0 - 0 United · 00:00"),
            (1.0, 1.5, "Rovers 1 - 0 United · 00:00"),
            (1.5, 2.5, "Rovers 1 - 0 United · 00:01"),
            (2.5, 3.5, "Rovers 1 - 0 United · HT"),
            (3.5, 4.5, "Rovers 1 - 0 United · 00:02"),
            (4.5, 5.5, "Rovers 1 - 0 United · 00:03"),
            (5.5, 6.0, "Rovers 1 - 0 United · FT"),
        ]
    );
}

/// A clip that pauses holds the clock, so the pause is **one** cue rather than
/// a running one — the invariant BACKLOG #27 is about, read through the cue
/// list this time.
#[test]
fn a_frozen_entry_holds_the_clock_in_one_cue() {
    let mut p = project(&[1000.0], 2700);
    // Kick-off 10 s into the source, so the clock reads 90 s at source 100.
    p.append_match_event(STOP, 0, 10.0);
    p.clips.push(Clip {
        id: uuid::Uuid::nil(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: 100.0,
        recording_duration: 30.0,
        recording_filename: "c.mkv".into(),
        events: vec![
            CommentaryEvent::new(2.0, EventKind::Pause { source_time: 102.0 }),
            CommentaryEvent::new(22.0, EventKind::Play { source_time: 102.0 }),
        ],
        show_pip: true,
        inset: Inset::Camera,
        sort_index: 0,
        created_at: "2026-09-20T00:00:00Z".into(),
        transcript: String::new(),
    });

    let cues = cues(&p, &ExportTarget::AllClips);
    // 20 s of commentary over a held frame, plus the second either side of it
    // that reads the same clock: one cue, not twenty-one.
    let held = cues
        .iter()
        .find(|c| c.text.ends_with("01:32"))
        .expect("the clock reads 01:32 over the pause");
    assert_eq!((held.start, held.end), (2.0, 23.0));
    assert_eq!(text_at(&cues, 1.5), "Rovers 0 - 0 United · 01:31");
    assert_eq!(text_at(&cues, 23.5), "Rovers 0 - 0 United · 01:33");
}

/// Past the period's length the clock stops and the time past it is appended.
#[test]
fn stoppage_appends_the_time_past_the_period() {
    let mut p = project(&[100.0], 60);
    p.append_match_event(STOP, 0, 10.0);

    let cues = cues(&p, &ExportTarget::WholeMatch);
    assert_eq!(text_at(&cues, 69.5), "Rovers 0 - 0 United · 00:59");
    assert_eq!(text_at(&cues, 70.5), "Rovers 0 - 0 United · 01:00 +0:00");
    assert_eq!(text_at(&cues, 71.5), "Rovers 0 - 0 United · 01:00 +0:01");
}

/// Nothing tagged is no match yet, which is a gap, not a cue reading 0-0.
#[test]
fn a_match_with_nothing_tagged_has_no_cues() {
    let p = project(&[3.0], 2);
    assert!(cues(&p, &ExportTarget::WholeMatch).is_empty());
}

#[test]
fn cues_to_srt_numbers_from_one_and_writes_hours_and_milliseconds() {
    let cue = |start: f64, end: f64, text: &str| Cue {
        start,
        end,
        text: text.into(),
    };
    let srt = cues_to_srt(&[
        cue(0.0, 1.5, "Rovers 0 - 0 United · 00:00"),
        cue(3661.5, 3662.25, "Rovers 1 - 0 United · HT"),
    ]);
    assert_eq!(
        srt,
        "1\n00:00:00,000 --> 00:00:01,500\nRovers 0 - 0 United · 00:00\n\n\
         2\n01:01:01,500 --> 01:01:02,250\nRovers 1 - 0 United · HT\n\n"
    );
}

#[test]
fn no_cues_is_an_empty_srt() {
    assert_eq!(cues_to_srt(&[]), "");
}
