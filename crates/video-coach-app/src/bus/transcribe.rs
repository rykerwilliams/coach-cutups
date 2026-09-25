//! Transcription (Phase 10 spec S5–S7): the coach's words, one clip at a
//! time, on a queue the bus thread owns.
//!
//! **One job in flight, FIFO behind it, and enqueueing is idempotent** —
//! against the queue *and* against the clip running, which is not in the
//! queue. A clip that is waiting says so: `Queued` is derived from the queue
//! rather than stored anywhere.
//!
//! **Recording always wins** (spec S5). A transcript never refuses a
//! recording: starting one cancels the job in flight and puts its clip back
//! at the **front** of the queue, and [`Bus::run_next_if_idle`] refuses to
//! start while a recording, an export or a preview is going. Nothing here
//! has to remember to resume the queue afterwards: `Bus::run` calls
//! [`Bus::run_next_if_idle`] at the bottom of every turn, so the queue picks
//! up on the first input after the machine is free.
//!
//! **A cancel is never a failure.** The clip goes back to idle, and a job
//! that finished before the cancel reached it keeps its words — the same
//! trade export makes when a cancel loses the race to a written file.
//!
//! **The job may download its model first** (Phase 11 spec S3), when the
//! model is absent and [`whisper`] gave it somewhere to fetch it from. That
//! is its own [`Stage`] on screen, and a download that fails drops the queue
//! behind it: otherwise every waiting clip tries again in turn — each one
//! offline at the field, or each one another 488 MB after a bad hash.
//!
//! The transcriber's messages arrive as their own input, tagged with the
//! generation of the job that sent them and the clip they are about: a
//! cancelled job's `Finished` can still be in the channel when the next one
//! starts, and taking it for the new job's would leave two running at once.

use std::ffi::OsString;
use std::path::PathBuf;

use uuid::Uuid;
use video_coach_core::store::RECORDINGS_DIRNAME;
use video_coach_core::undo::ClipEdit;
use video_coach_media::{
    TranscribeError, TranscribeKind, TranscribeMessage, Transcriber, WhisperModel,
};

use super::state::{cache_dir, APP_DIR};
use super::{Bus, Event, Input};

/// Whether stopping a recording queues its clip (spec S6).
///
/// A `const`, not a preference: `Preferences` lives in `project.json`, so a
/// field there is a format change.
///
/// **Off**, decided by the closeout measurement
/// (`docs/superpowers/spikes/2026-09-21-whisper-throughput.md`). `small.en`
/// runs at 0.73x realtime here, but the throughput is the weaker half of the
/// argument: a preempted job restarts from zero, so while the coach records
/// faster than a job finishes, *no job ever completes*. Six takes back to back
/// would end the session with a full queue, no transcripts, and 8 whisper
/// threads that spent it competing with the capture pipeline. Transcription
/// happens when the coach asks for it.
const AUTO_TRANSCRIBE: bool = false;

/// Under the cache directory, beside nothing else: downloaded weights are a
/// cache, not configuration.
const MODELS_DIRNAME: &str = "models";

/// Points the app at a model somewhere else — which is also what makes the
/// whole path testable, with a small model locally and none at all on CI.
const MODEL_ENV: &str = "COACH_CUTS_WHISPER_MODEL";

/// The transcriber that runs `model` (Phase 10 spec S3, Phase 11 S3):
/// `$XDG_CACHE_HOME/coach-cuts/models/<its file name>` (with the `~/.cache`
/// fallback), **downloaded there on first use** — or the file
/// `$COACH_CUTS_WHISPER_MODEL` names, which is never fetched.
///
/// **This is the one place that decides whether a job may download** — see
/// [`TranscribeKind::Whisper`] for why the path can't.
pub fn whisper(model: WhisperModel) -> TranscribeKind {
    whisper_kind(
        std::env::var_os(MODEL_ENV),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        model,
    )
}

/// The model `$COACH_CUTS_WHISPER_MODEL` names, when it names one.
///
/// **The picker says so rather than lying:** with this set, the coach's
/// choice is remembered but not what runs, so the control is disabled and
/// shows what the variable points at instead.
pub fn whisper_model_override() -> Option<PathBuf> {
    std::env::var_os(MODEL_ENV)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// [`whisper`] with the environment passed in, as
/// [`config_dir`](super::state) takes it: the rule is worth a test, and
/// `set_var` in one is a race with every other test in the binary.
fn whisper_kind(
    env: Option<OsString>,
    xdg: Option<OsString>,
    home: Option<OsString>,
    model: WhisperModel,
) -> TranscribeKind {
    if let Some(path) = env.filter(|p| !p.is_empty()) {
        return TranscribeKind::Whisper {
            model: PathBuf::from(path),
            fetch: None,
        };
    }
    match cache_dir(xdg, home) {
        Some(dir) => TranscribeKind::Whisper {
            model: dir
                .join(APP_DIR)
                .join(MODELS_DIRNAME)
                .join(model.file_name()),
            fetch: Some(model.fetch()),
        },
        // No `$HOME` and no `$XDG_CACHE_HOME`: a bare file name is still a
        // path for the failure to name, which is better than no failure at
        // all — and nowhere to download 488 MB into, since it would land in
        // whatever the working directory is.
        None => TranscribeKind::Whisper {
            model: PathBuf::from(model.file_name()),
            fetch: None,
        },
    }
}

/// `kind` pointed at `model` instead, when the coach picks it.
///
/// **Only a job that may download has a path of ours** (see [`whisper`]), so
/// `fetch` is what says the path may move — never its file name, which under
/// `$COACH_CUTS_WHISPER_MODEL` can be ours in a directory that is the
/// coach's. **And a `fetch` follows the model only when it is one of ours:**
/// anything else is a test's local server, which switching models must never
/// turn into a download from Hugging Face.
fn retarget(kind: &mut TranscribeKind, model: WhisperModel) {
    if let TranscribeKind::Whisper {
        model: path,
        fetch: Some(fetch),
    } = kind
    {
        path.set_file_name(model.file_name());
        if WhisperModel::ALL.iter().any(|m| m.fetch() == *fetch) {
            *fetch = model.fetch();
        }
    }
}

/// How a job ended, when the ending is something the inspector has to say
/// out loud. A run that wrote words says it with the words, and a cancel says
/// nothing at all (spec S5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The run wrote nothing. whisper returns no segments at all over
    /// silence, and `""` is *also* how a clip says it was never transcribed
    /// (spec S4), so without this the coach presses Transcribe, waits, and
    /// sees no change whatsoever.
    Silent,
    /// The run failed, with the message to show.
    Failed(String),
}

/// Where the running job is, and how far along, in whole percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Downloading its model first (Phase 11 spec S3). Its own stage, so a
    /// screen can't read "Transcribing… 63%" while 488 MB arrives.
    Downloading(u8),
    /// whisper's percent: a floor, which on a short clip never leaves 0.
    Transcribing(u8),
}

/// The whole transcription state (spec S5), as [`Event::Transcription`]
/// carries it: the clips waiting in order, the one running and its stage,
/// and how the last run ended if it ended with something to say. A clip in
/// none of the three is idle with nothing to report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptionState {
    pub queued: Vec<Uuid>,
    pub running: Option<(Uuid, Stage)>,
    pub finished: Option<(Uuid, Finish)>,
}

impl TranscriptionState {
    /// Nothing running and nothing waiting.
    pub fn is_idle(&self) -> bool {
        self.queued.is_empty() && self.running.is_none()
    }

    /// The clip running, whatever stage it is at.
    pub fn running_clip(&self) -> Option<Uuid> {
        self.running.map(|(id, _)| id)
    }
}

/// The job in flight: the clip it is about, the thread doing it, and the
/// stage it last reported.
pub(super) struct Active {
    clip: Uuid,
    /// Dropping it cancels the run and returns at once — see
    /// [`Bus::stop_transcription`].
    transcriber: Transcriber,
    /// A failure while this is `Downloading` is a failed download.
    stage: Stage,
}

impl Bus {
    /// [`Command::Transcribe`](super::Command::Transcribe): queues clip `id`.
    ///
    /// Idempotent against the queue and the clip running, so the inspector's
    /// button and the automatic enqueue can't stack up two runs of one clip.
    pub(super) fn transcribe(&mut self, id: Uuid) {
        if self.transcribing.as_ref().is_some_and(|a| a.clip == id)
            || self.transcribe_queue.contains(&id)
        {
            return;
        }
        if self.recording_path(id).is_none() {
            return eprintln!("bus: Transcribe on a clip that isn't there: {id}");
        }
        // A retry clears what the last one left: it is otherwise the only
        // thing the inspector says about this clip, including after the retry
        // succeeds.
        self.clear_transcribe_finished(id);
        self.transcribe_queue.push_back(id);
        self.publish_transcription();
    }

    /// [`Command::CancelTranscription`](super::Command::CancelTranscription):
    /// stops the job in flight **and drops the queue behind it** — "cancel"
    /// is otherwise ambiguous with a queue present.
    pub(super) fn cancel_transcription(&mut self) {
        self.transcribe_queue.clear();
        self.stop_transcription();
        self.publish_transcription();
    }

    /// A recording has just started (or the model a download was fetching was
    /// just turned down), so the transcript in flight gives way to it and its
    /// clip goes back to the **front** of the queue (spec S5).
    ///
    /// Called the moment the recording exists, never from the top of
    /// `toggle_recording`: that bails at five points, and a refused record
    /// must not kill a transcript for nothing.
    pub(super) fn preempt_transcription(&mut self) {
        let Some(clip) = self.transcribing.as_ref().map(|a| a.clip) else {
            return;
        };
        self.stop_transcription();
        self.transcribe_queue.push_front(clip);
        self.publish_transcription();
    }

    /// Clip `id` is going into the trash (spec S5): its job stops and its
    /// place in the queue goes with it. Otherwise the run reads a recording
    /// that is now in `.trash/` and leaves a failure naming a clip that no
    /// longer exists.
    pub(super) fn cancel_transcription_of(&mut self, id: Uuid) {
        if self.transcribing.as_ref().is_some_and(|a| a.clip == id) {
            self.stop_transcription();
        }
        self.transcribe_queue.retain(|&q| q != id);
        self.clear_transcribe_finished(id);
        self.publish_transcription();
    }

    /// A project is being opened: nothing of the last one's queue survives,
    /// just as nothing of its undo history does. A job left running would
    /// write its words into a project that is no longer open.
    pub(super) fn reset_transcription(&mut self) {
        self.transcribe_queue.clear();
        self.stop_transcription();
        self.transcribe_finished = None;
        self.publish_transcription();
    }

    /// [`Command::SetTranscribeModel`](super::Command::SetTranscribeModel):
    /// the coach picked a model. It is remembered for every project on this
    /// machine (`state.json`, never `project.json`), and everything still
    /// queued picks it up.
    ///
    /// **A job transcribing keeps the model it started with.** Cancelling it
    /// would cost about twelve seconds of CPU for nothing — whisper reads its
    /// abort flag once per encode and once per decode pass (see
    /// [`Bus::stop_transcription`]) — and the coach asked for a different
    /// model *next*, not for this one to be thrown away.
    ///
    /// **A job downloading is preempted**, as a recording preempts it, and
    /// restarts on the new model. A download stops within a tenth of a
    /// second, and left alone it would carry on fetching up to 488 MB of a
    /// model the coach has just turned down — on a slow link, for minutes.
    pub(super) fn set_transcribe_model(&mut self, model: WhisperModel) {
        if self.transcribe_model == model {
            return;
        }
        self.transcribe_model = model;
        self.files.set_whisper_model(model);
        retarget(&mut self.transcribe, model);
        if self
            .transcribing
            .as_ref()
            .is_some_and(|a| matches!(a.stage, Stage::Downloading(_)))
        {
            self.preempt_transcription();
        }
    }

    /// A recording just produced clip `id` (spec S6).
    pub(super) fn transcribe_after_recording(&mut self, id: Uuid) {
        if AUTO_TRANSCRIBE {
            self.transcribe(id);
        }
    }

    /// Starts the queue's first job unless something is in the way.
    ///
    /// **[`Bus::run`](super::Bus::run) calls this at the bottom of every
    /// turn**, beside the deadlines and the position, so nothing else has to
    /// remember to. The alternative was a call at each of the eight places a
    /// recording, an export or a preview ends and each place the queue
    /// changes — a discipline that stalls the queue silently when one is
    /// missed, and started a job *underneath* an opening preview when one was
    /// wrong.
    pub(super) fn run_next_if_idle(&mut self) {
        if self.transcribing.is_some() {
            return;
        }
        // One condition for all three, rather than a rule per pair: the
        // recording owns the machine (spec S5), the export owns the encoder,
        // and the preview owns the picture and the audio sink.
        if self.recording.is_some() || self.export.is_some() || self.preview.is_some() {
            return;
        }
        let mut changed = false;
        while let Some(id) = self.transcribe_queue.pop_front() {
            changed = true;
            // A clip that went away without passing through `trash_clip`.
            let Some(recording) = self.recording_path(id) else {
                continue;
            };
            self.transcribe_generation += 1;
            let (tx, generation) = (self.tx.clone(), self.transcribe_generation);
            let transcriber = Transcriber::start(recording, self.transcribe.clone(), move |msg| {
                // Fails only once the bus thread has exited.
                let _ = tx.send(Input::Transcription(generation, id, msg));
            });
            self.transcribing = Some(Active {
                clip: id,
                transcriber,
                stage: Stage::Transcribing(0),
            });
            break;
        }
        // Only when it moved something: this runs after every input, and an
        // empty queue must not emit an event per GStreamer message.
        if changed {
            self.publish_transcription();
        }
    }

    pub(super) fn transcription_message(
        &mut self,
        generation: u64,
        clip: Uuid,
        msg: TranscribeMessage,
    ) {
        match msg {
            TranscribeMessage::Downloading(percent) => {
                self.advance(generation, Stage::Downloading(percent))
            }
            TranscribeMessage::Progress(percent) => {
                self.advance(generation, Stage::Transcribing(percent))
            }
            TranscribeMessage::Finished(result) => {
                // The words are kept whatever became of the queue meanwhile:
                // a cancel too late to stop the job doesn't throw its work
                // away, and a clip deleted since simply has nowhere to put
                // them. A stale *outcome*, though, belongs to a job nobody is
                // waiting for, and saying so would be noise.
                let current = generation == self.transcribe_generation;
                match result {
                    Ok(text) => {
                        let silent = text.is_empty();
                        self.write_transcript(clip, text);
                        if silent && current {
                            self.transcribe_finished = Some((clip, Finish::Silent));
                        }
                    }
                    // Abandoned, not answered: the clip goes back to idle
                    // with nothing to say about it (spec S5).
                    Err(TranscribeError::Cancelled) => self.clear_transcribe_finished(clip),
                    Err(TranscribeError::Failed(e)) => {
                        eprintln!("bus: transcribing {clip} failed: {e}");
                        if current {
                            // A failed download fails every clip behind it
                            // the same way — offline, or another 488 MB per
                            // clip after a bad hash — so they stop waiting.
                            // The clip that failed says why; pressing
                            // Transcribe again is the retry.
                            if self
                                .transcribing
                                .as_ref()
                                .is_some_and(|a| matches!(a.stage, Stage::Downloading(_)))
                            {
                                self.transcribe_queue.clear();
                            }
                            self.transcribe_finished = Some((clip, Finish::Failed(e)));
                        }
                    }
                }
                if current {
                    self.transcribing = None;
                }
                self.publish_transcription();
            }
        }
    }

    /// The job in flight has reached `stage`: published when that is news,
    /// including the step from a download's 100% to whisper's 0%.
    fn advance(&mut self, generation: u64, stage: Stage) {
        if generation != self.transcribe_generation {
            return;
        }
        let Some(active) = &mut self.transcribing else {
            return;
        };
        if active.stage == stage {
            return;
        }
        active.stage = stage;
        self.publish_transcription();
    }

    /// The machine's write (spec S7): apply, save and publish, and **never**
    /// push undo — [`UndoController::push`](video_coach_core::undo::UndoController::push)
    /// clears the redo stack, so a transcript landing mid-session would
    /// silently destroy whatever the coach still had to redo.
    fn write_transcript(&mut self, clip: Uuid, text: String) {
        // Work already done is never redone: a job the recording preempted
        // may still have finished, and its clip is back on the queue.
        self.transcribe_queue.retain(|&q| q != clip);
        self.clear_transcribe_finished(clip);

        let Some(open) = &mut self.open else {
            return;
        };
        match open
            .project
            .apply_edit(clip, ClipEdit::Transcript(text.clone()))
        {
            // Deleted while its job ran, or belonging to a project that has
            // since been closed.
            None => eprintln!("bus: a transcript arrived for a clip that isn't there: {clip}"),
            // Unchanged: a re-run that agreed with itself, or the empty
            // transcript of a clip with nothing said over it. `""` is how a
            // clip says it has never been transcribed (spec S4), so this is
            // not a change to save either way.
            Some(before) if before == ClipEdit::Transcript(text) => {}
            Some(_) => self.project_changed(),
        }
    }

    /// Cancels the job in flight, if any, and **does not wait for it**. The
    /// generation goes up with it, so nothing it had already queued — nor
    /// anything it sends on its way out — is taken for the next job's.
    ///
    /// **Waiting here would freeze the app.** whisper.cpp reads its abort
    /// callback once per encode and once per decode pass, not per graph node:
    /// a cancelled `small.en` run was measured taking 12.4 s to return. Every
    /// caller of this is on the bus thread, so a join would mean Stop
    /// Recording doing nothing for twelve seconds, no recorder message
    /// handled and no deadline dispatched in the meantime, and a quit holding
    /// the UI's GL teardown for the same twelve seconds. The abandoned run
    /// writes nothing: [`Bus::write_transcript`] already tolerates a job
    /// nobody is waiting for, and a stale `Finished` is discarded by
    /// generation.
    fn stop_transcription(&mut self) {
        let Some(active) = self.transcribing.take() else {
            return;
        };
        self.transcribe_generation += 1;
        drop(active.transcriber);
    }

    /// Drops the last outcome if it is this clip's. One slot, in memory: a
    /// relaunch starts every clip idle, and a failed transcript is cheap to
    /// retry (spec S5).
    fn clear_transcribe_finished(&mut self, id: Uuid) {
        if self
            .transcribe_finished
            .as_ref()
            .is_some_and(|(c, _)| *c == id)
        {
            self.transcribe_finished = None;
        }
    }

    /// Where clip `id`'s commentary recording is, or `None` if there is no
    /// such clip in the open project.
    fn recording_path(&self, id: Uuid) -> Option<PathBuf> {
        let open = self.open.as_ref()?;
        let clip = open.project.clips.iter().find(|c| c.id == id)?;
        Some(
            open.folder
                .join(RECORDINGS_DIRNAME)
                .join(&clip.recording_filename),
        )
    }

    /// The whole state, every time (spec S5), so no view is left holding
    /// something the bus has moved past.
    fn publish_transcription(&self) {
        self.emit(Event::Transcription(TranscriptionState {
            queued: self.transcribe_queue.iter().copied().collect(),
            running: self.transcribing.as_ref().map(|a| (a.clip, a.stage)),
            finished: self.transcribe_finished.clone(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whisper_at(model: &str, fetch: Option<WhisperModel>) -> TranscribeKind {
        TranscribeKind::Whisper {
            model: PathBuf::from(model),
            fetch: fetch.map(WhisperModel::fetch),
        }
    }

    /// The chosen model's file under the cache directory, by the same XDG
    /// rule everything else in `state.rs` follows — and **ours to download
    /// there**, from that model's own URL.
    #[test]
    fn the_chosen_model_is_looked_for_under_the_cache_directory() {
        for (model, file) in [
            (WhisperModel::Base, "ggml-base.en.bin"),
            (WhisperModel::Small, "ggml-small.en.bin"),
        ] {
            assert_eq!(
                whisper_kind(None, Some("/x/cache".into()), Some("/home/u".into()), model),
                whisper_at(&format!("/x/cache/coach-cuts/models/{file}"), Some(model)),
            );
            assert_eq!(
                whisper_kind(None, None, Some("/home/u".into()), model),
                whisper_at(
                    &format!("/home/u/.cache/coach-cuts/models/{file}"),
                    Some(model)
                ),
            );
            // No `$HOME` and no `$XDG_CACHE_HOME`: a bare name is still a
            // path for the failure to name — and not somewhere to download
            // 488 MB into.
            assert_eq!(
                whisper_kind(None, None, None, model),
                whisper_at(file, None),
                "{model:?}"
            );
        }
    }

    /// **Switching models moves the path only when it is ours** — when the
    /// job may download — and never a file `$COACH_CUTS_WHISPER_MODEL` names,
    /// even one called what one of ours is: that is the coach's file, and
    /// its sibling was never downloaded.
    #[test]
    fn switching_models_moves_only_a_path_that_is_ours() {
        let mut kind = whisper_at("/opt/models/ggml-small.en.bin", None);
        retarget(&mut kind, WhisperModel::Base);
        assert_eq!(kind, whisper_at("/opt/models/ggml-small.en.bin", None));

        let mut kind = whisper_at("/c/models/ggml-small.en.bin", Some(WhisperModel::Small));
        retarget(&mut kind, WhisperModel::Base);
        assert_eq!(
            kind,
            whisper_at("/c/models/ggml-base.en.bin", Some(WhisperModel::Base))
        );
    }

    /// **And never turns a local fetch into a network one.** A test pointing
    /// the transcriber at its own server keeps that server across a switch;
    /// only our own fetch follows the model to Hugging Face.
    #[test]
    fn switching_models_never_sends_a_local_fetch_to_the_network() {
        let local = video_coach_media::Fetch {
            url: "http://127.0.0.1:1/ggml-small.en.bin".into(),
            sha256: "0".repeat(64),
            bytes: 1_000,
        };
        let mut kind = TranscribeKind::Whisper {
            model: PathBuf::from("/t/ggml-small.en.bin"),
            fetch: Some(local.clone()),
        };
        retarget(&mut kind, WhisperModel::Base);
        assert_eq!(
            kind,
            TranscribeKind::Whisper {
                model: PathBuf::from("/t/ggml-base.en.bin"),
                fetch: Some(local),
            }
        );
    }

    /// **`$COACH_CUTS_WHISPER_MODEL` beats the choice, every time** — it is
    /// how the `#[ignore]`d whisper tests find a model, and how a coach runs
    /// one we don't ship — **and is never downloaded to**, even when the
    /// file it names is called what one of ours is. An empty value is not a
    /// path, and is ignored.
    #[test]
    fn the_environment_overrides_whatever_was_picked() {
        for model in WhisperModel::ALL {
            for named in ["/opt/models/my-tuned.bin", "/opt/models/ggml-small.en.bin"] {
                assert_eq!(
                    whisper_kind(
                        Some(named.into()),
                        Some("/x/cache".into()),
                        Some("/home/u".into()),
                        model,
                    ),
                    whisper_at(named, None),
                    "{model:?}"
                );
            }
            assert_eq!(
                whisper_kind(Some("".into()), None, Some("/home/u".into()), model),
                whisper_at(
                    &format!("/home/u/.cache/coach-cuts/models/{}", model.file_name()),
                    Some(model)
                ),
                "{model:?}"
            );
        }
    }
}
