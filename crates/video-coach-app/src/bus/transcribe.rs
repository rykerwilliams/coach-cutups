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
//! The transcriber's messages arrive as their own input, tagged with the
//! generation of the job that sent them and the clip they are about: a
//! cancelled job's `Finished` can still be in the channel when the next one
//! starts, and taking it for the new job's would leave two running at once.

use std::path::PathBuf;

use uuid::Uuid;
use video_coach_core::store::RECORDINGS_DIRNAME;
use video_coach_core::undo::ClipEdit;
use video_coach_media::{TranscribeError, TranscribeMessage, Transcriber};

use super::state::{cache_dir, APP_DIR};
use super::{Bus, Event, Input};

/// Whether stopping a recording queues its clip (spec S6).
///
/// A `const`, not a preference: `Preferences` lives in `project.json`, so a
/// field there is a format change, and the closeout flips this literal once
/// there is a throughput number to flip it with.
const AUTO_TRANSCRIBE: bool = true;

/// Under the cache directory, beside nothing else: a 466 MB download is a
/// cache, not configuration, and nothing in this phase puts it there.
const MODELS_DIRNAME: &str = "models";

/// The model this phase looks for (spec S3). Never a quantization suffix:
/// tiny/base/small ship `q5_1` and medium/large `q5_0`.
const MODEL_FILE: &str = "ggml-small.en.bin";

/// Points the app at a model somewhere else — which is also what makes the
/// whole path testable, with a small model locally and none at all on CI.
const MODEL_ENV: &str = "COACH_CUTS_WHISPER_MODEL";

/// Where the whisper model is read from (spec S3): `$COACH_CUTS_WHISPER_MODEL`
/// if set, else `$XDG_CACHE_HOME/coach-cuts/models/ggml-small.en.bin` (with
/// the `~/.cache` fallback).
///
/// **Found, never fetched.** Downloading it is Phase 11's, with the bundling
/// decision; a model that isn't there fails the job with a message naming
/// this path.
pub fn whisper_model_path() -> PathBuf {
    if let Some(path) = std::env::var_os(MODEL_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    cache_dir(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
        .map(|dir| dir.join(APP_DIR).join(MODELS_DIRNAME).join(MODEL_FILE))
        // No `$HOME` and no `$XDG_CACHE_HOME`: a bare file name is still a path
        // for the failure to name, which is better than no failure at all.
        .unwrap_or_else(|| PathBuf::from(MODEL_FILE))
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

/// The whole transcription state (spec S5), as [`Event::Transcription`]
/// carries it: the clips waiting in order, the one running with its percent,
/// and how the last run ended if it ended with something to say. A clip in
/// none of the three is idle with nothing to report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptionState {
    pub queued: Vec<Uuid>,
    pub running: Option<(Uuid, u8)>,
    pub finished: Option<(Uuid, Finish)>,
}

impl TranscriptionState {
    /// Nothing running and nothing waiting.
    pub fn is_idle(&self) -> bool {
        self.queued.is_empty() && self.running.is_none()
    }

    /// The clip running, whatever percent it reports.
    pub fn running_clip(&self) -> Option<Uuid> {
        self.running.map(|(id, _)| id)
    }
}

/// The job in flight: the clip it is about, the thread doing it, and the
/// percent it last reported.
pub(super) struct Active {
    clip: Uuid,
    /// Dropping it cancels and joins.
    transcriber: Transcriber,
    percent: u8,
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

    /// A recording has just started, so the transcript in flight gives way to
    /// it and its clip goes back to the **front** of the queue (spec S5).
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
                percent: 0,
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
            TranscribeMessage::Progress(percent) => {
                if generation != self.transcribe_generation {
                    return;
                }
                let Some(active) = &mut self.transcribing else {
                    return;
                };
                if active.percent == percent {
                    return;
                }
                active.percent = percent;
                self.publish_transcription();
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

    /// Cancels the job in flight, if any, and joins its thread. The
    /// generation goes up with it, so nothing it had already queued is taken
    /// for the next job's.
    fn stop_transcription(&mut self) {
        let Some(active) = self.transcribing.take() else {
            return;
        };
        self.transcribe_generation += 1;
        // Cancels and joins. It notices within about 10 ms.
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

    /// The whole state, every time (spec S5): three fields, so no view is
    /// left holding something the bus has moved past.
    fn publish_transcription(&self) {
        self.emit(Event::Transcription(TranscriptionState {
            queued: self.transcribe_queue.iter().copied().collect(),
            running: self.transcribing.as_ref().map(|a| (a.clip, a.percent)),
            finished: self.transcribe_finished.clone(),
        }));
    }
}
