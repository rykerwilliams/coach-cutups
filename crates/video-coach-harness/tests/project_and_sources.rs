//! Bus end to end: project lifecycle (spec D6) and the source list (D7).
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project, `<tmp>/media` the fixture videos — so stored source paths climb
//! out of the project folder, as real ones usually do.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{Command, Event, StateFile, UserError};
use video_coach_core::project::{Clip, Project, SourceRef};
use video_coach_core::scoreboard_config::{MatchEventKind, MatchEventRecord};
use video_coach_core::store;
use video_coach_harness::Harness;
use video_coach_media::{fixtures, probe};

struct Dirs {
    tmp: TempDir,
}

impl Dirs {
    fn new() -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("project")).unwrap();
        std::fs::create_dir(tmp.path().join("media")).unwrap();
        Dirs { tmp }
    }

    fn config(&self) -> PathBuf {
        self.tmp.path().join("config")
    }

    fn project(&self) -> PathBuf {
        self.tmp.path().join("project")
    }

    /// A 2-second 30 fps WebM in `media/`.
    fn video(&self, name: &str, w: u32, h: u32) -> PathBuf {
        fixtures::webm(&self.tmp.path().join("media"), name, 2, w, h, 30, 15)
    }

    fn harness(&self) -> Harness {
        Harness::new(&self.config())
    }

    /// Writes a project whose sources are `videos` (16:9 fixtures in
    /// `media/`), with the given clip and match-event source indices.
    fn write_project(&self, videos: &[&str], clip_on: &[usize], event_on: &[usize]) -> Project {
        let mut project = Project::new("Game");
        for name in videos {
            let path = self.video(name, 320, 180);
            let p = probe(&path).unwrap();
            project.source_videos.push(SourceRef {
                relative_path: format!("../media/{name}"),
                display_name: (*name).into(),
                duration_seconds: p.duration_seconds,
                display_aspect: p.display_aspect,
            });
        }
        project.clips = clip_on.iter().map(|&i| clip(i)).collect();
        project.match_events = event_on.iter().map(|&i| match_event(i)).collect();
        store::write(&self.project(), &mut project).unwrap();
        project
    }
}

fn clip(source_index: usize) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: format!("clip on {source_index}"),
        notes: String::new(),
        tags: Vec::new(),
        source_index,
        start_source_seconds: 0.5,
        recording_duration: 1.0,
        recording_filename: format!("{}.mkv", Uuid::new_v4()),
        events: Vec::new(),
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

fn match_event(source_index: usize) -> MatchEventRecord {
    MatchEventRecord {
        id: Uuid::new_v4(),
        kind: MatchEventKind::HomeGoal,
        source_index,
        source_seconds: 1.0,
        is_auto_back_anchor: false,
    }
}

fn opened(h: &mut Harness) -> Arc<Project> {
    match h.wait_for("ProjectOpened", |e| matches!(e, Event::ProjectOpened(_))) {
        Event::ProjectOpened(p) => p,
        _ => unreachable!(),
    }
}

fn changed(h: &mut Harness) -> Arc<Project> {
    match h.wait_for("ProjectChanged", |e| matches!(e, Event::ProjectChanged(_))) {
        Event::ProjectChanged(p) => p,
        _ => unreachable!(),
    }
}

fn missing(h: &mut Harness) -> Vec<bool> {
    match h.wait_for("Missing", |e| matches!(e, Event::Missing(_))) {
        Event::Missing(m) => m,
        _ => unreachable!(),
    }
}

fn position(h: &mut Harness) -> (usize, Option<f64>) {
    match h.wait_for("Position", |e| matches!(e, Event::Position { .. })) {
        Event::Position {
            source_index,
            target_abs,
        } => (source_index, target_abs),
        _ => unreachable!(),
    }
}

fn playing(h: &mut Harness) -> bool {
    match h.wait_for("Playing", |e| matches!(e, Event::Playing(_))) {
        Event::Playing(p) => p,
        _ => unreachable!(),
    }
}

fn source_indices(p: &Project) -> (Vec<usize>, Vec<usize>) {
    (
        p.clips.iter().map(|c| c.source_index).collect(),
        p.match_events.iter().map(|m| m.source_index).collect(),
    )
}

fn names(p: &Project) -> Vec<&str> {
    p.source_videos
        .iter()
        .map(|s| s.display_name.as_str())
        .collect()
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[test]
fn opening_a_folder_without_a_project_creates_one() {
    let dirs = Dirs::new();
    let folder = dirs.tmp.path().join("Saturday Game");
    std::fs::create_dir(&folder).unwrap();
    let mut h = dirs.harness();

    h.send(Command::OpenProject(folder.clone()));
    let p = opened(&mut h);
    assert_eq!(p.name, "Saturday Game");
    assert!(p.source_videos.is_empty());
    assert_eq!(missing(&mut h), Vec::<bool>::new());

    assert_eq!(store::read(&folder).unwrap(), *p);
    assert_eq!(
        StateFile::in_config_dir(&dirs.config()).last_project(),
        Some(folder.canonicalize().unwrap())
    );
    h.shutdown();
}

#[test]
fn opening_an_unreadable_project_keeps_the_previous_project_and_folder() {
    let dirs = Dirs::new();
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);

    for (name, text, expected) in [
        (
            "corrupt",
            "{ this is not json",
            UserError::UnreadableProject(String::new()),
        ),
        (
            "legacy",
            r#"{"formatVersion": 6, "name": "Old"}"#,
            UserError::LegacyProject { found: 6 },
        ),
    ] {
        let bad = dirs.tmp.path().join(name);
        std::fs::create_dir(&bad).unwrap();
        std::fs::write(bad.join(store::PROJECT_FILENAME), text).unwrap();

        h.send(Command::OpenProject(bad.clone()));
        let err = h.wait_for_error();
        match (&err, &expected) {
            (UserError::UnreadableProject(_), UserError::UnreadableProject(_)) => {}
            _ => assert_eq!(err, expected, "{name}"),
        }

        // A mutation still lands on the previous project, in its own folder.
        h.send(Command::RenameProject(format!("after {name}")));
        assert_eq!(changed(&mut h).name, format!("after {name}"));
        assert_eq!(
            store::read(&dirs.project()).unwrap().name,
            format!("after {name}")
        );
        assert_eq!(
            read_bytes(&bad.join(store::PROJECT_FILENAME)),
            text.as_bytes(),
            "{name}: the refused file must not be touched"
        );
    }

    let rest = h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectOpened(_))),
        "{rest:#?}"
    );
    assert_eq!(
        StateFile::in_config_dir(&dirs.config()).last_project(),
        Some(dirs.project().canonicalize().unwrap())
    );
}

#[test]
fn restore_reopens_the_last_project() {
    let dirs = Dirs::new();
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);
    h.send(Command::RenameProject("Remembered".into()));
    changed(&mut h);
    h.shutdown();

    let mut h = dirs.harness();
    h.send(Command::RestoreLastProject);
    assert_eq!(opened(&mut h).name, "Remembered");
    h.shutdown();
}

#[test]
fn restoring_a_folder_that_no_longer_exists_does_not_create_it() {
    let dirs = Dirs::new();
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);
    h.shutdown();
    std::fs::remove_dir_all(dirs.project()).unwrap();

    let h = dirs.harness();
    h.send(Command::RestoreLastProject);
    let rest = h.shutdown();

    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectOpened(_))),
        "{rest:#?}"
    );
    assert!(!dirs.project().exists(), "restore recreated the folder");
    assert_eq!(
        StateFile::in_config_dir(&dirs.config()).last_project(),
        None,
        "a folder that can't be restored is forgotten"
    );
}

#[test]
fn a_source_with_a_different_aspect_is_rejected() {
    let dirs = Dirs::new();
    let wide = dirs.video("wide.webm", 320, 180);
    let square = dirs.video("square.webm", 320, 240);
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);

    h.send(Command::AddSource(wide));
    let p = changed(&mut h);
    assert_eq!(names(&p), ["wide.webm"]);
    assert_eq!(p.source_videos[0].relative_path, "../media/wide.webm");
    assert_eq!(missing(&mut h), [false]);

    h.send(Command::AddSource(square));
    assert!(
        matches!(h.wait_for_error(), UserError::AspectMismatch { .. }),
        "expected an aspect mismatch"
    );

    let rest = h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectChanged(_))),
        "{rest:#?}"
    );
    assert_eq!(names(&store::read(&dirs.project()).unwrap()), ["wide.webm"]);
}

#[test]
fn rotated_and_videoless_sources_are_rejected() {
    let dirs = Dirs::new();
    let media = dirs.tmp.path().join("media");
    let rotated = fixtures::rotated_mp4(&media);
    let audio = fixtures::audio_only(&media);
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);

    h.send(Command::AddSource(rotated));
    assert_eq!(h.wait_for_error(), UserError::Rotated("rotate-90".into()));
    h.send(Command::AddSource(audio));
    assert_eq!(h.wait_for_error(), UserError::NoVideo);

    let rest = h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectChanged(_))),
        "{rest:#?}"
    );
    assert!(store::read(&dirs.project())
        .unwrap()
        .source_videos
        .is_empty());
}

#[test]
fn remove_and_move_remap_clips_match_events_and_the_current_source() {
    let dirs = Dirs::new();
    // A clip on c, a match event on a.
    let written = dirs.write_project(&["a.webm", "b.webm", "c.webm", "d.webm"], &[2], &[0]);
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);
    assert_eq!(h.wait_settled(), 0);

    // Make d current.
    let d_start = written.cumulative_offset(3);
    h.send(Command::ScrubRelease { abs: d_start + 0.5 });
    assert_eq!(h.wait_settled(), 3);

    // Move d before b: [a, d, b, c]. Only offsets change: no reload.
    h.send(Command::MoveSource { from: 3, to: 1 });
    let p = changed(&mut h);
    assert_eq!(names(&p), ["a.webm", "d.webm", "b.webm", "c.webm"]);
    assert_eq!(source_indices(&p), (vec![3], vec![0]));
    assert_eq!(position(&mut h), (1, None));

    // Remove b, which nothing references: [a, d, c].
    h.send(Command::RemoveSource(2));
    let p = changed(&mut h);
    assert_eq!(names(&p), ["a.webm", "d.webm", "c.webm"]);
    assert_eq!(source_indices(&p), (vec![2], vec![0]));
    assert_eq!(position(&mut h), (1, None));

    // Remove d, the current source: c takes its index and loads from 0.
    h.send(Command::RemoveSource(1));
    let p = changed(&mut h);
    assert_eq!(names(&p), ["a.webm", "c.webm"]);
    assert_eq!(source_indices(&p), (vec![1], vec![0]));
    let (index, target) = position(&mut h);
    assert_eq!(index, 1);
    let target = target.expect("the replacement source is loaded");
    assert!(
        (target - p.cumulative_offset(1)).abs() < 1e-9,
        "loads c at its start, got {target}"
    );
    assert_eq!(h.wait_settled(), 1);

    h.shutdown();
    let on_disk = store::read(&dirs.project()).unwrap();
    assert_eq!(names(&on_disk), ["a.webm", "c.webm"]);
    assert_eq!(source_indices(&on_disk), (vec![1], vec![0]));
}

#[test]
fn a_referenced_source_cannot_be_removed() {
    let dirs = Dirs::new();
    dirs.write_project(&["a.webm", "b.webm", "c.webm"], &[2], &[1]);
    let before = read_bytes(&dirs.project().join(store::PROJECT_FILENAME));
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);

    h.send(Command::RemoveSource(2));
    assert_eq!(h.wait_for_error(), UserError::SourceReferenced { index: 2 });
    h.send(Command::RemoveSource(1));
    assert_eq!(h.wait_for_error(), UserError::SourceReferenced { index: 1 });

    let rest = h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectChanged(_))),
        "{rest:#?}"
    );
    assert_eq!(
        read_bytes(&dirs.project().join(store::PROJECT_FILENAME)),
        before
    );
}

#[test]
fn a_missing_source_blocks_play_until_relinked() {
    let dirs = Dirs::new();
    dirs.write_project(&["a.webm", "b.webm"], &[], &[]);
    std::fs::remove_file(dirs.tmp.path().join("media/b.webm")).unwrap();
    let mut h = dirs.harness();
    h.send(Command::OpenProject(dirs.project()));
    opened(&mut h);
    assert_eq!(missing(&mut h), [false, true]);
    assert_eq!(
        h.wait_settled(),
        0,
        "the present current source still loads"
    );

    h.send(Command::TogglePlay);
    assert!(!playing(&mut h), "play must be refused while b is missing");

    let found = dirs.video("b-found.webm", 320, 180);
    h.send(Command::RelinkSource(1, found));
    let p = changed(&mut h);
    assert_eq!(p.source_videos[1].relative_path, "../media/b-found.webm");
    assert_eq!(missing(&mut h), [false, false]);

    h.send(Command::TogglePlay);
    assert!(playing(&mut h));
    h.shutdown();
}
