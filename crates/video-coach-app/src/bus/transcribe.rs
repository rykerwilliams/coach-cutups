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
//! start while a recording, an export or a preview is going. So every place
//! one of those three ends has to call it, or a preempted transcript sits
//! there until the next enqueue.
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
        // A retry clears the message the last one left: it is otherwise the
        // only thing the inspector says about this clip, including after the
        // retry succeeds.
        self.clear_transcribe_failure(id);
        self.transcribe_queue.push_back(id);
        if !self.run_next_if_idle() {
            self.publish_transcription();
        }
    }

    /// [`Command::CancelTranscription`](super::Command::CancelTranscription):
    /// stops the job in flight **and drops the queue behind it** — "cancel"
    /// is otherwise ambiguous with a queue present.
    pub(super) fn cancel_transcription(&mut self) {
        self.transcribe_queue.clear();
        self.stop_transcription();
        if !self.run_next_if_idle() {
            self.publish_transcription();
        }
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
        let running = self.transcribing.as_ref().is_some_and(|a| a.clip == id);
        let queued = self.transcribe_queue.contains(&id);
        let failed = self.transcribe_failure_of(id);
        if !(running || queued || failed) {
            return;
        }
        if running {
            self.stop_transcription();
        }
        self.transcribe_queue.retain(|&q| q != id);
        self.clear_transcribe_failure(id);
        if !self.run_next_if_idle() {
            self.publish_transcription();
        }
    }

    /// A project is being opened: nothing of the last one's queue survives,
    /// just as nothing of its undo history does. A job left running would
    /// write its words into a project that is no longer open.
    pub(super) fn reset_transcription(&mut self) {
        self.transcribe_queue.clear();
        self.stop_transcription();
        self.transcribe_failed = None;
        self.publish_transcription();
    }

    /// A recording just produced clip `id` (spec S6). Also the point where a
    /// job the recording preempted resumes — with [`AUTO_TRANSCRIBE`] off as
    /// much as on, which is why the resume is not inside the `if`.
    pub(super) fn transcribe_after_recording(&mut self, id: Uuid) {
        if AUTO_TRANSCRIBE {
            self.transcribe(id);
        }
        self.run_next_if_idle();
    }

    /// Starts the queue's first job unless something is in the way. Returns
    /// whether it published the state, so a caller that changed it knows
    /// whether it still has to.
    ///
    /// **Every place one of the exclusive jobs ends calls this** — a
    /// recording finishing *or aborting*, an export's last target, a preview
    /// closing — as well as every place the queue changes. Miss one and the
    /// queue stalls silently until the next enqueue.
    pub(super) fn run_next_if_idle(&mut self) -> bool {
        // Shutting down: the bus is about to drop, and a job started now
        // would only be cancelled and joined again, delaying the teardown the
        // UI waits on.
        if self.shutting_down || self.transcribing.is_some() {
            return false;
        }
        // One condition for all three, rather than a rule per pair: the
        // recording owns the machine (spec S5), the export owns the encoder,
        // and the preview owns the picture and the audio sink.
        if self.recording.is_some() || self.export.is_some() || self.preview.is_some() {
            return false;
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
        if changed {
            self.publish_transcription();
        }
        changed
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
                // them. A stale *failure*, though, belongs to a job nobody is
                // waiting for, and saying so would be noise.
                let current = generation == self.transcribe_generation;
                let mut changed = match result {
                    Ok(text) => self.write_transcript(clip, text),
                    Err(TranscribeError::Cancelled) => false,
                    Err(TranscribeError::Failed(e)) => {
                        eprintln!("bus: transcribing {clip} failed: {e}");
                        if current {
                            self.transcribe_failed = Some((clip, e));
                        }
                        current
                    }
                };
                if current {
                    self.transcribing = None;
                    changed = true;
                }
                if !self.run_next_if_idle() && changed {
                    self.publish_transcription();
                }
            }
        }
    }

    /// The machine's write (spec S7): apply, save and publish, and **never**
    /// push undo — an out-of-band undo entry is bundled into the coach's next
    /// focus-loss flush, so Ctrl+Z on a notes edit would silently revert the
    /// transcript.
    ///
    /// Returns whether the queue state changed with it.
    fn write_transcript(&mut self, clip: Uuid, text: String) -> bool {
        // Work already done is never redone: a job the recording preempted
        // may still have finished, and its clip is back on the queue.
        let changed = self.transcribe_queue.contains(&clip) || self.transcribe_failure_of(clip);
        self.transcribe_queue.retain(|&q| q != clip);
        self.clear_transcribe_failure(clip);

        let Some(open) = &mut self.open else {
            return changed;
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
        changed
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

    fn transcribe_failure_of(&self, id: Uuid) -> bool {
        self.transcribe_failed
            .as_ref()
            .is_some_and(|(c, _)| *c == id)
    }

    /// Drops the failure message if it is this clip's. One slot, in memory:
    /// a relaunch starts every clip idle, and a failed transcript is cheap to
    /// retry (spec S5).
    fn clear_transcribe_failure(&mut self, id: Uuid) {
        if self.transcribe_failure_of(id) {
            self.transcribe_failed = None;
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
        self.emit(Event::Transcription {
            queued: self.transcribe_queue.iter().copied().collect(),
            running: self.transcribing.as_ref().map(|a| (a.clip, a.percent)),
            failed: self.transcribe_failed.clone(),
        });
    }
}
