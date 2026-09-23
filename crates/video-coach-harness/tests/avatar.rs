//! Bus end to end: an avatar project records commentary with no camera
//! (avatar spec C), and a camera project records exactly as it always did.
//!
//! The pair is the point. Without the camera test the first one would pass on
//! a bug: `capture_sources` returns early for `CaptureKind::Test`, so an
//! avatar branch written only into the device path would leave every test
//! recording with video whatever the project said.
//!
//! Picking the image itself is here too (spec A2): decode, copy, save.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use video_coach_app::bus::{Command, Event, RecordingStatus};
use video_coach_core::project::{Clip, Inset};
use video_coach_core::store;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{write_project, Harness};
use video_coach_media::fixtures::{self, StillFormat};
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

/// A project with one fixture source, open on a harness. The temp directory
/// comes back so it outlives the folder; `media` holds the fixtures the
/// project does **not** own, so what the project folder holds is exactly what
/// the avatar commands put there.
fn open_a_project() -> (Harness, PathBuf, PathBuf, TempDir) {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("project");
    let media = tmp.path().join("media");
    std::fs::create_dir(&folder).unwrap();
    std::fs::create_dir(&media).unwrap();
    write_project(&folder, &media, &[("a.webm", 2)]);

    let mut h = Harness::new(&tmp.path().join("config"));
    h.send(Command::OpenProject(folder.clone()));
    h.wait_opened();
    (h, folder, media, tmp)
}

/// The project folder's own image files, by name, and any temp file left
/// behind.
fn images_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("avatar") || n.ends_with(".tmp"))
        .collect();
    names.sort();
    names
}

#[test]
fn picking_an_avatar_copies_it_into_the_project() {
    let (mut h, folder, media, _tmp) = open_a_project();
    let png = fixtures::still_image(&media, "face.png", 64, 64, StillFormat::Png);
    let jpeg = fixtures::still_image(&media, "face.jpg", 64, 64, StillFormat::Jpeg);
    let not_an_image = media.join("notes.png");
    std::fs::write(&not_an_image, "this is not an image").unwrap();

    h.send(Command::SetAvatar(png.clone()));
    let saved = h.wait_changed();
    assert_eq!(saved.project.avatar.as_deref(), Some("avatar.png"));
    assert_eq!(images_in(&folder), ["avatar.png"]);
    // The copy is the coach's file, byte for byte, and their own is untouched.
    assert_eq!(
        std::fs::read(folder.join("avatar.png")).unwrap(),
        std::fs::read(&png).unwrap()
    );
    assert_eq!(
        store::read(&folder).unwrap().avatar.as_deref(),
        Some("avatar.png"),
        "the pick is saved"
    );

    // Replacing deletes exactly the file the project named, and leaves no
    // temp file behind.
    h.send(Command::SetAvatar(jpeg));
    let replaced = h.wait_changed();
    assert_eq!(replaced.project.avatar.as_deref(), Some("avatar.jpg"));
    assert_eq!(images_in(&folder), ["avatar.jpg"]);

    // A file that won't decode is refused before anything is copied.
    h.send(Command::SetAvatar(not_an_image));
    let e = h.wait_for_error();
    assert!(e.is_notice(), "the refusal is a notice: {e}");
    assert_eq!(images_in(&folder), ["avatar.jpg"]);
    assert_eq!(
        store::read(&folder).unwrap().avatar.as_deref(),
        Some("avatar.jpg"),
        "a refused pick changes nothing"
    );

    // Remove puts the project back on the camera.
    h.send(Command::ClearAvatar);
    let cleared = h.wait_changed();
    assert_eq!(cleared.project.avatar, None);
    assert!(images_in(&folder).is_empty());
    assert_eq!(store::read(&folder).unwrap().avatar, None);
    // The coach's own files are theirs: only the project's copy went.
    assert!(png.exists());

    h.shutdown();
}
