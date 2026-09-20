//! Bus end to end: match events and the scoreboard's setup (Phase 9 spec S5)
//! — tagging, deleting, the undo step each is, the refusals, and the purge a
//! source move makes of the history.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project, `<tmp>/media` the fixture game videos.

use std::path::PathBuf;

use tempfile::TempDir;
use video_coach_app::bus::{Command, Event, UserError};
use video_coach_core::project::Project;
use video_coach_core::scoreboard::{
    MatchEventKind, MatchEventRecord, MatchFormat, ScoreboardConfig, TeamConfig,
};
use video_coach_core::store;
use video_coach_core::stroke::Rgba;
use video_coach_harness::{write_project, Harness};

/// A project of 2-second fixture videos, as written.
struct Proj {
    folder: PathBuf,
    _tmp: TempDir,
}

impl Proj {
    /// Writes the project and opens it on a fresh bus.
    fn open(videos: &[&str]) -> (Harness, Self) {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&media).unwrap();
        let videos: Vec<(&str, u32)> = videos.iter().map(|&name| (name, 2)).collect();
        write_project(&folder, &media, &videos);

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        (h, Proj { folder, _tmp: tmp })
    }

    fn saved(&self) -> Project {
        store::read(&self.folder).unwrap()
    }
}

/// A tag as the UI sends it: the readout's position at the keypress.
fn tag(kind: MatchEventKind, source_index: usize, source_seconds: f64) -> Command {
    Command::TagMatchEvent {
        kind,
        source_index,
        source_seconds,
    }
}

/// Two named teams and soccer's format: four start/stops, no back-anchor.
fn scoreboard() -> ScoreboardConfig {
    let white = Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    ScoreboardConfig {
        home: TeamConfig::new("Rovers", Rgba::RED, white),
        away: TeamConfig::new("United", white, Rgba::RED),
        format: MatchFormat::default(),
        auto_back_anchor_p1: false,
    }
}

fn events(p: &Project) -> Vec<MatchEventRecord> {
    p.match_events.clone()
}

fn no_project_changed(rest: &[Event]) {
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectChanged(_))),
        "{rest:#?}"
    );
}

/// Each tag and each delete saves and is one undo step, taking the whole list
/// back with it.
#[test]
fn tagging_and_deleting_are_saved_and_undone_a_step_at_a_time() {
    let (mut h, p) = Proj::open(&["a.webm", "b.webm"]);

    h.send(tag(MatchEventKind::StartStop, 0, 0.5));
    let one = events(&h.wait_changed().project);
    h.send(tag(MatchEventKind::HomeGoal, 1, 1.0));
    let two = events(&h.wait_changed().project);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].source_seconds, 0.5);
    assert_eq!(two[1].kind, MatchEventKind::HomeGoal);
    assert_eq!(two[1].source_index, 1);
    assert_eq!(events(&p.saved()), two);

    h.send(Command::DeleteMatchEvent(two[0].id));
    assert_eq!(events(&h.wait_changed().project), two[1..]);

    // Back a step at a time: the delete, then the goal.
    h.send(Command::Undo);
    assert_eq!(events(&h.wait_changed().project), two);
    h.send(Command::Undo);
    assert_eq!(events(&h.wait_changed().project), one);
    h.send(Command::Redo);
    assert_eq!(events(&h.wait_changed().project), two);

    h.shutdown();
    assert_eq!(events(&p.saved()), two);
}

/// The cap is the format's start/stops, and it counts **records**: goals are
/// never capped. The mutator stores whatever it is given, so this is the only
/// place that refuses — out loud, where macOS's silently did nothing.
#[test]
fn a_start_stop_past_the_format_is_refused_and_goals_are_not() {
    let (mut h, p) = Proj::open(&["a.webm"]);
    h.send(Command::SetScoreboard(scoreboard()));
    h.wait_changed();

    for i in 0..4 {
        h.send(tag(MatchEventKind::StartStop, 0, f64::from(i) * 0.1));
        h.wait_changed();
    }
    h.send(tag(MatchEventKind::StartStop, 0, 1.0));
    let err = h.wait_for_error();
    assert!(
        matches!(&err, UserError::Scoreboard(msg) if msg.contains("format")),
        "{err:?}"
    );

    h.send(tag(MatchEventKind::HomeGoal, 0, 1.5));
    assert_eq!(h.wait_changed().project.match_events.len(), 5);

    h.shutdown();
    let saved = p.saved();
    assert_eq!(
        saved
            .match_events
            .iter()
            .filter(|m| m.kind == MatchEventKind::StartStop)
            .count(),
        4,
        "the refused start/stop was stored anyway"
    );
}

/// An empty team name is refused at the command, so the render path never has
/// to guard one (spec S5).
#[test]
fn a_team_without_a_name_is_refused_and_the_setup_stands() {
    let (mut h, p) = Proj::open(&["a.webm"]);
    h.send(Command::SetScoreboard(scoreboard()));
    h.wait_changed();

    let nameless = ScoreboardConfig {
        away: TeamConfig::new("  ", Rgba::RED, Rgba::RED),
        ..scoreboard()
    };
    h.send(Command::SetScoreboard(nameless));
    assert!(matches!(h.wait_for_error(), UserError::Scoreboard(_)));

    let rest = h.shutdown();
    no_project_changed(&rest);
    assert_eq!(p.saved().scoreboard, Some(scoreboard()));
}

/// A source move remaps the stored records but not the snapshots on the undo
/// and redo stacks, so both are purged: undo and redo can't restore a stale
/// `source_index`.
#[test]
fn a_source_move_purges_the_match_event_history() {
    let (mut h, p) = Proj::open(&["a.webm", "b.webm"]);
    // Two tags on b, the second undone so it sits on the redo stack.
    h.send(tag(MatchEventKind::HomeGoal, 1, 1.0));
    h.wait_changed();
    h.send(tag(MatchEventKind::AwayGoal, 1, 1.5));
    h.wait_changed();
    h.send(Command::Undo);
    assert_eq!(h.wait_changed().project.match_events.len(), 1);

    // b to the front: the live record is remapped to source 0.
    h.send(Command::MoveSource { from: 1, to: 0 });
    let moved = events(&h.wait_changed().project);
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].source_index, 0);

    h.send(Command::Undo);
    h.send(Command::Redo);
    let rest = h.shutdown();
    no_project_changed(&rest);
    assert_eq!(events(&p.saved()), moved);
}
