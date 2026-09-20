//! Clip management (Phase 3 spec C1–C5): field edits, delete to the trash,
//! reorder and sort, jump, and the undo history they share.
//!
//! Every mutation is diffed first (unchanged ⇒ no save, no undo step), then
//! applied, saved, recorded and published: recorded before it's published,
//! so an event always follows any shred the push caused. Every push goes
//! through [`Bus::record`], so a delete the history drops always has its
//! trashed file shredded.
//!
//! **The trash.** A deleted clip's recording moves to
//! `recordings/.trash/<file>` only once the project without it has saved, and
//! comes back before the project with it is saved. So `project.json` never
//! lists a clip whose file is in `.trash`, which every open empties; a crash
//! leaves at worst an unreferenced recording in `recordings/` (BACKLOG #38).
//! Shredding only ever touches `.trash`.

use std::io;
use std::path::{Path, PathBuf};

use uuid::Uuid;
use video_coach_core::project::Clip;
use video_coach_core::store::RECORDINGS_DIRNAME;
use video_coach_core::undo::{ClipEdit, UndoAction};
use video_coach_media::Origin;

use super::{Bus, Event, UserError};

/// Inside `recordings/`, so moves in and out are same-filesystem renames.
const TRASH_DIRNAME: &str = ".trash";

impl Bus {
    pub(super) fn edit_clip(&mut self, id: Uuid, edit: ClipEdit) {
        let Some(open) = &mut self.open else {
            return;
        };
        let Some(before) = open.project.apply_edit(id, edit.clone()) else {
            return eprintln!("bus: EditClip on a clip that isn't there: {id}");
        };
        if before == edit {
            return;
        }
        self.save();
        self.record(UndoAction::EditClip {
            id,
            before,
            after: edit,
        });
        self.publish_project();
    }

    pub(super) fn delete_clip(&mut self, id: Uuid) {
        match self.trash_clip(id) {
            Some(clip) => {
                self.record(UndoAction::DeleteClip(clip));
                self.publish_project();
            }
            None => eprintln!("bus: DeleteClip on a clip that isn't there: {id}"),
        }
    }

    pub(super) fn move_clip(&mut self, from: usize, to: usize) {
        let Some(open) = &self.open else {
            return;
        };
        let len = open.project.clips.len();
        if from >= len || to >= len {
            return eprintln!("bus: MoveClip {{ {from} -> {to} }} out of range");
        }
        let order = open.project.moved_order(from, to);
        self.reorder_clips(order);
    }

    pub(super) fn sort_clips_by_source(&mut self) {
        if let Some(open) = &self.open {
            let order = open.project.source_sorted_order();
            self.reorder_clips(order);
        }
    }

    /// Applies `after` as the clip order, unless it already is.
    fn reorder_clips(&mut self, after: Vec<Uuid>) {
        let Some(open) = &mut self.open else {
            return;
        };
        let before = open.project.clip_order();
        if after == before {
            return;
        }
        open.project.apply_clip_order(&after);
        self.save();
        self.record(UndoAction::ReorderClips { before, after });
        self.publish_project();
    }

    /// Pauses the game video at the clip's start: an accurate user seek, like
    /// a scrub release. The skip burst is dropped first, so its debounce
    /// can't move the video afterwards.
    pub(super) fn jump_to_clip(&mut self, id: Uuid) {
        let Some(clip) = self
            .open
            .as_ref()
            .and_then(|open| open.project.clips.iter().find(|c| c.id == id))
        else {
            return eprintln!("bus: JumpToClip on a clip that isn't there: {id}");
        };
        let (index, secs) = (clip.source_index, clip.start_source_seconds);
        self.reset_skip();
        self.set_playing(false);
        if self.seekable() {
            self.load(index, secs, true, Origin::Scrub);
        }
    }

    pub(super) fn undo(&mut self) {
        let Some(action) = self.history.take_undo() else {
            return;
        };
        match action {
            UndoAction::DeleteClip(clip) => {
                let id = clip.id;
                if self.restore_clip(&clip) {
                    self.history.file_redo(UndoAction::DeleteClip(clip));
                    self.emit(Event::Select(id));
                } else {
                    // Still trashed: it stays undoable.
                    self.history.file_undo(UndoAction::DeleteClip(clip));
                }
            }
            action => {
                if let Some(action) = self.replay(action, false) {
                    self.history.file_redo(action);
                }
            }
        }
    }

    pub(super) fn redo(&mut self) {
        let Some(action) = self.history.take_redo() else {
            return;
        };
        match action {
            // Files the clip as it is now, not the old snapshot: its source
            // index may have been remapped since it was restored.
            UndoAction::DeleteClip(clip) => match self.trash_clip(clip.id) {
                Some(clip) => {
                    self.history.file_undo(UndoAction::DeleteClip(clip));
                    self.publish_project();
                }
                None => eprintln!(
                    "bus: redo: dropped the delete of a missing clip {}",
                    clip.id
                ),
            },
            action => {
                if let Some(action) = self.replay(action, true) {
                    self.history.file_undo(action);
                }
            }
        }
    }

    /// Applies an edit or a reorder for a redo (`forward`) or an undo, saves
    /// it, and returns it to be filed. `None` (and nothing saved) for an edit
    /// of a clip that's gone, which eviction's purge makes unreachable. A
    /// delete moves a file, so `undo` and `redo` handle it themselves.
    fn replay(&mut self, action: UndoAction, forward: bool) -> Option<UndoAction> {
        let open = self.open.as_mut()?;
        match &action {
            UndoAction::EditClip { id, before, after } => {
                let value = if forward { after } else { before };
                if open.project.apply_edit(*id, value.clone()).is_none() {
                    eprintln!("bus: dropped an edit of a missing clip {id}");
                    return None;
                }
                let id = *id;
                self.project_changed();
                self.emit(Event::Select(id));
            }
            UndoAction::ReorderClips { before, after } => {
                open.project
                    .apply_clip_order(if forward { after } else { before });
                self.project_changed();
            }
            UndoAction::DeleteClip(_) => unreachable!("deletes aren't replayed"),
        }
        Some(action)
    }

    /// Evicts every delete on the undo stack and shreds its file, after a
    /// source change: a trashed clip's `source_index` wasn't remapped, so
    /// restored it would point at the wrong video (spec C4).
    pub(super) fn evict_trashed_clips(&mut self) {
        let evicted = self.history.evict_deletes();
        self.shred(evicted);
    }

    /// Pushes `action` onto the history and shreds any delete the cap drops.
    /// The one way onto the history.
    fn record(&mut self, action: UndoAction) {
        let dropped = self.history.push(action);
        self.shred(dropped);
    }

    /// Removes clip `id` and saves; only if the save succeeded, moves its
    /// recording into `.trash`. Returns the clip as removed, or `None` if it
    /// isn't there. The caller files it, then publishes, so the snapshot
    /// follows the file.
    fn trash_clip(&mut self, id: Uuid) -> Option<Clip> {
        // Its recording is about to move into `.trash`, and a preview holds
        // that file open (spec P5).
        self.close_preview_of(id);
        let open = self.open.as_mut()?;
        let clip = open.project.remove_clip(id)?;
        if self.save() {
            let recordings = self.open.as_ref()?.folder.join(RECORDINGS_DIRNAME);
            let trash = recordings.join(TRASH_DIRNAME);
            let moved = std::fs::create_dir_all(&trash).and_then(|()| {
                rename_if_present(
                    &recordings.join(&clip.recording_filename),
                    &trash.join(&clip.recording_filename),
                )
            });
            // Left in `recordings/`, it is only an orphan: undo still works.
            if let Err(e) = moved {
                eprintln!("bus: couldn't trash {}: {e}", clip.recording_filename);
            }
        }
        Some(clip)
    }

    /// Moves the clip's recording back from `.trash`, then reinserts the clip
    /// at its position and saves. False if the file couldn't be moved back:
    /// the clip stays out, since the next open would shred a restored clip's
    /// file.
    fn restore_clip(&mut self, clip: &Clip) -> bool {
        let Some(open) = &mut self.open else {
            return false;
        };
        let recordings = open.folder.join(RECORDINGS_DIRNAME);
        let restored = rename_if_present(
            &trash_path(&open.folder, clip),
            &recordings.join(&clip.recording_filename),
        );
        if let Err(e) = restored {
            self.emit(Event::Error(UserError::Io(format!(
                "couldn't restore the clip's recording: {e}"
            ))));
            return false;
        }
        open.project.insert_clip(clip.clone());
        self.project_changed();
        true
    }

    /// Deletes the trashed files of clips the history has dropped.
    fn shred(&self, clips: Vec<Clip>) {
        let Some(open) = &self.open else {
            return;
        };
        for clip in clips {
            let path = trash_path(&open.folder, &clip);
            if let Err(e) = ignore_not_found(std::fs::remove_file(&path)) {
                eprintln!("bus: couldn't shred {}: {e}", path.display());
            }
        }
    }
}

/// Empties `recordings/.trash` in `folder`, as every open does: undo is
/// in-memory only, so nothing can restore what's there.
pub(super) fn empty_trash(folder: &Path) {
    let trash = folder.join(RECORDINGS_DIRNAME).join(TRASH_DIRNAME);
    if let Err(e) = ignore_not_found(std::fs::remove_dir_all(&trash)) {
        eprintln!("bus: couldn't empty {}: {e}", trash.display());
    }
}

/// Where a deleted clip's recording waits: `recordings/.trash/<file>`.
fn trash_path(folder: &Path, clip: &Clip) -> PathBuf {
    folder
        .join(RECORDINGS_DIRNAME)
        .join(TRASH_DIRNAME)
        .join(&clip.recording_filename)
}

/// Renames `from` to `to`, replacing it; a missing `from` is not an error.
fn rename_if_present(from: &Path, to: &Path) -> io::Result<()> {
    ignore_not_found(std::fs::rename(from, to))
}

fn ignore_not_found(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
