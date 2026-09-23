//! Bus end to end: the goals reel and its trims (match vision spec R), and
//! the whole-match export (spec W) — the wiring only. What either plan is,
//! and which trims are allowed, are core's rules and core's tests
//! (`tests/reel.rs`, `tests/whole_match.rs`, `tests/scoreboard.rs`).
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
use video_coach_core::reel::ReelSide;
use video_coach_core::scoreboard::{MatchEventKind, ReelEnd, ScoreboardConfig, TeamConfig};
use video_coach_core::store::{self, EXPORTS_DIRNAME};
use video_coach_core::stroke::Rgba;
use video_coach_harness::{write_project, Harness};
use video_coach_media::fixtures::{self, ffprobe};

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

    /// Tags a home goal at the caller's position and returns it with the
    /// project.
    fn goal(&mut self, source_index: usize, source_seconds: f64) -> (Uuid, Project) {
        self.goal_of(MatchEventKind::HomeGoal, source_index, source_seconds)
    }

    fn goal_of(
        &mut self,
        kind: MatchEventKind,
        source_index: usize,
        source_seconds: f64,
    ) -> (Uuid, Project) {
        self.h.send(Command::TagMatchEvent {
            kind,
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
    let plan = compilation_plan(&project, &ExportTarget::Reel(ReelSide::All));
    assert_eq!(plan.entries.len(), 2);

    p.h.send(Command::Export {
        targets: vec![ExportTarget::Reel(ReelSide::All)],
        resolution: Resolution::R720,
        quality: Quality::Low,
        scoreboard: None,
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

/// The sheet leads with "Whole match", which any source video earns (spec
/// W1), and offers the scoring side's reel once there is a goal, after the
/// tag rows. With no scoreboard set up that side is "Home".
#[test]
fn the_sheet_leads_with_the_whole_match_and_follows_the_goals() {
    let mut p = Proj::open(&[("a.webm", 3)]);
    let rows = export_targets(&p.saved(), None);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].target, ExportTarget::WholeMatch);
    assert_eq!(rows[0].label, "Whole match");
    assert_eq!((rows[0].count, rows[0].unit), (1, "video"));

    let (_, project) = p.goal(0, 2.0);
    let rows = export_targets(&project, None);
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert_eq!(rows[0].target, ExportTarget::WholeMatch);
    assert_eq!(rows[1].target, ExportTarget::Reel(ReelSide::Home));
    assert_eq!(rows[1].label, "Home goals");
    assert_eq!((rows[1].count, rows[1].unit), (1, "goal"));
    p.h.shutdown();
}

/// A reel row per side that has scored, named from the scoreboard, and "All
/// goals" only once both have — with one side scoring it would be the same
/// film twice (spec R1b).
#[test]
fn the_sheet_has_a_reel_row_per_side_that_scored() {
    let mut p = Proj::open(&[("a.webm", 3)]);
    p.h.send(Command::SetScoreboard(ScoreboardConfig {
        home: TeamConfig::new("Rovers", Rgba::RED, Rgba::RED),
        away: TeamConfig::new("United", Rgba::RED, Rgba::RED),
        format: Default::default(),
        auto_back_anchor_p1: false,
    }));
    p.h.wait_changed();

    let (_, one_sided) = p.goal_of(MatchEventKind::AwayGoal, 0, 1.0);
    assert_eq!(
        reels(&one_sided),
        [(ExportTarget::Reel(ReelSide::Away), "United goals".into())]
    );

    let (_, both) = p.goal_of(MatchEventKind::HomeGoal, 0, 2.5);
    assert_eq!(
        reels(&both),
        [
            (ExportTarget::Reel(ReelSide::All), "All goals".into()),
            (ExportTarget::Reel(ReelSide::Home), "Rovers goals".into()),
            (ExportTarget::Reel(ReelSide::Away), "United goals".into()),
        ]
    );
    // And they are the tail of the sheet, after the whole match's row.
    let rows = export_targets(&both, None);
    assert_eq!(rows[0].target, ExportTarget::WholeMatch);
    assert_eq!(rows.len(), 4, "{rows:#?}");
    p.h.shutdown();
}

/// The sheet's reel rows, as `(target, label)`.
fn reels(project: &Project) -> Vec<(ExportTarget, String)> {
    export_targets(project, None)
        .into_iter()
        .filter(|r| matches!(r.target, ExportTarget::Reel(_)))
        .map(|r| (r.target, r.label))
        .collect()
}

/// The whole match renders through the same export path, named after itself,
/// with the match's own moments as its chapters (spec W3) — read back with
/// the only independent reader of `chpl`.
#[test]
fn the_whole_match_exports_with_the_matchs_own_chapters() {
    let mut p = Proj::open(&[("a.webm", 3), ("b.webm", 3)]);
    p.h.send(Command::SetScoreboard(ScoreboardConfig {
        home: TeamConfig::new("Rovers", Rgba::RED, Rgba::RED),
        away: TeamConfig::new("United", Rgba::RED, Rgba::RED),
        format: Default::default(),
        auto_back_anchor_p1: false,
    }));
    p.h.wait_changed();
    let tags = [
        (MatchEventKind::StartStop, 0, 0.5),
        (MatchEventKind::HomeGoal, 0, 1.5),
        (MatchEventKind::StartStop, 0, 2.5),
        (MatchEventKind::StartStop, 1, 0.5),
    ];
    for &(kind, source_index, source_seconds) in &tags {
        p.h.send(Command::TagMatchEvent {
            kind,
            source_index,
            source_seconds,
        });
    }
    let project = tags
        .iter()
        .map(|_| p.h.wait_changed())
        .last()
        .unwrap()
        .project;

    let plan = compilation_plan(&project, &ExportTarget::WholeMatch);
    assert_eq!(plan.entries.len(), 2);
    assert!(plan
        .entries
        .iter()
        .all(|e| e.clip_id.is_none() && e.text.is_empty()));
    let titles: Vec<&str> = plan.chapters.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(
        titles,
        ["Kick-off", "Rovers goal 1-0", "Half time", "Second half"]
    );

    p.h.send(Command::Export {
        targets: vec![ExportTarget::WholeMatch],
        resolution: Resolution::R720,
        quality: Quality::Low,
        // **Default, on WebM sources.** The copy Default reaches for joins
        // H.264 in MP4 alone (spec E2), so this is also where the fallback is
        // pinned: the board is burned in and the match re-encoded, as it was
        // before the copy existed, rather than the export failing at a gate
        // the coach never asked for. The copy's own chapters are
        // `media/tests/copy.rs`.
        scoreboard: None,
    });
    let done = p.h.wait_map("the run's outcome", |e| match e {
        Event::Export(run) if !run.is_running() => Some(run.clone()),
        _ => None,
    });
    let target = &done.targets[0];
    assert_eq!(target.label, "Whole match");
    assert_eq!(target.frames, plan.total_frames());
    let TargetState::Done(path) = &target.state else {
        panic!("{target:?}");
    };
    assert_eq!(
        path,
        &p.folder
            .join(EXPORTS_DIRNAME)
            .join("Whole match - Game.mp4")
    );
    assert_eq!(fixtures::decode_gray(path).len(), plan.total_frames());

    let got = ffprobe_chapters(path);
    assert_eq!(
        got.len(),
        plan.chapters.len(),
        "chapters read back: {got:?}"
    );
    for ((at, title), (want_at, want_title)) in got.iter().zip(&plan.chapters) {
        assert_eq!(title, want_title);
        assert!(
            (at - want_at).abs() < 0.001,
            "{title:?} starts at {at}, not {want_at}"
        );
    }
    p.h.shutdown();
}

/// `path`'s chapters as `ffprobe` reads them: `(start in seconds, title)`.
///
/// The independent reader, because GStreamer's `qtdemux` doesn't read `chpl`
/// and no released Rust MP4 crate parses it.
fn ffprobe_chapters(path: &Path) -> Vec<(f64, String)> {
    ffprobe(path, &["-show_chapters"])["chapters"]
        .as_array()
        .expect("a chapters array")
        .iter()
        .map(|c| {
            (
                c["start_time"].as_str().unwrap().parse().unwrap(),
                c["tags"]["title"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
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
/// file — the same refusal a whole-match entry gets.
#[test]
fn a_missing_game_video_is_refused_naming_the_file() {
    let mut p = Proj::open_with(&[("a.webm", 2), ("b.webm", 2)], |media| {
        std::fs::remove_file(media.join("b.webm")).unwrap();
    });
    p.goal(0, 1.0);
    p.goal(1, 0.5);
    p.goal(1, 1.5);

    p.h.send(Command::Export {
        targets: vec![ExportTarget::Reel(ReelSide::All)],
        resolution: Resolution::R720,
        quality: Quality::Low,
        scoreboard: None,
    });
    assert_eq!(
        p.h.wait_for_error(),
        UserError::CantExport("b.webm (the game video) is missing; relink it first".into())
    );
    p.h.shutdown();
}
