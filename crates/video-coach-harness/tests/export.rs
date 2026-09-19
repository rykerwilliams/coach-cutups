//! Bus end to end: export (Phase 5 spec X4) — the bus's own behavior. The
//! file's contents are the media crate's tests.
//!
//! Exports here are short (1 s, 30 frames) or cancelled early: on CI they run
//! on llvmpipe, at a fraction of a second per 1080p frame.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project, `<tmp>/media` the fixture game video, `<tmp>/out` the exports.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{Command, Event, ExportStatus, RecordingStatus, UserError};
use video_coach_core::store;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{add_clips, write_project, Harness};

const FRAME: f64 = 1.0 / 30.0;

/// A project with a 2-second fixture video and one clip on it, opened on a
/// fresh bus.
struct Rig {
    h: Harness,
    clip: Uuid,
    out: PathBuf,
    tmp: TempDir,
}

impl Rig {
    /// The clip lasts `secs`: past the video's end, it freezes on its last
    /// frame.
    fn open(secs: f64) -> Self {
        Self::open_with(secs, |_| {})
    }

    /// [`Rig::open`], with `before_open` run on the media folder first.
    fn open_with(secs: f64, before_open: impl FnOnce(&Path)) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        let out = tmp.path().join("out");
        for dir in [&folder, &media, &out] {
            std::fs::create_dir(dir).unwrap();
        }
        let mut project = write_project(&folder, &media, &[("a.webm", 2)]);
        let clip = add_clips(&folder, &mut project, &[0])[0].id;
        project.clips[0].recording_duration = secs;
        store::write(&folder, &mut project).unwrap();
        before_open(&media);

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder));
        h.wait_opened();
        Rig { h, clip, out, tmp }
    }

    /// Exports the clip to `name` in `out`.
    fn export(&self, name: &str) -> PathBuf {
        let path = self.out.join(name);
        self.h.send(Command::ExportClip {
            id: self.clip,
            path: path.clone(),
        });
        path
    }
}

/// The files in `out`, by name, `.part` files included.
fn outputs(out: &Path) -> Vec<String> {
    std::fs::read_dir(out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

/// How the running export ends, past its progress.
fn outcome(h: &mut Harness) -> ExportStatus {
    h.wait_map("the export's outcome", |e| match e {
        Event::Export(ExportStatus::Running(_)) => None,
        Event::Export(s) => Some(s.clone()),
        _ => None,
    })
}

fn no_export_events(rest: &[Event]) {
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Export(_))),
        "{rest:#?}"
    );
}

#[test]
fn an_export_reports_progress_then_its_file() {
    let mut rig = Rig::open(1.0);
    let path = rig.export("clip.mp4");

    assert_eq!(rig.h.wait_export(), ExportStatus::Running(0));
    let mut last = 0;
    let done = loop {
        match rig.h.wait_export() {
            ExportStatus::Running(p) => {
                assert!(p > last && p <= 100, "{p} after {last}");
                last = p;
            }
            status => break status,
        }
    };
    assert_eq!(done, ExportStatus::Done(path.clone()));
    assert_eq!(last, 100);
    assert!(std::fs::metadata(&path).unwrap().len() > 0);
    assert_eq!(outputs(&rig.out), ["clip.mp4"]);
    rig.h.shutdown();
}

/// A failed export reports it, and doesn't block the next.
#[test]
fn a_failed_export_is_reported_and_the_next_one_runs() {
    let mut rig = Rig::open(1.0);
    rig.export("missing/clip.mp4");
    assert_eq!(rig.h.wait_export(), ExportStatus::Running(0));
    let status = outcome(&mut rig.h);
    assert!(matches!(status, ExportStatus::Failed(_)), "{status:?}");

    let path = rig.export("clip.mp4");
    assert_eq!(outcome(&mut rig.h), ExportStatus::Done(path));
    assert_eq!(outputs(&rig.out), ["clip.mp4"]);
    rig.h.shutdown();
}

/// One export at a time, and never alongside a recording. Cancel stops the
/// one running, leaving nothing.
#[test]
fn while_exporting_a_second_export_and_recording_are_refused() {
    // 300 frames: far from done when the cancel lands.
    let mut rig = Rig::open(10.0);
    rig.export("first.mp4");
    rig.export("second.mp4");
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });

    assert_eq!(rig.h.wait_export(), ExportStatus::Running(0));
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("an export is running".into())
    );
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantRecord("an export is running")
    );
    rig.h.send(Command::CancelExport);
    assert_eq!(outcome(&mut rig.h), ExportStatus::Cancelled);
    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Recording(_))),
        "{rest:#?}"
    );
    assert!(outputs(&rig.out).is_empty(), "{:?}", outputs(&rig.out));
}

#[test]
fn an_export_needs_its_clip_and_game_video() {
    let mut rig = Rig::open_with(1.0, |media| {
        std::fs::remove_file(media.join("a.webm")).unwrap();
    });
    rig.export("clip.mp4");
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("the clip's game video is missing; relink it first".into())
    );
    rig.h.send(Command::ExportClip {
        id: Uuid::new_v4(),
        path: rig.out.join("other.mp4"),
    });
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("the clip is gone".into())
    );
    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(outputs(&rig.out).is_empty(), "{:?}", outputs(&rig.out));
}

#[test]
fn an_export_never_writes_over_its_game_video() {
    let mut rig = Rig::open(1.0);
    let source = rig.tmp.path().join("media/a.webm");
    rig.h.send(Command::ExportClip {
        id: rig.clip,
        path: source.clone(),
    });
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("that file is the clip's game video".into())
    );
    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(source.exists());
}

#[test]
fn an_export_needs_frames() {
    let mut rig = Rig::open(0.0);
    rig.export("clip.mp4");
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("the clip has nothing to export".into())
    );
    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(outputs(&rig.out).is_empty(), "{:?}", outputs(&rig.out));
}

/// The recording guard drops it: the UI greys the menu item out, so it's
/// only reached through a UI bug.
#[test]
fn an_export_while_recording_is_dropped() {
    let mut rig = Rig::open(1.0);
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

    rig.export("clip.mp4");
    rig.h.send(Command::StopRecording);
    let rest = rig.h.shutdown();
    no_export_events(&rest);
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Error(_))),
        "{rest:#?}"
    );
    assert!(outputs(&rig.out).is_empty(), "{:?}", outputs(&rig.out));
}
