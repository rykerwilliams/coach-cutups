//! Bus end to end: an avatar project records commentary with no camera
//! (avatar spec C), and a camera project records exactly as it always did.
//!
//! The pair is the point. Without the camera test the first one would pass on
//! a bug: `capture_sources` returns early for `CaptureKind::Test`, so an
//! avatar branch written only into the device path would leave every test
//! recording with video whatever the project said.

use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;
use video_coach_app::bus::{Command, Event, RecordingStatus};
use video_coach_core::project::{Clip, Inset};
use video_coach_core::store;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{write_project, Harness};
use video_coach_media::{probe, ProbeError};

/// Records one short take over a fixture video in a project with `avatar`
/// set or not, and returns the clip and the file it wrote. The temp directory
/// comes back so it outlives the paths.
fn record_a_take(avatar: Option<&str>) -> (Clip, PathBuf, TempDir) {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("project");
    let media = tmp.path().join("media");
    std::fs::create_dir(&folder).unwrap();
    std::fs::create_dir(&media).unwrap();
    let mut project = write_project(&folder, &media, &[("a.webm", 4)]);
    if let Some(avatar) = avatar {
        // The image *is* the mode; recording never opens the file, so this
        // test needs no picture.
        project.avatar = Some(avatar.into());
        store::write(&folder, &mut project).unwrap();
    }

    let mut h = Harness::new(&tmp.path().join("config"));
    h.send(Command::OpenProject(folder.clone()));
    h.wait_opened();
    h.wait_settled();
    h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(h.wait_recording(), RecordingStatus::Starting);
    assert!(matches!(
        h.wait_recording(),
        RecordingStatus::Recording { .. }
    ));
    // A mic level proves the audio branch is running in both modes.
    h.wait_map("a mic level", |e| match e {
        Event::Level { .. } => Some(()),
        _ => None,
    });
    std::thread::sleep(Duration::from_millis(500));
    h.send(Command::StopRecording);
    let changed = h.wait_changed();
    assert_eq!(h.wait_recording(), RecordingStatus::Idle);
    h.shutdown();

    let clip = changed
        .project
        .clips
        .last()
        .expect("the take made a clip")
        .clone();
    let path = folder.join("recordings").join(&clip.recording_filename);
    (clip, path, tmp)
}

#[test]
fn an_avatar_project_records_without_a_camera() {
    let (clip, path, _tmp) = record_a_take(Some("avatar.png"));
    assert_eq!(clip.inset, Inset::Avatar);
    assert_eq!(probe(&path), Err(ProbeError::NoVideo));
}

#[test]
fn a_camera_project_still_records_with_one() {
    let (clip, path, _tmp) = record_a_take(None);
    assert_eq!(clip.inset, Inset::Camera);
    probe(&path).expect("a camera take records video");
}
