//! Bus end to end: clip management (Phase 3 spec C1–C5) — edits, delete to
//! the trash, reorder and sort, jump, undo and redo.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project with a stand-in recording per clip in `recordings/`, `<tmp>/media`
//! the fixture game videos.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{Command, Event, RecordingStatus, UserError};
use video_coach_core::project::{Clip, Project};
use video_coach_core::store;
use video_coach_core::undo::ClipEdit;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{add_clips, write_project, Harness, ReadOnly};

const FRAME: f64 = 1.0 / 30.0;

/// A project of 2-second fixture videos with clips on the given sources, as
/// written.
struct Proj {
    clips: Vec<Clip>,
    folder: PathBuf,
    _tmp: TempDir,
}

impl Proj {
    /// Writes the project and opens it on a fresh bus.
    fn open(videos: &[&str], clips_on: &[usize]) -> (Harness, Self) {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&media).unwrap();
        let videos: Vec<(&str, u32)> = videos.iter().map(|&name| (name, 2)).collect();
        let mut project = write_project(&folder, &media, &videos);
        let clips = add_clips(&folder, &mut project, clips_on);

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        (
            h,
            Proj {
                clips,
                folder,
                _tmp: tmp,
            },
        )
    }

    fn id(&self, i: usize) -> Uuid {
        self.clips[i].id
    }

    fn saved(&self) -> Project {
        store::read(&self.folder).unwrap()
    }

    fn recordings(&self) -> PathBuf {
        self.folder.join(store::RECORDINGS_DIRNAME)
    }

    fn trash(&self) -> PathBuf {
        self.recordings().join(".trash")
    }

    /// Whether clip `i`'s recording is in `.trash`.
    fn trashed(&self, i: usize) -> bool {
        self.trash()
            .join(&self.clips[i].recording_filename)
            .exists()
    }

    /// Whether clip `i`'s recording is in `recordings/` and not in `.trash`,
    /// or the other way round.
    fn in_recordings(&self, i: usize) -> bool {
        let name = &self.clips[i].recording_filename;
        let live = self.recordings().join(name).exists();
        let trashed = self.trash().join(name).exists();
        assert_ne!(live, trashed, "clip {i}: exactly one copy of its file");
        live
    }
}

/// Waits until no seek is outstanding, the bus's current source is `index`,
/// and the pipeline is within a frame of `secs` in it.
fn settle_at(h: &mut Harness, index: usize, secs: f64) {
    h.poll_until(&format!("settled at {secs} in source {index}"), |h| {
        latest_position(h) == Some((index, None))
            && h.position_secs().is_some_and(|p| (p - secs).abs() < FRAME)
    });
}

fn edit(h: &Harness, id: Uuid, edit: ClipEdit) {
    h.send(Command::EditClip { id, edit });
}

fn ids(p: &Project) -> Vec<Uuid> {
    p.clips.iter().map(|c| c.id).collect()
}

/// The latest `Position` the bus published: source index and target.
fn latest_position(h: &Harness) -> Option<(usize, Option<f64>)> {
    h.log().iter().rev().find_map(|e| match e {
        Event::Position {
            source_index,
            target_abs,
        } => Some((*source_index, *target_abs)),
        _ => None,
    })
}

fn no_project_changed(rest: &[Event]) {
    assert!(
        !rest.iter().any(|e| matches!(e, Event::ProjectChanged(_))),
        "{rest:#?}"
    );
}

/// The files in `dir`; none if it doesn't exist.
fn files(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn an_edit_saves_and_is_one_undo_step() {
    let (mut h, p) = Proj::open(&["a.webm"], &[0]);
    let id = p.id(0);
    let original = p.clips[0].name.clone();

    edit(&h, p.id(0), ClipEdit::Name("Corner".into()));
    assert_eq!(h.wait_changed().project.clips[0].name, "Corner");
    // Tags arrive as the field's raw text and are normalized.
    edit(
        &h,
        p.id(0),
        ClipEdit::Tags(vec![" Press, set piece ,PRESS".into()]),
    );
    assert_eq!(
        h.wait_changed().project.clips[0].tags,
        ["press", "set piece"]
    );
    assert_eq!(p.saved().clips[0].tags, ["press", "set piece"]);

    h.send(Command::Undo);
    assert!(h.wait_changed().project.clips[0].tags.is_empty());
    assert_eq!(h.wait_select(), id);
    h.send(Command::Undo);
    assert_eq!(h.wait_changed().project.clips[0].name, original);
    assert_eq!(h.wait_select(), id);
    h.send(Command::Redo);
    assert_eq!(h.wait_changed().project.clips[0].name, "Corner");
    assert_eq!(h.wait_select(), id);

    // Unchanged after normalization: no save, and no undo step, so the
    // redo stack survives and the next change is the redone tags.
    edit(&h, p.id(0), ClipEdit::Name("Corner".into()));
    edit(&h, p.id(0), ClipEdit::Tags(vec![String::new()]));
    h.send(Command::Redo);
    assert_eq!(
        h.wait_changed().project.clips[0].tags,
        ["press", "set piece"]
    );
    assert_eq!(h.wait_select(), id);

    h.shutdown();
    let saved = p.saved();
    assert_eq!(saved.clips[0].name, "Corner");
    assert_eq!(saved.clips[0].tags, ["press", "set piece"]);
}

#[test]
fn deletes_go_to_the_trash_and_each_undo_restores_one_in_place() {
    let (mut h, p) = Proj::open(&["a.webm"], &[0, 0, 0]);
    let all = ids(&p.saved());

    h.send(Command::DeleteClip(p.id(1)));
    h.wait_changed();
    h.send(Command::DeleteClip(p.id(0)));
    assert_eq!(ids(&h.wait_changed().project), [p.id(2)]);
    assert_eq!(ids(&p.saved()), [p.id(2)]);
    assert!(!p.in_recordings(0) && !p.in_recordings(1));

    h.send(Command::Undo);
    assert_eq!(ids(&h.wait_changed().project), [p.id(0), p.id(2)]);
    assert_eq!(h.wait_select(), p.id(0));
    h.send(Command::Undo);
    assert_eq!(ids(&h.wait_changed().project), all);
    assert_eq!(h.wait_select(), p.id(1));
    assert_eq!(ids(&p.saved()), all);
    assert!(p.in_recordings(0) && p.in_recordings(1));

    h.send(Command::Redo);
    assert_eq!(ids(&h.wait_changed().project), [p.id(0), p.id(2)]);
    h.shutdown();
    assert!(!p.in_recordings(1) && p.in_recordings(0));
    assert_eq!(ids(&p.saved()), [p.id(0), p.id(2)]);
}

#[test]
fn a_delete_whose_save_fails_leaves_the_recording_in_place() {
    let (mut h, p) = Proj::open(&["a.webm"], &[0]);
    let read_only = ReadOnly::new(&p.folder);
    if !read_only.enforced() {
        eprintln!("skipped: permissions aren't enforced (running as root?)");
        return;
    }

    h.send(Command::DeleteClip(p.id(0)));
    assert!(matches!(h.wait_for_error(), UserError::Io(_)));
    // The in-memory change stands, as for any failed save.
    assert!(h.wait_changed().project.clips.is_empty());
    assert_eq!(ids(&p.saved()), [p.id(0)]);
    assert!(
        p.in_recordings(0),
        "the file moved although the save failed"
    );

    // Undo still brings it back, and saves once it can.
    drop(read_only);
    h.send(Command::Undo);
    assert_eq!(ids(&h.wait_changed().project), [p.id(0)]);
    h.shutdown();
    assert_eq!(ids(&p.saved()), [p.id(0)]);
    assert!(p.in_recordings(0));
}

#[test]
fn a_source_change_shreds_trashed_clips_and_their_undo() {
    // Clips on a and b; c is referenced by nothing.
    let (mut h, p) = Proj::open(&["a.webm", "b.webm", "c.webm"], &[0, 1]);

    // A removal: b is unreferenced once its clip is deleted.
    h.send(Command::DeleteClip(p.id(1)));
    h.wait_changed();
    assert!(p.trashed(1));
    h.send(Command::RemoveSource(1));
    assert_eq!(h.wait_changed().project.source_videos.len(), 2);
    assert!(!p.trashed(1), "the removal didn't shred");
    // A move.
    h.send(Command::DeleteClip(p.id(0)));
    h.wait_changed();
    assert!(p.trashed(0));
    h.send(Command::MoveSource { from: 0, to: 1 });
    h.wait_changed();
    assert!(!p.trashed(0), "the move didn't shred");

    // Nothing left to undo.
    h.send(Command::Undo);
    h.send(Command::Undo);
    let rest = h.shutdown();
    no_project_changed(&rest);
    assert!(files(&p.trash()).is_empty(), "{:?}", files(&p.trash()));
    for c in &p.clips {
        assert!(!p.recordings().join(&c.recording_filename).exists());
    }
    assert!(p.saved().clips.is_empty());
}

#[test]
fn a_redone_delete_restores_the_remapped_clip() {
    let (mut h, p) = Proj::open(&["a.webm", "b.webm"], &[1]);
    h.send(Command::DeleteClip(p.id(0)));
    h.wait_changed();
    h.send(Command::Undo);
    h.wait_changed();

    // The clip is live, so the move remaps it, and leaves the redo alone.
    h.send(Command::MoveSource { from: 0, to: 1 });
    assert_eq!(h.wait_changed().project.clips[0].source_index, 0);

    h.send(Command::Redo);
    assert!(h.wait_changed().project.clips.is_empty());
    assert!(!p.in_recordings(0));
    h.send(Command::Undo);
    let restored = h.wait_changed().project.clips[0].clone();
    assert_eq!(restored.id, p.id(0));
    assert_eq!(restored.source_index, 0, "restored the stale snapshot");
    h.shutdown();
    assert_eq!(p.saved().clips[0].source_index, 0);
    assert!(p.in_recordings(0));
}

#[test]
fn reorder_and_sort_are_undoable() {
    // Clips on b, a, b: sorted is 1, 0, 2.
    let (mut h, p) = Proj::open(&["a.webm", "b.webm"], &[1, 0, 1]);
    let original = ids(&p.saved());
    let [c0, c1, c2] = [p.id(0), p.id(1), p.id(2)];

    h.send(Command::MoveClip { from: 0, to: 2 });
    assert_eq!(ids(&h.wait_changed().project), [c1, c2, c0]);
    h.send(Command::Undo);
    assert_eq!(ids(&h.wait_changed().project), original);

    h.send(Command::SortClipsBySource);
    assert_eq!(ids(&h.wait_changed().project), [c1, c0, c2]);
    // Already sorted, and a move onto itself: nothing to do.
    h.send(Command::SortClipsBySource);
    h.send(Command::MoveClip { from: 1, to: 1 });
    h.send(Command::Undo);
    assert_eq!(ids(&h.wait_changed().project), original);
    h.send(Command::Redo);
    let sorted = h.wait_changed().project;
    assert_eq!(ids(&sorted), [c1, c0, c2]);
    let indices: Vec<i64> = sorted.clips.iter().map(|c| c.sort_index).collect();
    assert_eq!(indices, [0, 1, 2]);

    let rest = h.shutdown();
    no_project_changed(&rest);
    assert_eq!(ids(&p.saved()), [c1, c0, c2]);
}

#[test]
fn a_jump_pauses_at_the_clip_and_outlasts_a_skip_burst() {
    let (mut h, p) = Proj::open(&["a.webm", "b.webm"], &[1]);
    settle_at(&mut h, 0, 0.0);
    h.toggle_play();
    assert!(h.wait_playing());

    // A burst in progress: its debounce and follow-up seeks are pending.
    h.skip(1.0);
    h.skip(0.25);
    h.send(Command::JumpToClip(p.id(0)));
    assert!(!h.wait_playing());
    settle_at(&mut h, 1, 0.5);

    // Well past the burst's debounce, it hasn't moved.
    std::thread::sleep(Duration::from_millis(500));
    settle_at(&mut h, 1, 0.5);
    let rest = h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Playing(true))),
        "{rest:#?}"
    );
}

#[test]
fn opening_empties_the_trash_and_the_history() {
    let (mut h, p) = Proj::open(&["a.webm"], &[0, 0]);

    for (i, reopen) in [
        (0, Command::OpenProject(p.folder.clone())),
        (1, Command::RestoreLastProject),
    ] {
        h.send(Command::DeleteClip(p.id(i)));
        h.wait_changed();
        assert!(!p.in_recordings(i));

        h.send(reopen);
        h.wait_opened();
        assert!(!p.trash().exists(), "clip {i}: the trash survived");
        h.send(Command::Undo);
    }
    let rest = h.shutdown();
    no_project_changed(&rest);
    assert!(p.saved().clips.is_empty());
}

#[test]
fn clip_commands_are_refused_while_recording() {
    let (mut h, p) = Proj::open(&["a.webm"], &[0]);
    settle_at(&mut h, 0, 0.0);
    h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(h.wait_recording(), RecordingStatus::Starting);
    assert!(matches!(
        h.wait_recording(),
        RecordingStatus::Recording { .. }
    ));

    h.send(Command::DeleteClip(p.id(0)));
    h.send(Command::StopRecording);
    // The recording's clip is the first change: the delete never happened.
    let changed = h.wait_changed();
    assert_eq!(changed.project.clips.len(), 2);
    assert_eq!(changed.project.clips[0].id, p.id(0));
    h.shutdown();
    assert_eq!(p.saved().clips.len(), 2);
    assert!(p.in_recordings(0));
}
