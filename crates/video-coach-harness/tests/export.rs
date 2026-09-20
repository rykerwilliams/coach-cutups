//! Bus end to end: export runs (Phase 5 spec X4, Phase 8 specs E1, E5 and
//! E6) — the bus's own behavior. The files' contents are the media crate's
//! tests.
//!
//! Runs here are short (1 s, 30 frames a target) or cancelled early, and
//! exported at 720p: on CI they run on llvmpipe, at a fraction of a second
//! per frame.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project (and, once a run starts, its `exports/`), `<tmp>/media` the
//! fixture game video.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{Command, Event, ExportRun, RecordingStatus, TargetState, UserError};
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Quality, Resolution};
use video_coach_core::store::{self, EXPORTS_DIRNAME, RECORDINGS_DIRNAME};
use video_coach_core::zoom::Zoom;
use video_coach_harness::{add_clips, write_project, Harness};

const FRAME: f64 = 1.0 / 30.0;

/// A project called `Game` with a 2-second fixture video and one clip per
/// entry of `secs`, opened on a fresh bus.
///
/// Clip `i` is called `clip i` and carries the single tag `ti`, so every clip
/// is a target of its own beside All clips.
struct Rig {
    h: Harness,
    clips: Vec<Uuid>,
    /// `<project>/exports`, which no run has created yet.
    exports: PathBuf,
    tmp: TempDir,
}

impl Rig {
    /// Clip `i` lasts `secs[i]`: past the video's end it freezes on its last
    /// frame.
    fn open(secs: &[f64]) -> Self {
        Self::open_with(secs, |_, _| {})
    }

    /// [`Rig::open`], with `before_open` run on the project and media folders
    /// first.
    fn open_with(secs: &[f64], before_open: impl FnOnce(&Path, &Path)) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        for dir in [&folder, &media] {
            std::fs::create_dir(dir).unwrap();
        }
        let mut project = write_project(&folder, &media, &[("a.webm", 2)]);
        let clips = add_clips(&folder, &mut project, &vec![0; secs.len()])
            .iter()
            .map(|c| c.id)
            .collect();
        for (i, clip) in project.clips.iter_mut().enumerate() {
            clip.recording_duration = secs[i];
            clip.name = format!("clip {i}");
            clip.tags = vec![format!("t{i}")];
        }
        store::write(&folder, &mut project).unwrap();
        before_open(&folder, &media);

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        Rig {
            h,
            clips,
            exports: folder.join(EXPORTS_DIRNAME),
            tmp,
        }
    }

    /// Runs `targets`, smallest and cheapest: these render on llvmpipe.
    fn export(&self, targets: Vec<ExportTarget>) {
        self.h.send(Command::Export {
            targets,
            resolution: Resolution::R720,
            quality: Quality::Low,
        });
    }

    fn tag(&self, n: usize) -> ExportTarget {
        ExportTarget::Tag(format!("t{n}"))
    }
}

/// The files in `exports/`, sorted, `.part` files included. Empty when no run
/// has created the folder.
fn outputs(exports: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(exports)
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The run's last word: the event with nothing left running.
fn outcome(h: &mut Harness) -> ExportRun {
    h.wait_map("the run's outcome", |e| match e {
        Event::Export(run) if !run.is_running() => Some(run.clone()),
        _ => None,
    })
}

fn no_export_events(rest: &[Event]) {
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Export(_))),
        "{rest:#?}"
    );
}

/// Two targets, one after the other: the frames left only fall, every target
/// ends written, and the files are named as spec E6 says.
#[test]
fn a_run_renders_every_target_and_its_frames_only_fall() {
    let mut rig = Rig::open(&[1.0, 1.0]);
    rig.export(vec![rig.tag(0), ExportTarget::Clip(rig.clips[1])]);

    let first = rig.h.wait_export();
    let labels: Vec<&str> = first.targets.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(labels, ["t0", "clip 1"]);
    assert_eq!(first.targets.iter().map(|t| t.frames).sum::<usize>(), 60);
    assert_eq!(first.remaining_frames(), 60);
    assert!(first.is_running());

    let mut left = first.remaining_frames();
    let done = loop {
        let run = rig.h.wait_export();
        let now = run.remaining_frames();
        assert!(now <= left, "{now} frames left after {left}");
        left = now;
        if !run.is_running() {
            break run;
        }
    };
    assert_eq!(done.remaining_frames(), 0);
    for target in &done.targets {
        assert!(matches!(target.state, TargetState::Done(_)), "{target:?}");
    }
    assert_eq!(
        outputs(&rig.exports),
        ["clip 1 - Game.mp4", "t0 - Game.mp4"]
    );
    rig.h.shutdown();
}

/// Two targets that would be called the same thing — a clip named after a tag
/// — get one file each: the label names the file (spec E6), so without the
/// suffix the second target would overwrite the first's output part-way
/// through the run.
#[test]
fn two_targets_with_the_same_name_write_two_files() {
    let mut rig = Rig::open_with(&[1.0, 1.0], |folder, _| {
        let mut project = store::read(folder).unwrap();
        project.clips[1].name = "t0".into();
        store::write(folder, &mut project).unwrap();
    });
    rig.export(vec![rig.tag(0), ExportTarget::Clip(rig.clips[1])]);

    let done = outcome(&mut rig.h);
    let labels: Vec<&str> = done.targets.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(labels, ["t0", "t0 (2)"]);
    assert_eq!(
        outputs(&rig.exports),
        ["t0 (2) - Game.mp4", "t0 - Game.mp4"]
    );
    rig.h.shutdown();
}

/// A target that can't be written reports it and the run carries on: one
/// broken output must not cost the rest of an evening's exports.
#[test]
fn a_failed_target_does_not_stop_the_run() {
    let mut rig = Rig::open_with(&[1.0, 1.0], |folder, _| {
        // A directory where the first target's file goes. It renders, and the
        // move into place then fails -- a target failing, rather than the
        // run being refused before it starts.
        std::fs::create_dir_all(folder.join(EXPORTS_DIRNAME).join("t0 - Game.mp4")).unwrap();
    });
    rig.export(vec![rig.tag(0), rig.tag(1)]);

    let done = outcome(&mut rig.h);
    assert!(
        matches!(done.targets[0].state, TargetState::Failed(_)),
        "{:?}",
        done.targets[0]
    );
    assert!(
        matches!(done.targets[1].state, TargetState::Done(_)),
        "{:?}",
        done.targets[1]
    );
    assert!(rig.exports.join("t1 - Game.mp4").is_file());
    assert!(!rig.exports.join("t0 - Game.mp4.part").exists());
    rig.h.shutdown();
}

/// Cancel stops the run where it stands: the target still rendering loses its
/// file, the ones after it never start, and the one already written stays
/// (spec E5).
#[test]
fn cancel_leaves_the_targets_already_written_alone() {
    // The second target is 300 frames, so the cancel lands well inside it.
    let mut rig = Rig::open(&[1.0, 10.0]);
    rig.export(vec![rig.tag(0), rig.tag(1), ExportTarget::AllClips]);

    rig.h.wait_map("the first target's file", |e| match e {
        Event::Export(run) => matches!(run.targets[0].state, TargetState::Done(_)).then_some(()),
        _ => None,
    });
    rig.h.send(Command::CancelExport);

    let done = outcome(&mut rig.h);
    assert!(
        matches!(done.targets[0].state, TargetState::Done(_)),
        "{:?}",
        done.targets[0]
    );
    assert_eq!(done.targets[1].state, TargetState::Cancelled);
    assert_eq!(done.targets[2].state, TargetState::Cancelled);
    assert_eq!(outputs(&rig.exports), ["t0 - Game.mp4"]);
    rig.h.shutdown();
}

/// One run at a time, and never alongside a recording.
#[test]
fn while_a_run_is_going_a_second_run_and_recording_are_refused() {
    let mut rig = Rig::open(&[10.0]);
    rig.export(vec![ExportTarget::AllClips]);
    rig.export(vec![ExportTarget::AllClips]);
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });

    assert!(rig.h.wait_export().is_running());
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("an export is running".into())
    );
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantRecord("an export is running")
    );
    rig.h.send(Command::CancelExport);
    assert_eq!(outcome(&mut rig.h).targets[0].state, TargetState::Cancelled);
    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Recording(_))),
        "{rest:#?}"
    );
    assert!(
        outputs(&rig.exports).is_empty(),
        "{:?}",
        outputs(&rig.exports)
    );
}

/// A missing game video is refused before anything renders, naming the clip:
/// media only warns about one and would export an hour of black.
#[test]
fn a_missing_game_video_is_refused_up_front_naming_the_clip() {
    let mut rig = Rig::open_with(&[1.0], |_, media| {
        std::fs::remove_file(media.join("a.webm")).unwrap();
    });
    rig.export(vec![ExportTarget::AllClips]);
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("clip 0's game video is missing; relink it first".into())
    );
    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(
        !rig.exports.exists(),
        "a refused run made {:?}",
        rig.exports
    );
}

/// And so is a missing commentary recording, which media would degrade to
/// silence and a black inset.
#[test]
fn a_missing_recording_is_refused_up_front_naming_the_clip() {
    let mut rig = Rig::open_with(&[1.0], |folder, _| {
        for entry in std::fs::read_dir(folder.join(RECORDINGS_DIRNAME)).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
    });
    rig.export(vec![ExportTarget::AllClips]);
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("clip 0's commentary recording is missing".into())
    );
    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(
        !rig.exports.exists(),
        "a refused run made {:?}",
        rig.exports
    );
}

/// A target with no clips, or no frames, or a clip that has been deleted:
/// each is refused by name rather than producing an empty file.
#[test]
fn a_target_with_nothing_in_it_is_refused() {
    let mut rig = Rig::open(&[0.0]);
    rig.export(vec![ExportTarget::AllClips]);
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("All clips has nothing to export".into())
    );

    rig.export(vec![ExportTarget::Tag("nobody".into())]);
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("nobody has nothing to export".into())
    );

    rig.export(vec![ExportTarget::Clip(Uuid::new_v4())]);
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("the clip is gone".into())
    );

    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(
        !rig.exports.exists(),
        "a refused run made {:?}",
        rig.exports
    );
}

/// The pickers' values become the project's, so the next run uses them
/// without being told (spec E4).
#[test]
fn a_run_persists_the_resolution_and_quality() {
    let mut rig = Rig::open(&[1.0]);
    rig.export(vec![ExportTarget::AllClips]);
    let snapshot = rig.h.wait_changed();
    let prefs = &snapshot.project.preferences;
    assert_eq!(prefs.last_export_resolution, Resolution::R720);
    assert_eq!(prefs.last_export_quality, Quality::Low);

    outcome(&mut rig.h);
    let saved = store::read(&rig.tmp.path().join("project")).unwrap();
    assert_eq!(saved.preferences.last_export_resolution, Resolution::R720);
    rig.h.shutdown();
}

/// The recording guard drops it: the UI greys the menu item out, so it's
/// only reached through a UI bug.
#[test]
fn an_export_while_recording_is_dropped() {
    let mut rig = Rig::open(&[1.0]);
    rig.h.poll_until("settled at the start", |h| {
        let settled = h.log().iter().rev().find_map(|e| match e {
            Event::Position { target_abs, .. } => Some(target_abs.is_none()),
            _ => None,
        });
        settled == Some(true) && h.position_secs().is_some_and(|p| p.abs() < FRAME)
    });
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);

    rig.export(vec![ExportTarget::AllClips]);
    rig.h.send(Command::StopRecording);
    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Error(_))),
        "{rest:#?}"
    );
    assert!(
        !rig.exports.exists(),
        "a dropped run made {:?}",
        rig.exports
    );
}
