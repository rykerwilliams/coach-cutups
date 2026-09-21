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

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{
    CaptureKind, Command, Event, Finish, RecordingStatus, StateFile, TranscriptionState,
};
use video_coach_core::project::{Clip, Project};
use video_coach_core::store;
use video_coach_core::undo::ClipEdit;
use video_coach_core::zoom::Zoom;
use video_coach_harness::{clip, write_project, Harness};
use video_coach_media::{fixtures, Fetch, TranscribeKind, WhisperModel};

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
    /// Where the bus's `state.json` is, so a test can read back what the bus
    /// remembered without touching the real one.
    config: PathBuf,
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

        let config = tmp.path().join("config");
        let mut h = Harness::with_transcribe(&config, capture, transcribe);
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();
        // The queue the open cleared, so a later wait can't match it.
        h.wait_transcription(
            "the opened project's empty queue",
            TranscriptionState::is_idle,
        );
        Rig {
            h,
            folder,
            config,
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
    fn wait(
        &mut self,
        what: &str,
        f: impl Fn(&TranscriptionState, &[Uuid]) -> bool,
    ) -> TranscriptionState {
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
    fn wait_idle(&mut self) -> TranscriptionState {
        self.h
            .wait_transcription("an idle queue", TranscriptionState::is_idle)
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

/// Spec S6, as the closeout settled it: stopping a recording does **not**
/// queue its clip, and asking for it afterwards transcribes it.
///
/// `AUTO_TRANSCRIBE` is off because a preempted job restarts from zero, so a
/// coach recording faster than a job finishes would never complete one
/// (`docs/superpowers/spikes/2026-09-21-whisper-throughput.md`). This test
/// fails if that const is flipped, which is the point: the decision is worth
/// re-arguing, not re-discovering.
#[test]
fn stopping_a_recording_leaves_the_transcript_to_the_coach() {
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

    // The clip's id before `Idle`, since both waits share one cursor and the
    // project change may land either side of it.
    let project = rig.h.wait_map("the recorded clip", |e| match e {
        Event::ProjectChanged(s) => (!s.project.clips.is_empty()).then(|| s.project.clone()),
        _ => None,
    });
    let id = project.clips[0].id;
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Idle);

    // No `wait_idle` here: with the const off, the queue has never published
    // anything to be idle *from*. That silence is the assertion.

    // Asking for it is what runs it.
    rig.h.send(Command::Transcribe { clip_id: id });
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

    // Two runs, not four -- which is what "does nothing" means, and what a
    // queue assertion alone would miss.
    assert_eq!(runs(rig.h.log()), [rig.id(0), rig.id(1)]);
    // And nothing was ever waiting twice. A clip is in the queue from its
    // enqueue until it starts, so the second ask for each would show up here
    // as a queue of two.
    for t in states(rig.h.log()) {
        assert!(t.queued.len() <= 1, "{t:#?}");
    }
    rig.h.shutdown();
}

/// Every transcription state in `log`, in order.
fn states(log: &[Event]) -> Vec<&TranscriptionState> {
    log.iter()
        .filter_map(|e| match e {
            Event::Transcription(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// The clips that ran, in order: each stretch of states naming the same
/// running clip is one run of it.
fn runs(log: &[Event]) -> Vec<Uuid> {
    let mut runs: Vec<Uuid> = Vec::new();
    let mut last = None;
    for t in states(log) {
        let running = t.running_clip();
        if running != last {
            runs.extend(running);
            last = running;
        }
    }
    runs
}

/// Spec S5: recording always wins. The job in flight is cancelled and its
/// clip goes back to the **front**, ahead of whatever was already waiting, so
/// it is the first thing to resume.
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
    rig.transcribe(1);
    rig.wait("clip 0 running", |t, id| t.running_clip() == Some(id[0]));

    // The record is not refused, and the transcript gives way to it.
    rig.h.send(Command::ToggleRecording {
        zoom: Zoom::IDENTITY,
    });
    assert_eq!(rig.h.wait_recording(), RecordingStatus::Starting);
    let preempted = rig.wait("clip 0 preempted", |t, _| {
        t.running.is_none() && !t.queued.is_empty()
    });
    // Ahead of clip 1, which was already waiting: this is the "front" part.
    assert_eq!(preempted.queued, [rig.id(0), rig.id(1)]);

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
    assert_eq!(resumed.queued, [rig.id(1)], "clip 1 still waits behind it");
    rig.wait_idle();
    assert_eq!(rig.saved_transcript(0), WORDS);
    assert_eq!(rig.saved_transcript(1), WORDS);
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
    assert_eq!(idle.finished, None, "a cancel is not a failure");

    let rest = rig.h.shutdown();
    assert!(runs(&rest).is_empty(), "nothing started after the cancel");
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
    let failed = rig.wait("the failure", |t, _| t.finished.is_some());
    let (id, how) = failed.finished.expect("just checked");
    assert_eq!(id, rig.id(0));
    let Finish::Failed(message) = how else {
        panic!("a failure, not {how:?}")
    };
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
    rig.wait("the failure cleared", |t, _| t.finished.is_none());
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

    // Nothing was written into the project that was closed -- not the clip
    // that was only waiting, and not the one that was **running**, whose job
    // the open had to cancel for this to hold.
    let untouched = [rig.saved_transcript(0), rig.saved_transcript(1)];
    let rest = rig.h.shutdown();
    assert!(runs(&rest).is_empty(), "nothing of the old project started");
    assert_eq!(untouched, ["", ""]);
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
    assert_eq!(idle.finished, None, "a deleted clip leaves no message");
    // The queue is told first, then the project is saved without the clip.
    let empty = rig.h.wait_changed().project.clips.is_empty();

    let rest = rig.h.shutdown();
    assert!(
        states(&rest).iter().all(|t| t.finished.is_none()),
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

/// Spec S5: a preview owns the picture and the audio sink, so a clip queued
/// while one is open waits — and starts as soon as it closes, with nobody
/// having had to remember to say so.
#[test]
fn a_preview_holds_the_queue_until_it_closes() {
    let mut rig = Rig::open(2, Duration::ZERO);
    rig.h.send(Command::OpenPreview(rig.id(0)));
    assert_eq!(rig.h.wait_preview(), Some(rig.id(0)));

    rig.transcribe(1);
    rig.wait("clip 1 waiting", |t, id| {
        t.running.is_none() && t.queued == [id[1]]
    });

    rig.h.send(Command::ClosePreview);
    assert_eq!(rig.h.wait_preview(), None);
    rig.wait("clip 1 running", |t, id| t.running_clip() == Some(id[1]));
    rig.wait_idle();
    assert_eq!(rig.saved_transcript(1), WORDS);

    // And it started *after* the close, not before it.
    let log = rig.h.log();
    let closed = log
        .iter()
        .position(|e| matches!(e, Event::Preview(None)))
        .expect("the preview closed");
    let started = log
        .iter()
        .position(|e| matches!(e, Event::Transcription(t) if t.running.is_some()))
        .expect("the job started");
    assert!(started > closed, "{log:#?}");
    rig.h.shutdown();
}

/// Replacing one preview with another tears the first down on the way, and a
/// queued clip must **not** slip in underneath the one opening: it would hold
/// the audio sink against the preview that is about to want it.
#[test]
fn replacing_a_preview_does_not_start_the_queue() {
    let mut rig = Rig::open(2, Duration::ZERO);
    rig.h.send(Command::OpenPreview(rig.id(0)));
    assert_eq!(rig.h.wait_preview(), Some(rig.id(0)));

    rig.transcribe(1);
    rig.wait("clip 1 waiting", |t, id| {
        t.running.is_none() && t.queued == [id[1]]
    });

    rig.h.send(Command::OpenPreview(rig.id(1)));
    assert_eq!(rig.h.wait_preview(), None, "the first one closed");
    assert_eq!(rig.h.wait_preview(), Some(rig.id(1)));

    let ran = runs(rig.h.log());
    // The shutdown is the barrier: every command sent before it has been
    // handled, and every event it produced delivered.
    let rest = rig.h.shutdown();
    assert!(ran.is_empty() && runs(&rest).is_empty(), "{ran:?}");
}

/// The model picker is **machine-wide** (spec S3's model path): it describes
/// how fast this laptop is, not the match, so it is remembered in
/// `state.json` beside the last project — never in `project.json`, where a
/// new field is a format change every existing project would fail
/// `store::read`'s exact-version guard on.
#[test]
fn the_chosen_model_is_remembered_for_the_machine() {
    let rig = Rig::open(1, Duration::ZERO);
    let state = StateFile::in_config_dir(&rig.config);
    assert_eq!(
        state.whisper_model(),
        WhisperModel::Small,
        "the default, until the coach picks otherwise"
    );

    rig.h.send(Command::SetTranscribeModel(WhisperModel::Base));
    // Handles every command sent before it, so this is the write's barrier.
    rig.h.shutdown();
    assert_eq!(state.whisper_model(), WhisperModel::Base);
    // And the last project survived the write, which rewrites the file whole.
    assert_eq!(state.last_project().as_deref(), Some(rig.folder.as_path()));
}

/// **Switching mid-queue never touches the job in flight.** Cancelling a
/// whisper run costs about twelve seconds of CPU for nothing — it reads its
/// abort flag once per encode and once per decode pass — and the coach asked
/// for a different model *next*, not for this run to be thrown away.
#[test]
fn changing_the_model_leaves_the_running_job_alone() {
    let mut rig = Rig::open(2, SLOW);
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait("both known", |t, id| {
        t.running_clip() == Some(id[0]) && t.queued == [id[1]]
    });

    rig.h.send(Command::SetTranscribeModel(WhisperModel::Base));

    rig.wait("clip 1 running", |t, id| t.running_clip() == Some(id[1]));
    rig.wait_idle();
    // A cancel would have left clip 0 idle with nothing written, and a
    // preemption would have run it twice.
    assert_eq!(rig.saved_transcript(0), WORDS);
    assert_eq!(rig.saved_transcript(1), WORDS);
    assert_eq!(runs(rig.h.log()), [rig.id(0), rig.id(1)]);
    rig.h.shutdown();
}

/// ... and the job **started after** the change runs the model just picked.
///
/// On the whisper transcriber, with no model file anywhere near it: a model
/// that isn't downloaded is the ordinary `Failed`, and that message names the
/// exact file it looked for. Which is the point — the failure has to name the
/// model the coach has just chosen, and offer the URL for it.
///
/// **And a job with no `fetch` never downloads** (Phase 11 spec S3), not
/// before the switch and not after it: the file is named as ours are, so a
/// rule that read permission off the path — or a switch that handed out a
/// `fetch` the job never had — would pull 148 MB from Hugging Face on CI.
#[test]
fn a_new_job_runs_the_model_just_picked() {
    let models = tempfile::tempdir().unwrap();
    let mut rig = Rig::open_with(
        1,
        SLOW_CAMERA,
        TranscribeKind::Whisper {
            model: models.path().join(WhisperModel::Small.file_name()),
            fetch: None,
        },
    );

    rig.transcribe(0);
    let first = rig.wait("the first failure", |t, _| t.finished.is_some());
    assert!(
        failure(&first).contains(WhisperModel::Small.file_name()),
        "{:?}",
        first.finished
    );

    rig.h.send(Command::SetTranscribeModel(WhisperModel::Base));
    rig.transcribe(0);
    let second = rig.wait("the second failure", |t, _| {
        matches!(&t.finished, Some((_, Finish::Failed(m)))
            if m.contains(WhisperModel::Base.file_name()))
    });
    let message = failure(&second);
    // The other model, beside the first: the directory is the one the app
    // resolved at start-up, and only the file name follows the picker.
    let wanted = models.path().join(WhisperModel::Base.file_name());
    assert!(message.contains(&wanted.display().to_string()), "{message}");
    // And it is one of ours, so it comes with somewhere to get it.
    assert!(message.contains("https://huggingface.co/"), "{message}");
    assert_eq!(rig.saved_transcript(0), "", "no words were written");

    let mut log = rig.h.log().to_vec();
    log.extend(rig.h.shutdown());
    assert!(
        states(&log).iter().all(|t| t.downloading.is_none()),
        "a job with no fetch downloaded"
    );
    let left: Vec<_> = std::fs::read_dir(models.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        left.is_empty(),
        "the models directory was written to: {left:?}"
    );
}

/// An HTTP server answering every request with `response`, `delay` after
/// reading it, and the URL of a model on it. **No test touches Hugging
/// Face.**
fn serve(delay: Duration, response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/ggml-small.en.bin",
        listener.local_addr().unwrap()
    );
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let response = response.clone();
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut byte = [0; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => request.push(byte[0]),
                        _ => return,
                    }
                }
                std::thread::sleep(delay);
                let _ = stream.write_all(&response);
            });
        }
    });
    url
}

/// A whisper transcriber for the model at `<models>/ggml-small.en.bin`,
/// which isn't there, allowed to fetch it from `url`.
fn fetching(models: &Path, url: String, sha256: &str, bytes: u64) -> TranscribeKind {
    TranscribeKind::Whisper {
        model: models.join(WhisperModel::Small.file_name()),
        fetch: Some(Fetch {
            url,
            sha256: sha256.into(),
            bytes,
        }),
    }
}

/// **A failed download drops the queue behind it** (Phase 11 spec S3): the
/// next clip would fail the same way — offline at the field, or another
/// 488 MB after a bad hash — and so would every one after it. The clip that
/// failed says why, and nothing else runs.
#[test]
fn a_failed_download_drops_the_queue() {
    let models = tempfile::tempdir().unwrap();
    // Late enough that the second clip is certainly queued behind the first.
    let url = serve(
        Duration::from_millis(1_000),
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
    );
    let mut rig = Rig::open_with(
        2,
        SLOW_CAMERA,
        fetching(models.path(), url, &"0".repeat(64), 1_000),
    );
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait("clip 0 downloading, clip 1 waiting", |t, id| {
        t.running_clip() == Some(id[0]) && t.downloading.is_some() && t.queued == [id[1]]
    });

    let after = rig.wait("the failure", |t, _| t.finished.is_some());
    assert_eq!(after.finished.as_ref().map(|(c, _)| *c), Some(rig.id(0)));
    assert!(failure(&after).contains("could not download"), "{after:?}");
    assert!(
        after.queued.is_empty() && after.running.is_none() && after.downloading.is_none(),
        "the queue outlived the download: {after:?}"
    );
    let first = rig.id(0);
    let mut log = rig.h.log().to_vec();
    log.extend(rig.h.shutdown());
    assert_eq!(runs(&log), [first], "clip 1 ran");
}

/// A download that **succeeds** hands over to whisper: the `downloading`
/// state ends, and a failure after it — here, whisper refusing a "model"
/// that is a sentence — is an ordinary failure that leaves the queue alone.
/// The next clip then finds the file already there, and downloads nothing.
#[test]
fn a_finished_download_hands_the_job_to_whisper() {
    const MODEL: &[u8] = b"this is not a speech model";
    const MODEL_SHA256: &str = "50deac7ae7a3e51baccfb5e55164061e0eeaf698929724eac6b46d9d076cd9e2";
    let models = tempfile::tempdir().unwrap();
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", MODEL.len()).into_bytes();
    response.extend_from_slice(MODEL);
    let url = serve(Duration::from_millis(1_000), response);
    let mut rig = Rig::open_with(
        2,
        SLOW_CAMERA,
        fetching(models.path(), url, MODEL_SHA256, MODEL.len() as u64),
    );
    rig.transcribe(0);
    rig.transcribe(1);
    rig.wait("clip 0 downloading, clip 1 waiting", |t, id| {
        t.running_clip() == Some(id[0]) && t.downloading.is_some() && t.queued == [id[1]]
    });
    rig.wait("the download over, clip 0 still running", |t, id| {
        t.running_clip() == Some(id[0]) && t.downloading.is_none()
    });
    let last = rig.wait(
        "clip 1's failure",
        |t, id| matches!(t.finished, Some((c, _)) if c == id[1]),
    );
    assert!(failure(&last).contains("could not load"), "{last:?}");
    assert_eq!(
        std::fs::read(models.path().join(WhisperModel::Small.file_name())).unwrap(),
        MODEL
    );

    let ids = [rig.id(0), rig.id(1)];
    let mut log = rig.h.log().to_vec();
    log.extend(rig.h.shutdown());
    assert_eq!(runs(&log), ids);
    assert!(
        states(&log)
            .iter()
            .all(|t| t.downloading.is_none() || t.running_clip() == Some(ids[0])),
        "clip 1 downloaded a model that was already there"
    );
}

/// The message of the last failure.
fn failure(t: &TranscriptionState) -> String {
    match &t.finished {
        Some((_, Finish::Failed(message))) => message.clone(),
        other => panic!("a failure, not {other:?}"),
    }
}
