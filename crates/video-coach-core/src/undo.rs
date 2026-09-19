//! The undo history.
//!
//! Ported from macOS `UndoController`. Pure: it holds the two stacks and
//! their push / cap / eviction rules, and never applies an action or touches a
//! file. The bus applies each action, moves recordings in and out of
//! `recordings/.trash/`, and shreds what eviction returns. Deviations (Phase 3
//! spec C1, C2):
//!
//! - **Any number of deletes can be undone.** macOS kept at most one delete
//!   across both stacks and evicted the previous one on every delete. Here a
//!   delete leaves the history only when the cap drops it or
//!   [`UndoController::evict_deletes`] removes it.
//! - **Eviction purges the clip's edits.** An evicted clip can never return,
//!   so its `EditClip` entries could only no-op; macOS kept them, and Ctrl+Z
//!   silently consumed them.
//! - **Take, then file.** macOS moved an action to the other stack as it
//!   popped it. Here the caller takes it, applies it, and files what it
//!   applied — possibly an updated action (a redone delete files the clip as
//!   it was when trashed), or nothing if its target is gone.
//! - **Edits snapshot one field** ([`ClipEdit`]), not the whole clip, so
//!   undoing an edit can't revert a later reorder.
//! - There is one `push`: with no one-delete invariant, deletes need no
//!   separate entry point.

use uuid::Uuid;

use crate::project::Clip;

/// The most actions the undo stack holds. Excess entries drop from the front
/// (oldest first). The redo stack inherits the bound: it only ever holds what
/// was on the undo stack.
pub const STACK_CAP: usize = 100;

/// One user-editable clip field and its value.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipEdit {
    Name(String),
    Tags(Vec<String>),
    Notes(String),
    ShowPip(bool),
}

/// One step of the history.
#[derive(Debug, Clone, PartialEq)]
pub enum UndoAction {
    /// One field of one clip. `before` and `after` are the same variant.
    EditClip {
        id: Uuid,
        before: ClipEdit,
        after: ClipEdit,
    },
    /// The clip as it was removed. Its recording is
    /// `recordings/.trash/<recording_filename>` while this is on the undo
    /// stack; on the redo stack the clip is live again.
    DeleteClip(Clip),
    /// Clip id order around a move or a sort.
    ReorderClips { before: Vec<Uuid>, after: Vec<Uuid> },
}

/// The undo and redo stacks, newest entry last.
#[derive(Debug, Default)]
pub struct UndoController {
    undo: Vec<UndoAction>,
    redo: Vec<UndoAction>,
}

impl UndoController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn undo_stack(&self) -> &[UndoAction] {
        &self.undo
    }

    pub fn redo_stack(&self) -> &[UndoAction] {
        &self.redo
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Record a new action: clears redo, appends, and enforces
    /// [`STACK_CAP`].
    ///
    /// Returns the deletes the cap dropped, whose trashed files the caller
    /// must shred.
    #[must_use]
    pub fn push(&mut self, action: UndoAction) -> Vec<Clip> {
        self.redo.clear();
        self.undo.push(action);
        let excess = self.undo.len().saturating_sub(STACK_CAP);
        let dropped: Vec<_> = self.undo.drain(..excess).collect();
        self.evict(dropped)
    }

    /// Remove the newest undo action for the caller to apply in reverse, then
    /// file with [`undone`](Self::undone).
    pub fn take_undo(&mut self) -> Option<UndoAction> {
        self.undo.pop()
    }

    /// File an action the caller has just undone onto the redo stack.
    pub fn undone(&mut self, action: UndoAction) {
        self.redo.push(action);
    }

    /// Remove the newest redo action for the caller to apply forward, then
    /// file with [`redone`](Self::redone).
    pub fn take_redo(&mut self) -> Option<UndoAction> {
        self.redo.pop()
    }

    /// File an action the caller has just redone onto the undo stack. Unlike
    /// [`push`](Self::push), this keeps the rest of the redo stack.
    pub fn redone(&mut self, action: UndoAction) {
        self.undo.push(action);
    }

    /// Evict every delete on the undo stack and return its clip, whose
    /// trashed file the caller must shred.
    ///
    /// For source changes: a trashed clip's `source_index` isn't remapped, so
    /// restored later it would point at the wrong video. A delete on the redo
    /// stack is left alone — that clip is live, was remapped, and is
    /// re-snapshotted when redone.
    #[must_use]
    pub fn evict_deletes(&mut self) -> Vec<Clip> {
        let (deletes, kept) = std::mem::take(&mut self.undo)
            .into_iter()
            .partition(|a| matches!(a, UndoAction::DeleteClip(_)));
        self.undo = kept;
        self.evict(deletes)
    }

    /// Drop both stacks, as on project open.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// The one eviction routine: of `removed` (actions already taken off the
    /// stacks), return the deleted clips, and purge every edit of those clips
    /// from both stacks.
    fn evict(&mut self, removed: Vec<UndoAction>) -> Vec<Clip> {
        let clips: Vec<Clip> = removed
            .into_iter()
            .filter_map(|a| match a {
                UndoAction::DeleteClip(clip) => Some(clip),
                _ => None,
            })
            .collect();
        if !clips.is_empty() {
            let is_evicted_edit = |a: &UndoAction| match a {
                UndoAction::EditClip { id, .. } => clips.iter().any(|c| c.id == *id),
                _ => false,
            };
            self.undo.retain(|a| !is_evicted_edit(a));
            self.redo.retain(|a| !is_evicted_edit(a));
        }
        clips
    }
}
