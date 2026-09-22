//! Bus end to end: the goals reel and its trims (match vision spec R) — the
//! wiring only. What the reel's plan is and which trims are allowed are
//! core's rules and core's tests (`tests/reel.rs`, `tests/scoreboard.rs`).
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project (and, once a run starts, its `exports/`), `<tmp>/media` the
//! fixture game videos.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{export_targets, Command, Event, TargetState, UserError};
use video_coach_core::plan::{compilation_plan, ExportTarget};
use video_coach_core::project::{Project, Quality, Resolution};
use video_coach_core::scoreboard::{MatchEventKind, ReelEnd};
use video_coach_core::store::{self, EXPORTS_DIRNAME};
use video_coach_harness::{write_project, Harness};
use video_coach_media::fixtures;

/// A project called `Game` of fixture videos (name, seconds) and no clips,
/// opened on a fresh bus.
struct Proj {
    h: Harness,
    folder: PathBuf,
    _tmp: TempDir,
}

impl Proj {
    fn open(videos: &[(&str, u32)]) -> Self {
        Self::open_with(videos, |_| {})
    }

    /// [`Proj::open`], with `before_open` run on the media folder first.
    fn open_with(videos: &[(&str, u32)], before_open: impl FnOnce(&Path)) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&media).unwrap();
        write_project(&folder, &media, videos);
        before_open(&media);

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        Proj {
            h,
            folder,
            _tmp: tmp,
        }
    }

    /// Tags a goal at the caller's position and returns it with the project.
    fn goal(&mut self, source_index: usize, source_seconds: f64) -> (Uuid, Project) {
        self.h.send(Command::TagMatchEvent {
            kind: MatchEventKind::HomeGoal,
            source_index,
            source_seconds,
        });
        let project = self.h.wait_changed().project;
        let id = project.match_events.last().unwrap().id;
        (id, (*project).clone())
    }

    fn trim(&mut self, goal: Uuid, end: ReelEnd, at: Option<(usize, f64)>) {
        self.h.send(Command::SetReelTrim { goal, end, at });
    }

    fn saved(&self) -> Project {
        saved(&self.folder)
    }
}

/// The project as written, for after [`Harness::shutdown`] has taken the bus.
fn saved(folder: &Path) -> Project {
    store::read(folder).unwrap()
}

fn no_project_changed(rest: &[Event]) {
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectChanged(_))),
        "{rest:#?}"
    );
}

/// The reel of a project with goals and no clips renders through the one
/// export path: named "All goals", as long as its plan.
#[test]
fn the_reel_exports_through_the_bus() {
    let mut p = Proj::open(&[("a.webm", 3)]);
    // Trimmed apart, so the reel is two entries: [0, 1.5] and [2, 3].
    let (first, _) = p.goal(0, 1.0);
    let (second, _) = p.goal(0, 2.5);
    p.trim(first, ReelEnd::End, Some((0, 1.5)));
    p.h.wait_changed();
    p.trim(second, ReelEnd::Start, Some((0, 2.0)));
    let project = p.h.wait_changed().project;
    let plan = compilation_plan(&project, &ExportTarget::Reel);
    assert_eq!(plan.entries.len(), 2);

    p.h.send(Command::Export {
        targets: vec![ExportTarget::Reel],
        resolution: Resolution::R720,
        quality: Quality::Low,
    });
    let done = p.h.wait_map("the run's outcome", |e| match e {
        Event::Export(run) if !run.is_running() => Some(run.clone()),
        _ => None,
    });
    let target = &done.targets[0];
    assert_eq!(target.label, "All goals");
    assert_eq!(target.frames, plan.total_frames());
    let TargetState::Done(path) = &target.state else {
        panic!("{target:?}");
    };
    assert_eq!(
        path,
        &p.folder.join(EXPORTS_DIRNAME).join("All goals - Game.mp4")
    );
    assert_eq!(fixtures::decode_gray(path).len(), plan.total_frames());
    p.h.shutdown();
}

/// The sheet offers "All goals" once there is a goal, after the tag rows, and
/// not before.
#[test]
fn the_all_goals_row_follows_the_goals() {
    let mut p = Proj::open(&[("a.webm", 3)]);
    assert!(export_targets(&p.saved(), None).is_empty());

    let (_, project) = p.goal(0, 2.0);
    let rows = export_targets(&project, None);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].target, ExportTarget::Reel);
    assert_eq!(rows[0].label, "All goals");
    assert_eq!((rows[0].count, rows[0].unit), (1, "goal"));
    p.h.shutdown();
}

/// A trim is one undo step and a saved edit; a refused one is a notice and
/// changes nothing.
#[test]
fn a_trim_is_set_undone_and_refused_out_loud() {
    let mut p = Proj::open(&[("a.webm", 3)]);
    let (goal, _) = p.goal(0, 2.0);

    p.trim(goal, ReelEnd::Start, Some((0, 0.5)));
    let trimmed = p.h.wait_changed().project;
    assert_eq!(trimmed.match_events[0].reel_lead_in, Some(1.5));
    assert_eq!(p.saved().match_events, trimmed.match_events);

    p.h.send(Command::Undo);
    let undone = p.h.wait_changed().project;
    assert_eq!(undone.match_events[0].reel_lead_in, None);

    // A start after the goal.
    p.trim(goal, ReelEnd::Start, Some((0, 2.5)));
    let err = p.h.wait_for_error();
    assert!(
        matches!(&err, UserError::Scoreboard(msg) if msg.contains("before the goal")),
        "{err:?}"
    );
    assert!(err.is_notice());

    let rest = p.h.shutdown();
    no_project_changed(&rest);
    assert_eq!(saved(&p.folder).match_events, undone.match_events);
}

/// A trim is an `EditMatchEvents` snapshot, holding source indices, so a
/// source move purges it like a tag: an undo can't bring back a stale one.
#[test]
fn a_source_move_purges_the_trim_history() {
    let mut p = Proj::open(&[("a.webm", 2), ("b.webm", 2)]);
    let (goal, _) = p.goal(1, 1.5);
    p.trim(goal, ReelEnd::Start, Some((1, 1.0)));
    p.h.wait_changed();

    p.h.send(Command::MoveSource { from: 1, to: 0 });
    let moved = p.h.wait_changed().project.match_events.clone();
    assert_eq!(moved[0].source_index, 0);
    assert_eq!(moved[0].reel_lead_in, Some(0.5));

    p.h.send(Command::Undo);
    let rest = p.h.shutdown();
    no_project_changed(&rest);
    assert_eq!(saved(&p.folder).match_events, moved);
}

/// A reel entry has no clip to name, so the refusal names the game video's
/// file.
#[test]
fn a_missing_game_video_is_refused_naming_the_file() {
    let mut p = Proj::open_with(&[("a.webm", 2), ("b.webm", 2)], |media| {
        std::fs::remove_file(media.join("b.webm")).unwrap();
    });
    p.goal(0, 1.0);
    p.goal(1, 0.5);
    p.goal(1, 1.5);

    p.h.send(Command::Export {
        targets: vec![ExportTarget::Reel],
        resolution: Resolution::R720,
        quality: Quality::Low,
    });
    assert_eq!(
        p.h.wait_for_error(),
        UserError::CantExport("b.webm (a goal's game video) is missing; relink it first".into())
    );
    p.h.shutdown();
}
