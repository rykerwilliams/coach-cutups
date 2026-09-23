//! Bus end to end: clip preview (Phase 7 spec P5) — the bus's behavior around
//! the composite, not the picture it makes. What lands in the frame is the
//! media crate's `tests/preview.rs`.
//!
//! The preview paces itself against the commentary's clock, so a test's wall
//! time is its clip's: clips here are 2 s of 320×180, and the composite runs
//! on the process's surfaceless GL context (spec P1: "no private GL context"
//! is an app rule, not a test rule), llvmpipe on CI.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project, its `recordings/` and (once one runs) its `exports/`, and
//! `<tmp>/media` the fixture game video.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{Command, Event, RecordingStatus, UserError};
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Quality, Resolution};
use video_coach_core::store;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{clip, write_project, Harness};
use video_coach_media::fixtures;

/// The clip's commentary recording, inside the project's `recordings/`.
const RECORDING: &str = "commentary.webm";
/// Its frame rate, and the composite's.
const FPS: f64 = 30.0;

/// A project with a 3-second fixture game video and one clip on it whose
/// recording is a real (generated) file, opened on a fresh bus.
struct Rig {
    h: Harness,
    clip: Uuid,
    folder: PathBuf,
    tmp: TempDir,
}

impl Rig {
    /// The clip's commentary lasts `secs`, which is the preview's length.
    fn open(secs: f64) -> Self {
        Self::open_with(secs, |_| {})
    }

    /// [`Rig::open`], with `before_open` run on the media folder first.
    fn open_with(secs: f64, before_open: impl FnOnce(&Path)) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        for dir in [&folder, &media] {
            std::fs::create_dir(dir).unwrap();
        }
        let mut project = write_project(&folder, &media, &[("a.webm", 3)]);
        let mut c = clip(0);
        c.name = "Chance".into();
        c.start_source_seconds = 0.0;
        c.recording_duration = secs;
        c.recording_filename = RECORDING.into();
        let id = c.id;
        project.clips.push(c);
        store::write(&folder, &mut project).unwrap();

        // A real file, not `add_clips`'s stand-in: the recording branch plays
        // natively, so a text file would fail the graph instead of the guard.
        let recordings = folder.join(store::RECORDINGS_DIRNAME);
        std::fs::create_dir_all(&recordings).unwrap();
        let frames = (secs * FPS).round().max(1.0) as u32;
        fixtures::solid_video(
            &recordings.join(RECORDING),
            320,
            180,
            FPS as u32,
            frames,
            0x0000_00ff,
            true,
        );
        before_open(&media);

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        Rig {
            h,
            clip: id,
            folder,
            tmp,
        }
    }

    fn preview(&self) {
        self.h.send(Command::OpenPreview(self.clip));
    }

    /// Opens a preview and waits until it is on screen and playing.
    fn previewing(&mut self) {
        self.preview();
        assert_eq!(self.h.wait_preview(), Some(self.clip));
        assert!(self.h.wait_playing(), "a preview starts playing");
    }

    fn recording(&self) -> PathBuf {
        self.folder.join(store::RECORDINGS_DIRNAME).join(RECORDING)
    }

    /// Exports the clip as a one-clip target, into the project's `exports/`.
    fn export(&self) {
        self.h.send(Command::Export {
            targets: vec![ExportTarget::Clip(self.clip)],
            resolution: Resolution::R720,
            quality: Quality::Low,
            scoreboard: None,
        });
    }
}

fn no_preview_events(rest: &[Event]) {
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Preview(_))),
        "{rest:#?}"
    );
}

/// The transport over a preview: it opens, plays, takes the scrubber's seek
/// and closes. Where a seek lands is the media crate's
/// `a_seek_lands_on_the_frame_it_asked_for`; what this asserts is that the
/// commands reach the preview at all.
#[test]
fn a_preview_opens_plays_seeks_and_closes() {
    let mut rig = Rig::open(2.0);
    rig.previewing();

    // The position the UI's tick reads comes from the preview while one is
    // open (spec P3), and it moves.
    rig.h
        .poll_until("the preview to play on", |h| h.preview_secs() > 0.2);
    // A scrub release is one frame-accurate seek, over the clip's own
    // duration rather than the concat timeline.
    rig.h.send(Command::ScrubRelease { abs: 0.1 });

    rig.h.send(Command::ClosePreview);
    assert!(!rig.h.wait_playing());
    assert_eq!(rig.h.wait_preview(), None);
    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Error(_))),
        "{rest:#?}"
    );
}

/// Exclusivity (P5): the game video is paused, not unloaded, so closing the
/// preview hands it back where it was.
#[test]
fn a_preview_pauses_the_game_video_and_gives_it_back() {
    let mut rig = Rig::open(2.0);
    rig.h.toggle_play();
    assert!(rig.h.wait_playing());
    rig.h.poll_until("the game video to play on", |h| {
        h.position_secs().is_some_and(|p| p > 0.2)
    });
    let before = rig.h.position_secs().unwrap();

    rig.preview();
    assert!(!rig.h.wait_playing(), "the game video pauses first");
    assert_eq!(rig.h.wait_preview(), Some(rig.clip));
    assert!(rig.h.wait_playing(), "then the preview plays");

    rig.h.send(Command::ClosePreview);
    assert!(!rig.h.wait_playing());
    assert_eq!(rig.h.wait_preview(), None);
    // The close re-requests where the game video already is, so that its own
    // frame replaces the composited one; it answers again once that lands.
    rig.h.poll_until("the game video to answer again", |h| {
        h.position_secs().is_some()
    });
    let after = rig
        .h
        .position_secs()
        .expect("the game video is still loaded");
    assert!((after - before).abs() < 0.5, "{before} -> {after}");
    rig.h.shutdown();
}

/// A preview and an export never overlap: whichever is running refuses the
/// other (P5).
#[test]
fn a_preview_and_an_export_refuse_each_other() {
    let mut rig = Rig::open(2.0);
    rig.previewing();
    rig.export();
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantExport("a preview is open; close it first".into())
    );

    rig.h.send(Command::ClosePreview);
    assert_eq!(rig.h.wait_preview(), None);
    rig.export();
    assert!(rig.h.wait_export().is_running());
    rig.preview();
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantPreview("an export is running".into())
    );

    rig.h.send(Command::CancelExport);
    let rest = rig.h.shutdown();
    no_preview_events(&rest);
}

/// A recording is made over the game video, so it waits for the preview to
/// close (P5).
#[test]
fn a_recording_is_refused_while_previewing() {
    let mut rig = Rig::open(2.0);
    rig.previewing();
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantRecord("a preview is open; close it first")
    );
    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Recording(_))),
        "{rest:#?}"
    );
}

/// The recording guard drops it, as it does an export: the UI greys the
/// Preview button and the menu item out, so it's only reached through a UI
/// bug.
#[test]
fn a_preview_while_recording_is_dropped() {
    let mut rig = Rig::open(2.0);
    rig.h.poll_until("settled at the start", |h| {
        let settled = h.log().iter().rev().find_map(|e| match e {
            Event::Position { target_abs, .. } => Some(target_abs.is_none()),
            _ => None,
        });
        settled == Some(true) && h.position_secs().is_some_and(|p| p.abs() < 1.0 / FPS)
    });
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);

    rig.preview();
    rig.h.send(Command::StopRecording);
    let rest = rig.h.shutdown();
    no_preview_events(&rest);
}

/// The clip's recording is about to move into the trash, so the preview
/// showing it closes first (P5).
#[test]
fn deleting_the_previewed_clip_closes_the_preview() {
    let mut rig = Rig::open(2.0);
    rig.previewing();
    rig.h.send(Command::DeleteClip(rig.clip));

    assert_eq!(rig.h.wait_preview(), None);
    // The close comes before the project without the clip is published.
    assert!(rig.h.wait_changed().project.clips.is_empty());
    assert!(!rig.recording().exists(), "its recording was trashed");
    rig.h.shutdown();
}

/// A clip that isn't there, and one whose commentary recording has gone.
#[test]
fn a_preview_needs_its_clip_and_its_recording() {
    let mut rig = Rig::open(2.0);
    std::fs::remove_file(rig.recording()).unwrap();
    rig.preview();
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantPreview("the clip's commentary recording is missing".into())
    );

    rig.h.send(Command::OpenPreview(Uuid::new_v4()));
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantPreview("the clip is gone".into())
    );
    let rest = rig.h.shutdown();
    no_preview_events(&rest);
}

#[test]
fn a_preview_needs_its_game_video() {
    let mut rig = Rig::open_with(2.0, |media| {
        std::fs::remove_file(media.join("a.webm")).unwrap();
    });
    rig.preview();
    assert_eq!(
        rig.h.wait_for_error(),
        UserError::CantPreview("the clip's game video is missing; relink it first".into())
    );
    let rest = rig.h.shutdown();
    no_preview_events(&rest);
    drop(rig.tmp);
}
