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

use uuid::Uuid;
use video_coach_core::export::frame_schedule;
use video_coach_core::store::RECORDINGS_DIRNAME;
use video_coach_media::{Gl, Preview, PreviewJob, PreviewMessage};

use super::{Bus, Event, Input, UserError};

/// The preview on screen: the thread rendering it, and the generation that
/// tags its messages.
pub(super) struct Active {
    generation: u64,
    preview: Preview,
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
        };

        // Whatever was on screen stops first, and takes its frame with it.
        self.close_preview();
        self.set_playing(false);
        self.preview_generation += 1;
        let generation = self.preview_generation;
        let tx = self.tx.clone();
        self.preview = Some(Active {
            generation,
            preview: Preview::start(job, gl, self.mailbox.clone(), move |msg| {
                // Fails only once the bus thread has exited.
                let _ = tx.send(Input::Preview(generation, msg));
            }),
        });
        self.emit(Event::Preview(Some(id)));
        Ok(())
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
        self.mailbox.take();
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
            // The preview stays open on its last frame (spec P3); Task 3's
            // transport work is what reports the stop to the UI.
            PreviewMessage::Ended => {}
            PreviewMessage::Failed(e) => {
                eprintln!("bus: preview failed: {e}");
                self.close_preview();
                self.emit(Event::Error(UserError::CantPreview(e)));
            }
        }
    }
}
