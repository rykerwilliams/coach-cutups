//! Bus end to end: recording (Phase 4 spec R6, R10) with test capture
//! sources — no camera, microphone or display.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project and its `recordings/`, `<tmp>/media` the fixture game videos.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{CaptureKind, Command, Event, RecordingStatus, UserError};
use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::project::{Clip, Project};
use video_coach_core::store;
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_core::timeline;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{write_project, Harness, FRAME};
use video_coach_media::{fixtures, now_ns};

/// A camera slow enough to warm up that a test can act before its first
/// frame.
const SLOW_CAMERA: CaptureKind = CaptureKind::Test {
    video_delay: Duration::from_secs(2),
};

/// A project of fixture videos, opened on a fresh bus that has settled on
/// the first source.
struct Rig {
    h: Harness,
    project: Project,
    folder: PathBuf,
    /// The project's `recordings/`.
    recordings: PathBuf,
    tmp: TempDir,
}

impl Rig {
    fn open(videos: &[(&str, u32)]) -> Self {
        Self::open_with(
            videos,
            CaptureKind::Test {
                video_delay: Duration::ZERO,
            },
        )
    }

    fn open_with(videos: &[(&str, u32)], capture: CaptureKind) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&media).unwrap();
        let project = write_project(&folder, &media, videos);

        let mut h = Harness::with_capture(&tmp.path().join("config"), capture);
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        let mut rig = Rig {
            h,
            project,
            recordings: folder.join("recordings"),
            folder,
            tmp,
        };
        rig.settle_at(0, 0.0);
        rig
    }

    /// Waits until no seek is outstanding, the bus's current source is
    /// `index`, and the pipeline is within a frame of `secs` in it.
    fn settle_at(&mut self, index: usize, secs: f64) {
        self.h
            .poll_until(&format!("settled at {secs} in source {index}"), |h| {
                latest_position(h) == Some((index, None))
                    && h.position_secs().is_some_and(|p| (p - secs).abs() < FRAME)
            });
    }

    /// Starts recording and waits for its first video frame. Returns t0.
    fn record(&mut self) -> u64 {
        self.h.send(Command::ToggleRecording {
            zoom: Zoom::IDENTITY,
        });
        assert_eq!(self.h.wait_recording(), RecordingStatus::Starting);
        match self.h.wait_recording() {
            RecordingStatus::Recording { t0_ns } => t0_ns,
            status => panic!("expected Recording, got {status:?}"),
        }
    }

    /// Stops the recording and returns the clip it made.
    fn stop(&mut self) -> Clip {
        self.h.send(Command::StopRecording);
        let changed = self.h.wait_changed();
        assert_eq!(self.h.wait_recording(), RecordingStatus::Idle);
        changed
            .project
            .clips
            .last()
            .expect("the recording made a clip")
            .clone()
    }
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

/// The files in `dir`; none if it doesn't exist.
fn files(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

fn kinds(events: &[CommentaryEvent]) -> Vec<EventKind> {
    events.iter().map(|e| e.kind.clone()).collect()
}

/// A finished stroke as the UI sends one at pen-up: two points, red, auto-clear
/// on.
fn a_stroke() -> Stroke {
    Stroke {
        id: Uuid::new_v4(),
        color: Rgba::RED,
        line_width: 0.005,
        points: vec![
            StrokePoint {
                x: 0.25,
                y: 0.5,
                t: 0.0,
            },
            StrokePoint {
                x: 0.75,
                y: 0.5,
                t: 0.1,
            },
        ],
        auto_clear_after_seconds: Some(5.0),
    }
}

/// Phase 6 spec D4: both land at the record time the caller's `host_ns` names,
/// so a queued command can't drift the drawing.
#[test]
fn a_stroke_and_a_clear_while_recording_are_logged() {
    let mut rig = Rig::open(&[("a.webm", 2)]);
    let t0 = rig.record();
    let drawn = a_stroke();
    // As the UI captures them: `now_ns()` at the input event, here pinned to
    // t0 so the record times are exact.
    rig.h.send(Command::Stroke {
        host_ns: t0 + 250_000_000,
        stroke: drawn.clone(),
    });
    rig.h.send(Command::ClearAll {
        host_ns: t0 + 500_000_000,
    });
    let clip = rig.stop();

    assert_eq!(
        kinds(&clip.events),
        [
            EventKind::Zoom(Zoom::IDENTITY),
            EventKind::Pause {
                source_time: clip.start_source_seconds
            },
            EventKind::Stroke(drawn),
            EventKind::ClearAll,
        ]
    );
    assert_eq!(clip.events[2].record_time, 0.25);
    assert_eq!(clip.events[3].record_time, 0.5);
    rig.h.shutdown();
}

#[test]
fn a_stroke_and_a_clear_outside_a_recording_are_dropped() {
    let mut rig = Rig::open(&[("a.webm", 2)]);
    rig.h.send(Command::Stroke {
        host_ns: now_ns(),
        stroke: a_stroke(),
    });
    rig.h.send(Command::ClearAll { host_ns: now_ns() });
    // Nothing was held over: the next recording's log opens clean.
    rig.record();
    let clip = rig.stop();

    assert_eq!(
        kinds(&clip.events),
        [
            EventKind::Zoom(Zoom::IDENTITY),
            EventKind::Pause {
                source_time: clip.start_source_seconds
            },
        ]
    );
    rig.h.shutdown();
}

#[test]
fn a_recording_makes_a_clip_where_the_player_was() {
    let mut rig = Rig::open(&[("a.webm", 4)]);
    rig.h.send(Command::ScrubRelease { abs: 1.5 });
    rig.settle_at(0, 1.5);

    let t0 = rig.record();
    rig.h.wait_map("a mic level", |e| match e {
        Event::Level { peak_db, .. } => Some(*peak_db),
        _ => None,
    });
    // Let it run long enough that its duration means something.
    std::thread::sleep(Duration::from_millis(1000));
    let stopped_at = (now_ns() - t0) as f64 / 1e9;
    let clip = rig.stop();

    assert_eq!(clip.source_index, 0);
    assert!(
        (clip.start_source_seconds - 1.5).abs() < FRAME,
        "{}",
        clip.start_source_seconds
    );
    assert_eq!(clip.name, "1-00:00:01");
    // The recorder's duration can include audio that runs ~60 ms past the
    // last video frame.
    assert!(
        (clip.recording_duration - stopped_at).abs() < 0.15,
        "duration {}, stopped at {stopped_at}",
        clip.recording_duration
    );
    assert_eq!(
        kinds(&clip.events),
        [
            EventKind::Zoom(Zoom::IDENTITY),
            EventKind::Pause {
                source_time: clip.start_source_seconds
            },
        ]
    );
    assert!(clip.events.iter().all(|e| e.record_time == 0.0));
    let file = rig.recordings.join(&clip.recording_filename);
    assert_eq!(files(&rig.recordings), std::slice::from_ref(&file));
    assert!(std::fs::metadata(&file).unwrap().len() > 0);
    // And it was saved.
    assert_eq!(store::read(&rig.folder).unwrap().clips, [clip]);
    rig.h.shutdown();
}

/// The camera is shown live while recording, and only then.
#[test]
fn the_camera_is_shown_live_while_recording() {
    let mut rig = Rig::open(&[("a.webm", 4)]);
    assert!(rig.h.take_self_view().is_none());

    rig.record();
    rig.h
        .poll_until("a self-view frame", |h| h.take_self_view().is_some());
    rig.stop();

    // Whatever was on its way at the stop is gone after a take, and nothing
    // follows it.
    rig.h.take_self_view();
    std::thread::sleep(Duration::from_millis(200));
    assert!(rig.h.take_self_view().is_none());
    rig.h.shutdown();
}

/// A second R during start-up cancels.
#[test]
fn stopping_before_the_first_video_frame_leaves_no_clip_and_no_file() {
    let mut rig = Rig::open_with(&[("a.webm", 2)], SLOW_CAMERA);
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Idle);

    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Recording(_))),
        "{rest:#?}"
    );
    assert!(files(&rig.recordings).is_empty());
    assert!(store::read(&rig.folder).unwrap().clips.is_empty());
}

#[test]
fn play_during_the_camera_warm_up_is_logged() {
    let mut rig = Rig::open_with(&[("a.webm", 6)], SLOW_CAMERA);
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);
    rig.h.toggle_play();
    assert!(rig.h.wait_playing());
    assert!(matches!(
        rig.h.wait_recording(),
        RecordingStatus::Recording { .. }
    ));
    let clip = rig.stop();

    let play = &clip.events[2];
    assert!(
        matches!(play.kind, EventKind::Play { source_time } if source_time.abs() < FRAME),
        "{:?}",
        clip.events
    );
    // Pressed after t0, and well before the video's first frame at 2 s.
    assert!(
        play.record_time > 0.0 && play.record_time < 1.5,
        "{}",
        play.record_time
    );
    rig.h.shutdown();
}

#[test]
fn recording_is_refused_while_a_source_is_missing() {
    gstreamer::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("project");
    let media = tmp.path().join("media");
    std::fs::create_dir(&folder).unwrap();
    std::fs::create_dir(&media).unwrap();
    write_project(&folder, &media, &[("a.webm", 2)]);
    std::fs::remove_file(media.join("a.webm")).unwrap();

    let mut h = Harness::new(&tmp.path().join("config"));
    h.send(Command::OpenProject(folder.clone()));
    assert_eq!(*h.wait_opened().missing, [true]);
    h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert!(matches!(h.wait_for_error(), UserError::CantRecord(_)));

    let rest = h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Recording(_))),
        "{rest:#?}"
    );
    assert!(files(&folder.join("recordings")).is_empty());
}

#[test]
fn source_list_changes_are_refused_while_recording() {
    let mut rig = Rig::open(&[("a.webm", 2)]);
    let extra = fixtures::webm(&rig.tmp.path().join("media"), "b.webm", 2, 320, 180, 30, 15);
    rig.record();
    rig.h.send(Command::AddSource(extra));
    let clip = rig.stop();

    let rest = rig.h.shutdown();
    let saved = store::read(&rig.folder).unwrap();
    assert_eq!(saved.source_videos.len(), 1);
    assert_eq!(saved.clips, [clip]);
    assert!(
        !rest
            .iter()
            .any(|e| matches!(e, Event::ProjectChanged(s) if s.project.source_videos.len() > 1)),
        "{rest:#?}"
    );
}

#[test]
fn a_skip_while_recording_stays_in_the_clips_source() {
    let mut rig = Rig::open(&[("a.webm", 2), ("b.webm", 2)]);
    let end_a = rig.project.source_videos[0].duration_seconds;
    rig.record();
    rig.h.skip(10.0);
    rig.settle_at(0, end_a - 0.05);
    let clip = rig.stop();

    assert_eq!(clip.source_index, 0);
    assert!(
        matches!(clip.events[2].kind, EventKind::Skip { delta } if delta == 10.0),
        "the requested delta is logged: {:?}",
        clip.events
    );
    rig.h.shutdown();
}

#[test]
fn a_pause_right_after_a_skip_is_anchored_at_the_skip_target() {
    let mut rig = Rig::open(&[("a.webm", 4)]);
    rig.record();
    rig.h.toggle_play();
    assert!(rig.h.wait_playing());
    rig.h.poll_until("playback past 0.3 s", |h| {
        h.position_secs().is_some_and(|p| p > 0.3)
    });
    // Back to back: the pause is handled while the skip's seek is in flight,
    // when the UI's position still reads from before the skip.
    let before = rig.h.position_secs().unwrap();
    rig.h.skip(1.0);
    rig.h.toggle_play();
    assert!(!rig.h.wait_playing());
    // And the player paused: once the skip lands, the position holds.
    rig.h.poll_until("the skip landed", |h| {
        latest_position(h).is_some_and(|(_, target)| target.is_none())
    });
    let landed = rig.h.position_secs().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let later = rig.h.position_secs().unwrap();
    assert!(
        (later - landed).abs() < FRAME,
        "played from {landed} to {later}"
    );
    let clip = rig.stop();

    let Some(EventKind::Pause { source_time }) = clip.events.last().map(|e| e.kind.clone()) else {
        panic!("ends with the pause: {:?}", clip.events);
    };
    // The skip starts from where the bus found the player, a little after
    // `before`.
    let target = before + 1.0;
    assert!(
        (target - FRAME..target + 0.1).contains(&source_time),
        "{source_time}, from {before}"
    );
    rig.h.shutdown();
}

#[test]
fn a_skip_burst_while_playing_lands_where_replay_puts_it() {
    let mut rig = Rig::open(&[("a.webm", 10)]);
    let duration = rig.project.source_videos[0].duration_seconds;
    rig.record();
    rig.h.toggle_play();
    assert!(rig.h.wait_playing());
    // Replay applies each delta at its press and keeps playing; live has to
    // land there too, not at the burst's base plus the deltas.
    rig.h.skip(3.0);
    rig.h.skip(3.0);
    std::thread::sleep(Duration::from_millis(1500));
    rig.h.toggle_play();
    assert!(!rig.h.wait_playing());
    let clip = rig.stop();

    let Some(pause) = clip.events.last() else {
        panic!("no events");
    };
    let EventKind::Pause { source_time } = pause.kind else {
        panic!("ends with the pause: {:?}", clip.events);
    };
    let replayed = timeline::source_time(&clip, pause.record_time - 1e-6, duration);
    assert!(
        (replayed - source_time).abs() < 0.05,
        "replay reaches {replayed} before the pause anchored at {source_time}"
    );
    rig.h.shutdown();
}

#[test]
fn closing_while_recording_keeps_the_clip() {
    let mut rig = Rig::open(&[("a.webm", 2)]);
    rig.record();
    std::thread::sleep(Duration::from_millis(500));
    rig.h.shutdown();

    let saved = store::read(&rig.folder).unwrap();
    let [clip] = saved.clips.as_slice() else {
        panic!("one clip: {:?}", saved.clips);
    };
    assert!(clip.recording_duration > 0.4, "{}", clip.recording_duration);
    assert!(rig.recordings.join(&clip.recording_filename).is_file());
}
