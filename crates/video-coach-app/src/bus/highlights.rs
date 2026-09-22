//! Player highlights (match vision spec H): the four commands, each one undo
//! step.
//!
//! **A key's position is the caller's**, like every logged position: the
//! stream time of the frame on screen when the coach let go of the drag. The
//! bus asking the player where it is would put the box on whatever frame the
//! queue delay had reached, and a ring a frame off the player it rings is
//! wrong in the preview and in every export.
//!
//! Each command is one `EditHighlights` step holding the **whole** list, as a
//! match-event edit is: the list is small, a snapshot needs no per-command
//! inverse, and a source move or removal purges it from both stacks because a
//! snapshot holds source indices.

use uuid::Uuid;
use video_coach_core::highlight::{HighlightEdit, HighlightKey, NormRect};
use video_coach_core::project::Project;
use video_coach_core::stroke::Rgba;
use video_coach_core::undo::UndoAction;

use super::Bus;

impl Bus {
    /// Places a key on highlight `id`, creating it with `color` if there is
    /// none. `source_seconds` is the displayed frame's stream time, so a
    /// second key on the same frame replaces this one (spec H2).
    pub(super) fn set_highlight_key(
        &mut self,
        id: Uuid,
        source_index: usize,
        source_seconds: f64,
        rect: NormRect,
        color: Rgba,
    ) {
        let Some(open) = &self.open else {
            return;
        };
        if source_index >= open.project.source_videos.len() {
            return eprintln!("bus: SetHighlightKey on source {source_index}, which isn't there");
        }
        let key = HighlightKey {
            source_seconds,
            rect,
            // Every hand-placed key, which is every key until P6's tracker.
            tracked: false,
        };
        let mut refused = None;
        self.edit_highlights(|project| {
            refused = project
                .set_highlight_key(id, source_index, color, key)
                .err();
        });
        // The H tool starts a new highlight rather than aiming a key at one on
        // another source, so reaching this is a UI bug, not a slip the coach
        // could make.
        if let Some(e) = refused {
            eprintln!("bus: SetHighlightKey on highlight {id}: {e}");
        }
    }

    pub(super) fn edit_highlight(&mut self, id: Uuid, edit: HighlightEdit) {
        self.edit_highlights(|project| project.edit_highlight(id, edit));
    }

    /// "Delete key here": removes highlight `id`'s key at exactly the
    /// displayed frame's stream time, which is the number that placed it.
    /// Deleting its last key deletes the highlight.
    pub(super) fn delete_highlight_key(&mut self, id: Uuid, source_seconds: f64) {
        self.edit_highlights(|project| project.delete_highlight_key(id, source_seconds));
    }

    pub(super) fn delete_highlight(&mut self, id: Uuid) {
        self.edit_highlights(|project| project.delete_highlight(id));
    }

    /// Applies `edit` to the highlight list as one undo step, unless it
    /// changed nothing — which is how a command naming a highlight that is
    /// gone costs neither a save nor a step.
    fn edit_highlights(&mut self, edit: impl FnOnce(&mut Project)) {
        let Some(open) = &mut self.open else {
            return;
        };
        let before = open.project.player_highlights.clone();
        edit(&mut open.project);
        let after = open.project.player_highlights.clone();
        if after == before {
            return;
        }
        self.save();
        self.record(UndoAction::EditHighlights { before, after });
        self.publish_project();
    }
}
