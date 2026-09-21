//! Clip preview (Phase 7 spec P5): one clip's composite on screen in place of
//! the game video, built by a [`Preview`] on its own thread.
//!
//! The source player is **paused, not unloaded**, so closing a preview is a
//! re-request of where it already is: the preview fills the same mailbox, and
//! closing empties it and has the player preroll its own frame back into it,
//! so the last composited frame doesn't stay up (nor stay mapped by the UI).
//!
//! The preview's messages arrive as their own input, tagged with the
//! generation that sent them: closing joins the thread, but a message it had
//! already queued is still in the channel, and must not be taken for the next
//! preview's.

use uuid::Uuid;
use video_coach_core::export::compilation_schedule;
use video_coach_core::plan::ExportTarget;
use video_coach_core::scoreboard::ScoreboardContext;
use video_coach_core::store::RECORDINGS_DIRNAME;
use video_coach_media::{Gl, Origin, Preview, PreviewJob, PreviewMessage, SinkKind};

use super::{Bus, Event, Input, UserError};

/// The preview on screen: the thread rendering it, and the generation that
/// tags its messages.
pub(super) struct Active {
    generation: u64,
    /// The clip it shows: deleting that one closes it first (spec P5).
    clip: Uuid,
    pub(super) preview: Preview,
    /// The commentary is muted for the length of a scrub drag, since every
    /// tick flushes the audio sink (spec P3). Set by the first `ScrubMove`,
    /// cleared by the `ScrubRelease`.
    muted_for_scrub: bool,
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
        // While recording, `Bus::command`'s guard drops `OpenPreview` before
        // it reaches here, as it does `ExportClip`: the UI greys the button
        // and the menu item out, so either is only a UI bug.
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
        // The clip as a one-entry compilation: the same schedule export runs,
        // down to the bar's `1 / 1 | ...` line (spec E1, E7).
        let compilation = compilation_schedule(&open.project, &ExportTarget::Clip(id));
        if compilation.frames.is_empty() {
            return refused("the clip has nothing to preview");
        }
        // Which context composites follows the sink, not what has arrived:
        // with a GL sink the only one that may be used is Slint's, since its
        // textures are drawn by Slint (spec P1), so a preview asked for
        // before `GlReady` waits rather than quietly compositing on a
        // surfaceless display of its own.
        let gl = match self.sinks {
            SinkKind::Gl => match self.gl.clone() {
                Some(gl) => gl,
                None => return refused("the window isn't ready yet"),
            },
            // Headless -- tests and the harness -- has no UI context, and
            // composites on the process's surfaceless one (spec P1: "no
            // private GL context" is an app rule, not a test rule).
            SinkKind::System => Gl::shared().map_err(|e| UserError::CantPreview(e.to_string()))?,
        };
        // A snapshot: later edits to the clip don't reach this preview.
        let job = PreviewJob {
            source: open.folder.join(&video.relative_path),
            recording,
            clip: clip.clone(),
            compilation,
            commentary_volume: open.project.preferences.preview_commentary_volume,
            scoreboard: ScoreboardContext::for_project(&open.project),
        };

        // Whatever was on screen stops first, and takes its frame with it.
        self.close_preview();
        if self.playing {
            self.set_playing(false);
        }
        self.preview_generation += 1;
        let generation = self.preview_generation;
        let tx = self.tx.clone();
        let preview = Preview::start(
            job,
            gl,
            self.mailbox.clone(),
            self.preview_position.clone(),
            move |msg| {
                // Fails only once the bus thread has exited.
                let _ = tx.send(Input::Preview(generation, msg));
            },
        );
        self.preview = Some(Active {
            generation,
            clip: id,
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
        let from = self.preview_position.seconds();
        active.preview.seek(from + delta);
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
        // The game video is paused, not unloaded, so re-requesting where it
        // already is restores the picture: the flushing seek prerolls its own
        // frame back into the emptied mailbox, which is also what releases
        // the composited buffer the UI still holds mapped. (`load` closes the
        // preview, which is already gone, so this doesn't recurse.)
        self.load(self.current, self.current_secs(), true, Origin::System);
        // The play state was the preview's, and isn't now.
        if self.playing {
            self.set_playing(false);
        }
        eprintln!(
            "bus: preview closed: {} frames composited, {} dropped",
            stats.composited, stats.dropped
        );
        self.emit(Event::Preview(None));
    }

    /// Closes the preview if it is the one showing clip `id`, whose recording
    /// is about to move into the trash (spec P5).
    pub(super) fn close_preview_of(&mut self, id: Uuid) {
        if self.preview.as_ref().is_some_and(|a| a.clip == id) {
            self.close_preview();
        }
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
