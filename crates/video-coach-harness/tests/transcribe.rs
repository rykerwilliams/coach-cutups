//! Bus end to end: the transcription queue (Phase 10 spec S5–S7), on the test
//! transcriber — no model, no whisper build, no camera.
//!
//! Every clip here has a **real** recording with sound in it, because the
//! extraction runs before the seam does: the queue is what these tests are
//! about, but a clip whose file can't be read is a failure, and one test
//! wants exactly that.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project and its `recordings/`, `<tmp>/media` the fixture game videos.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{CaptureKind, Command, Event, RecordingStatus};
use video_coach_core::project::{Clip, Project};
use video_coach_core::store;
use video_coach_core::undo::ClipEdit;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{clip, write_project, Harness, Transcription};
use video_coach_media::{fixtures, TranscribeKind};

/// What the test transcriber says.
const WORDS: &str = "he has to shoot there";

/// Long enough that a test can queue behind a running job, preempt one with a
/// recording, or cancel one, without a sleep of its own.
const SLOW: Duration = Duration::from_millis(1_500);

/// A camera slow enough to warm up that a recording can be aborted before its
/// first frame.
const SLOW_CAMERA: CaptureKind = CaptureKind::Test {
    video_delay: Duration::from_secs(2),
};

/// A project of fixture videos and clips with real recordings, opened on a
/// fresh bus.
struct Rig {
    h: Harness,
    folder: PathBuf,
    clips: Vec<Clip>,
    #[expect(dead_code, reason = "kept alive: dropping it deletes the project")]
    tmp: TempDir,
}

impl Rig {
    /// `clips` clips on the one game video, each with a one-second recording
    /// that really has sound in it.
    fn open(clips: usize, delay: Duration) -> Self {
        Self::open_with(clips, SLOW_CAMERA, transcriber(delay))
    }

    fn open_with(clips: usize, capture: CaptureKind, transcribe: TranscribeKind) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&media).unwrap();
        let mut project = write_project(&folder, &media, &[("a.webm", 4)]);
        let added = add_clips_with_sound(&folder, &mut project, clips);

        let mut h = Harness::with_transcribe(&tmp.path().join("config"), capture, transcribe);
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        // The queue the open cleared, so a later wait can't match it.
        h.wait_transcription("the opened project's empty queue", Transcription::is_idle);
        Rig {
            h,
            folder,
            clips: added,
            tmp,
        }
    }

    fn id(&self, i: usize) -> Uuid {
        self.clips[i].id
    }

    /// Waits for a transcription state that `f` accepts, handing it the clip
    /// ids: the harness is borrowed for the wait, so a closure can't reach
    /// back into the rig for them.
    fn wait(&mut self, what: &str, f: impl Fn(&Transcription, &[Uuid]) -> bool) -> Transcription {
        let ids: Vec<Uuid> = self.clips.iter().map(|c| c.id).collect();
        self.h.wait_transcription(what, |t| f(t, &ids))
    }

    fn transcribe(&self, i: usize) {
        self.h.send(Command::Transcribe {
            clip_id: self.id(i),
        });
    }

    /// The saved transcript of clip `i`.
    fn saved_transcript(&self, i: usize) -> String {
        let id = self.id(i);
        store::read(&self.folder)
            .expect("the project reads back")
            .clips
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.transcript.clone())
            .unwrap_or_default()
    }

    /// Waits until the queue has emptied and nothing is running.
    fn wait_idle(&mut self) -> Transcription {
        self.h
            .wait_transcription("an idle queue", Transcription::is_idle)
    }
}

fn transcriber(delay: Duration) -> TranscribeKind {
    TranscribeKind::Test {
        delay,
        text: WORDS.into(),
    }
}

/// Clips on source 0 whose recordings are real one-second files with sound,
/// so the extraction the transcriber runs first has something to read.
fn add_clips_with_sound(folder: &Path, project: &mut Project, n: usize) -> Vec<Clip> {
    let recordings = folder.join(store::RECORDINGS_DIRNAME);
    std::fs::create_dir_all(&recordings).expect("create recordings/");
    let added: Vec<Clip> = (0..n).map(|_| clip(0)).collect();
    for (i, c) in added.iter().enumerate() {
        // The container is WebM under an `.mkv` name; `decodebin3` reads the
        // file, not the extension.
        fixtures::webm(&recordings, &c.recording_filename, 1, 160, 90, 25, 25);
        let mut c = c.clone();
        c.name = format!("clip {i}");
        c.sort_index = i as i64;
        project.clips.push(c);
    }
    store::write(folder, project).expect("write the project with its clips");
    added
}

/// Spec S6: stopping a recording queues its clip, and the words land on it.
#[test]
fn stopping_a_recording_transcribes_its_clip() {
    let mut rig = Rig::open_with(
        0,
        CaptureKind::Test {
            video_delay: Duration::ZERO,
        },
        transcriber(Duration::ZERO),
    );
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);
    assert!(matches!(
        rig.h.wait_recording(),
        RecordingStatus::Recording { .. }
    ));
    std::thread::sleep(Duration::from_millis(300));
    rig.h.send(Command::StopRecording);
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Idle);

    let id = rig
        .h
        .wait_transcription("the new clip running", |t| t.running.is_some())
        .running_clip()
        .expect("just checked");
    let project = rig.h.wait_map("the transcript", |e| match e {
        Event::ProjectChanged(s) => s
            .project
            .clips
            .iter()
            .any(|c| c.id == id && c.transcript == WORDS)
            .then(|| s.project.clone()),
        _ => None,
    });
    assert_eq!(project.clips.len(), 1);
    rig.wait_idle();
    rig.h.shutdown();
}

/// One job at a time, the rest waiting in the order they were asked for, and
/// a clip that is waiting says so (BACKLOG #18's fix).
#[test]
fn the_queue_runs_one_clip_at_a_time_in_order() {
    let mut rig = Rig::open(3, SLOW);
    rig.transcribe(0);
    rig.transcribe(1);
    rig.transcribe(2);

    let queued = rig.wait("all three known", |t, id| {
        t.running_clip() == Some(id[0]) && t.queued.len() == 2
    });
    assert_eq!(queued.queued, [rig.id(1), rig.id(2)]);

    for i in 1..3 {
        rig.wait(&format!("clip {i} running"), |t, id| {
            t.running_clip() == Some(id[i])
        });
    }
    rig.wait_idle();
    for i in 0..3 {
        assert_eq!(rig.saved_transcript(i), WORDS, "clip {i}");
    }
    rig.h.shutdown();
}

/// Enqueueing is idempotent against the queue **and** the clip running: the
/// running clip is not in the queue, so a plain `contains` would re-run it.
#[test]
fn re_enqueueing_a_running_or_queued_clip_does_nothing() {
    let mut rig = Rig::open(2, SLOW);
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait("both known", |t, id| {
        t.running_clip() == Some(id[0]) && t.queued == [id[1]]
    });

    // The one running, and the one waiting.
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait_idle();

    // Two runs, not four: every state the bus published had at most one
    // entry in the queue, and clip 0 never went back into it.
    let first = rig.id(0);
    let states: Vec<&Event> = rig
        .h
        .log()
        .iter()
        .filter(|e| matches!(e, Event::Transcription { .. }))
        .collect();
    for state in &states {
        let Event::Transcription { queued, .. } = state else {
            unreachable!()
        };
        assert!(queued.len() <= 1, "{states:#?}");
        assert!(!queued.contains(&first), "{states:#?}");
    }
    rig.h.shutdown();
}

/// Spec S5: recording always wins. The job in flight is cancelled and its
/// clip goes back to the **front**, so it is the first thing to resume.
#[test]
fn a_recording_preempts_the_running_job_and_it_resumes_first() {
    let mut rig = Rig::open_with(
        2,
        CaptureKind::Test {
            video_delay: Duration::ZERO,
        },
        transcriber(SLOW),
    );
    rig.transcribe(0);
    rig.wait("clip 0 running", |t, id| t.running_clip() == Some(id[0]));

    // The record is not refused, and the transcript gives way to it.
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);
    let preempted = rig.wait("clip 0 preempted", |t, _| {
        t.running.is_none() && !t.queued.is_empty()
    });
    assert_eq!(preempted.queued, [rig.id(0)]);

    // Queued behind it while the recording runs -- the command is refused
    // while recording by construction, so this waits for the stop.
    assert!(matches!(
        rig.h.wait_recording(),
        RecordingStatus::Recording { .. }
    ));
    std::thread::sleep(Duration::from_millis(300));
    rig.h.send(Command::StopRecording);
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Idle);

    // The preempted clip is first, ahead of the clip the recording made.
    let resumed = rig.wait("clip 0 resumed", |t, id| t.running_clip() == Some(id[0]));
    assert_eq!(resumed.queued.len(), 1, "the new clip waits behind it");
    rig.wait_idle();
    assert_eq!(rig.saved_transcript(0), WORDS);
    rig.h.shutdown();
}

/// A recording that never got video makes no clip — and still has to let the
/// job it preempted resume. This is the call site that is easy to miss.
#[test]
fn a_recording_that_aborts_still_resumes_the_queue() {
    let mut rig = Rig::open(1, SLOW);
    rig.transcribe(0);
    rig.wait("clip 0 running", |t, id| t.running_clip() == Some(id[0]));

    // The camera takes two seconds to warm up, so this stop aborts.
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);
    rig.wait("clip 0 preempted", |t, _| t.running.is_none());
    rig.h.send(Command::StopRecording);
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Idle);

    rig.wait("clip 0 resumed", |t, id| t.running_clip() == Some(id[0]));
    rig.wait_idle();
    assert_eq!(rig.saved_transcript(0), WORDS);
    assert!(
        store::read(&rig.folder).unwrap().clips.len() == 1,
        "the aborted recording made no clip"
    );
    rig.h.shutdown();
}

/// A cancel stops the job **and** drops the queue behind it, and the clips go
/// back to idle rather than wearing a failure (spec S5).
#[test]
fn a_cancel_clears_the_queue_and_leaves_no_failure() {
    let mut rig = Rig::open(2, SLOW);
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait("both known", |t, id| {
        t.running_clip() == Some(id[0]) && t.queued == [id[1]]
    });

    rig.h.send(Command::CancelTranscription);
    let idle = rig.wait_idle();
    assert_eq!(idle.failed, None, "a cancel is not a failure");

    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(
            e,
            Event::Transcription {
                running: Some(_),
                ..
            }
        )),
        "nothing started after the cancel: {rest:#?}"
    );
}

/// A failure names the clip and stays until that clip is tried again — and
/// the next try clears it even before it lands.
#[test]
fn a_failure_is_reported_and_cleared_on_the_next_try() {
    let mut rig = Rig::open(1, Duration::ZERO);
    // Its recording can't be decoded any more, so the extraction fails.
    let recording = rig
        .folder
        .join(store::RECORDINGS_DIRNAME)
        .join(&rig.clips[0].recording_filename);
    std::fs::write(&recording, b"this is not a recording").unwrap();

    rig.transcribe(0);
    let failed = rig.wait("the failure", |t, _| t.failed.is_some());
    let (id, message) = failed.failed.expect("just checked");
    assert_eq!(id, rig.id(0));
    assert!(message.contains("could not read the sound"), "{message}");
    assert_eq!(rig.saved_transcript(0), "", "no words were written");

    // The retry clears it, whatever it goes on to do.
    fixtures::webm(
        &rig.folder.join(store::RECORDINGS_DIRNAME),
        &rig.clips[0].recording_filename,
        1,
        160,
        90,
        25,
        25,
    );
    rig.transcribe(0);
    rig.wait("the failure cleared", |t, _| t.failed.is_none());
    rig.wait_idle();
    assert_eq!(rig.saved_transcript(0), WORDS);
    rig.h.shutdown();
}

/// Opening a project cancels the job and drops the queue: it holds ids of
/// clips the open project has never heard of.
#[test]
fn opening_a_project_clears_the_queue() {
    let mut rig = Rig::open(2, SLOW);
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait("both known", |t, _| {
        t.running.is_some() && !t.queued.is_empty()
    });

    // A second project, in a folder of its own.
    let other = rig.folder.parent().expect("a parent").join("other");
    std::fs::create_dir(&other).unwrap();
    rig.h.send(Command::OpenProject(other.clone()));
    rig.h.wait_opened();
    let cleared = rig.wait_idle();
    assert!(cleared.queued.is_empty() && cleared.running.is_none());

    // Nothing was written into the project that was closed.
    let untouched = rig.saved_transcript(1);
    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(
            e,
            Event::Transcription {
                running: Some(_),
                ..
            }
        )),
        "nothing of the old project started: {rest:#?}"
    );
    assert_eq!(untouched, "");
}

/// Deleting a clip stops its job and takes it out of the queue: its recording
/// is moving into `.trash`, and a failure naming a clip that no longer exists
/// would sit there for the session.
#[test]
fn deleting_a_clip_stops_and_dequeues_its_job() {
    let mut rig = Rig::open(2, SLOW);
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait("both known", |t, id| {
        t.running_clip() == Some(id[0]) && t.queued == [id[1]]
    });

    // The one queued, then the one running.
    rig.h.send(Command::DeleteClip(rig.id(1)));
    rig.wait("clip 1 dequeued", |t, id| {
        t.queued.is_empty() && t.running_clip() == Some(id[0])
    });
    rig.h.send(Command::DeleteClip(rig.id(0)));
    let idle = rig.wait_idle();
    assert_eq!(idle.failed, None, "a deleted clip leaves no message");
    // The queue is told first, then the project is saved without the clip.
    let empty = rig.h.wait_changed().project.clips.is_empty();

    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(
            e,
            Event::Transcription {
                failed: Some(_),
                ..
            }
        )),
        "{rest:#?}"
    );
    assert!(empty);
}

/// Spec S7: the machine's write is not an undo step. An undo right after it
/// takes back the coach's own edit, not the transcript.
#[test]
fn the_transcript_write_is_not_undoable() {
    let mut rig = Rig::open(1, Duration::ZERO);
    rig.h.send(Command::EditClip {
        id: rig.id(0),
        edit: ClipEdit::Name("Corner".into()),
    });
    rig.h.wait_changed();
    rig.transcribe(0);
    rig.wait_idle();
    assert_eq!(rig.saved_transcript(0), WORDS);

    rig.h.send(Command::Undo);
    rig.h.wait_changed();
    let folder = rig.folder.clone();
    rig.h.shutdown();

    let saved = store::read(&folder).unwrap();
    let clip = &saved.clips[0];
    assert_eq!(clip.transcript, WORDS, "the transcript is not undone");
    assert_eq!(clip.name, "clip 0", "the coach's own edit is");
}
