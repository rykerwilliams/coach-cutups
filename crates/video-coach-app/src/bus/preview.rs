//! Clip preview (Phase 7 spec P5): one clip's composite on screen in place of
//! the game video, built by a [`Preview`] on its own thread.
//!
//! The source player is **paused, not unloaded**, so closing a preview is a
//! no-op restore; the preview fills the same mailbox, and closing empties it
//! so the last composited frame doesn't stay up.
//!
//! The preview's messages arrive as their own input, tagged with the
//! generation that sent them: closing joins the thread, but a message it had
//! already queued is still in the channel, and must not be taken for the next
//! preview's.

use std::sync::{Arc, Mutex};

use uuid::Uuid;
use video_coach_core::export::frame_schedule;
use video_coach_core::store::RECORDINGS_DIRNAME;
use video_coach_media::{Gl, Preview, PreviewJob, PreviewMessage, PreviewPosition};

use super::{Bus, Event, Input, UserError};

/// The preview on screen: the thread rendering it, and the generation that
/// tags its messages.
pub(super) struct Active {
    generation: u64,
    pub(super) preview: Preview,
    /// The commentary is muted for the length of a scrub drag, since every
    /// tick flushes the audio sink (spec P3). Set by the first `ScrubMove`,
    /// cleared by the `ScrubRelease`.
    muted_for_scrub: bool,
}

/// Where the preview is, for the UI's 30 Hz tick. Empty while no preview is
/// open, when the game video's `PositionHandle` answers instead: one position
/// path, and no position event of the preview's own (spec P3).
#[derive(Clone, Default)]
pub struct PreviewPositionSlot(Arc<Mutex<Option<PreviewPosition>>>);

impl PreviewPositionSlot {
    /// Seconds into the clip, or `None` with no preview open.
    pub fn seconds(&self) -> Option<f64> {
        self.slot().as_ref().map(PreviewPosition::seconds)
    }

    fn set(&self, position: Option<PreviewPosition>) {
        *self.slot() = position;
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<PreviewPosition>> {
        self.0.lock().expect("the position slot isn't poisoned")
    }
}

impl Bus {
    /// Opens a preview of clip `id`, or says why it can't.
    pub(super) fn open_preview(&mut self, id: Uuid) {
        if let Err(e) = self.start_preview(id) {
            self.emit(Event::Error(e));
        }
    }

    fn start_preview(&mut self, id: Uuid) -> Result<(), UserError> {
        let refused = |why: &str| Err(UserError::CantPreview(why.into()));
        if self.export.is_some() {
            return refused("an export is running");
        }
        let Some(open) = &self.open else {
            return refused("no project is open");
        };
        let Some(clip) = open.project.clips.iter().find(|c| c.id == id) else {
            return refused("the clip is gone");
        };
        let Some(video) = open.project.source_videos.get(clip.source_index) else {
            return refused("the clip's game video is gone");
        };
        if self.missing.get(clip.source_index).copied().unwrap_or(true) {
            return refused("the clip's game video is missing; relink it first");
        }
        let recording = open
            .folder
            .join(RECORDINGS_DIRNAME)
            .join(&clip.recording_filename);
        if !recording.exists() {
            return refused("the clip's commentary recording is missing");
        }
        let frames = frame_schedule(clip, video.duration_seconds);
        if frames.is_empty() {
            return refused("the clip has nothing to preview");
        }
        // Slint's context in the app; a surfaceless one with no UI, which is
        // how tests and the harness preview (spec P1).
        let gl = match self.gl.clone() {
            Some(gl) => gl,
            None => Gl::shared().map_err(|e| UserError::CantPreview(e.to_string()))?,
        };
        // A snapshot: later edits to the clip don't reach this preview.
        let job = PreviewJob {
            source: open.folder.join(&video.relative_path),
            recording,
            clip: clip.clone(),
            frames,
            commentary_volume: open.project.preferences.preview_commentary_volume,
        };

        // Whatever was on screen stops first, and takes its frame with it.
        self.close_preview();
        if self.playing {
            self.set_playing(false);
        }
        self.preview_generation += 1;
        let generation = self.preview_generation;
        let tx = self.tx.clone();
        let preview = Preview::start(job, gl, self.mailbox.clone(), move |msg| {
            // Fails only once the bus thread has exited.
            let _ = tx.send(Input::Preview(generation, msg));
        });
        self.preview_position.set(Some(preview.position()));
        self.preview = Some(Active {
            generation,
            preview,
            muted_for_scrub: false,
        });
        self.emit(Event::Preview(Some(id)));
        // A preview starts playing, and the transport now drives it (spec P5).
        self.set_playing(true);
        Ok(())
    }

    /// Scrubbing a preview (spec P3): a frame-accurate seek per tick, with
    /// the commentary muted for the length of the drag.
    pub(super) fn preview_scrub(&mut self, secs: f64, release: bool) {
        let volume = self.open.as_ref().map_or(1.0, |open| {
            open.project.preferences.preview_commentary_volume
        });
        let Some(active) = &mut self.preview else {
            return;
        };
        if release {
            active.muted_for_scrub = false;
            active.preview.set_volume(volume);
        } else if !active.muted_for_scrub {
            active.muted_for_scrub = true;
            active.preview.set_volume(0.0);
        }
        active.preview.seek(secs);
    }

    /// Skipping in a preview: a seek from where it is, clamped to the clip.
    /// It bypasses `SkipCoordinator`, whose targets are concat source time
    /// (spec P5).
    pub(super) fn preview_skip(&mut self, delta: f64) {
        let Some(active) = &self.preview else {
            return;
        };
        active
            .preview
            .seek(active.preview.position().seconds() + delta);
    }

    /// Closes the preview, if one is open, and clears the picture it left.
    /// Idempotent.
    pub(super) fn close_preview(&mut self) {
        // Dropping it takes its pipelines to NULL and joins its thread, so
        // nothing can refill the mailbox after this.
        let Some(active) = self.preview.take() else {
            return;
        };
        let stats = active.preview.stats();
        drop(active.preview);
        self.preview_position.set(None);
        self.mailbox.take();
        // The game video is paused, not unloaded, so this is the whole of the
        // restore -- but the play state was the preview's, and isn't now.
        if self.playing {
            self.set_playing(false);
        }
        eprintln!(
            "bus: preview closed: {} frames composited at {:.2} fps, {} dropped",
            stats.composited, stats.fps, stats.dropped
        );
        self.emit(Event::Preview(None));
    }

    pub(super) fn preview_message(&mut self, generation: u64, msg: PreviewMessage) {
        if self
            .preview
            .as_ref()
            .is_none_or(|a| a.generation != generation)
        {
            return;
        }
        match msg {
            // The preview stays open, holding its last frame, with the
            // position at the end of the clip (spec P3). It paused itself, so
            // this only tells the UI.
            PreviewMessage::Ended => self.set_playing(false),
            PreviewMessage::Failed(e) => {
                eprintln!("bus: preview failed: {e}");
                self.close_preview();
                self.emit(Event::Error(UserError::CantPreview(e)));
            }
        }
    }
}
