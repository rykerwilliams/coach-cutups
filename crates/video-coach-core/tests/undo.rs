//! The undo history (Phase 3 spec C1).
//!
//! Ported from `UndoControllerTests.swift` except `test_pushDelete_evicts_*`:
//! macOS kept one delete in the history, and multi-level delete undo is a
//! deliberate divergence, so those are replaced by the "new" tests below.
//! `test_canUndo_canRedo_track_stacks` has no API left to test, and
//! `a_push_clears_redo` covers `test_pushDelete_with_no_prior_returns_nil`.

use uuid::Uuid;

use video_coach_core::project::Clip;
use video_coach_core::undo::{ClipEdit, UndoAction, UndoController, STACK_CAP};

fn clip(id: Uuid) -> Clip {
    Clip {
        id,
        name: "Clip".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: 0.0,
        recording_duration: 1.0,
        recording_filename: format!("{id}.mkv"),
        events: Vec::new(),
        show_pip: true,
        sort_index: 0,
        created_at: String::new(),
        transcript: String::new(),
    }
}

fn delete(id: Uuid) -> UndoAction {
    UndoAction::DeleteClip(clip(id))
}

fn edit_of(id: Uuid) -> UndoAction {
    UndoAction::EditClip {
        id,
        before: ClipEdit::Tags(Vec::new()),
        after: ClipEdit::Tags(vec!["a".into()]),
    }
}

fn edit() -> UndoAction {
    edit_of(Uuid::new_v4())
}

fn ids(clips: &[Clip]) -> Vec<Uuid> {
    clips.iter().map(|c| c.id).collect()
}

/// Undo one step the way the bus does: take it, then file it unchanged.
fn undo(c: &mut UndoController) -> Option<UndoAction> {
    let a = c.take_undo()?;
    c.file_redo(a.clone());
    Some(a)
}

fn redo(c: &mut UndoController) -> Option<UndoAction> {
    let a = c.take_redo()?;
    c.file_undo(a.clone());
    Some(a)
}

// ------------------------------------------------------------------ push

/// Ported: `test_pushEdit_appends_and_clears_redo`,
/// `test_pushEdit_accepts_reorder_action_and_clears_redo` and
/// `test_pushDelete_clears_redo_when_pushing_succeeds`.
#[test]
fn a_push_clears_redo() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let reorder = UndoAction::ReorderClips {
        before: vec![a, b],
        after: vec![b, a],
    };
    for next in [edit(), delete(Uuid::new_v4()), reorder] {
        let mut c = UndoController::default();
        let _ = c.push(edit());
        let _ = c.push(edit());
        undo(&mut c);
        undo(&mut c);
        assert_eq!(c.redo_stack().len(), 2);

        assert!(c.push(next.clone()).is_empty());

        assert_eq!(c.undo_stack(), [next]);
        assert!(c.redo_stack().is_empty());
    }
}

/// Ported: `test_pushEdit_trims_to_cap`. Also pins that the **oldest** entry
/// is the one dropped.
#[test]
fn push_trims_to_the_cap_from_the_front() {
    let mut c = UndoController::default();
    let first = edit();
    let second = edit();
    let _ = c.push(first.clone());
    let _ = c.push(second.clone());
    for _ in 0..STACK_CAP - 1 {
        assert!(c.push(edit()).is_empty());
    }
    assert_eq!(c.undo_stack().len(), STACK_CAP);
    assert_eq!(c.undo_stack()[0], second);
    assert!(!c.undo_stack().contains(&first));
}

// ------------------------------------------------------ take and file

/// Ported: `test_popForUndo_moves_action_to_redoStack`.
#[test]
fn undo_moves_the_action_to_redo() {
    let mut c = UndoController::default();
    let e = edit();
    let _ = c.push(e.clone());

    assert_eq!(c.take_undo(), Some(e.clone()));
    assert!(c.undo_stack().is_empty() && c.redo_stack().is_empty());
    c.file_redo(e.clone());
    assert_eq!(c.redo_stack(), [e]);
}

/// Ported: `test_popForUndo_returns_nil_when_empty` and
/// `test_popForRedo_returns_nil_when_empty`.
#[test]
fn taking_from_an_empty_stack_is_none() {
    let mut c = UndoController::default();
    assert_eq!(c.take_undo(), None);
    assert_eq!(c.take_redo(), None);
}

/// Ported: `test_popForRedo_moves_action_back_to_undoStack`.
#[test]
fn redo_moves_the_action_back_to_undo() {
    let mut c = UndoController::default();
    let e = edit();
    let _ = c.push(e.clone());
    undo(&mut c);

    assert_eq!(redo(&mut c), Some(e.clone()));
    assert_eq!(c.undo_stack(), [e]);
    assert!(c.redo_stack().is_empty());
}

/// New: `file_undo` files onto undo without clearing the rest of redo.
#[test]
fn file_undo_keeps_the_rest_of_redo() {
    let mut c = UndoController::default();
    let (a, b) = (edit(), edit());
    let _ = c.push(a.clone());
    let _ = c.push(b.clone());
    undo(&mut c);
    undo(&mut c);

    assert_eq!(redo(&mut c), Some(a.clone()));
    assert_eq!(c.undo_stack(), [a]);
    assert_eq!(c.redo_stack(), [b]);
}

/// New: the bus files an **updated** action, e.g. a redone delete files the
/// clip as it was when trashed. The filed action, not the taken one, is what
/// the next undo sees.
#[test]
fn the_filed_action_is_the_updated_one() {
    let mut c = UndoController::default();
    let id = Uuid::new_v4();
    let _ = c.push(delete(id));
    undo(&mut c);

    let Some(UndoAction::DeleteClip(mut trashed)) = c.take_redo() else {
        panic!("expected the delete on redo");
    };
    trashed.source_index = 3;
    trashed.name = "renamed".into();
    c.file_undo(UndoAction::DeleteClip(trashed.clone()));

    assert_eq!(c.take_undo(), Some(UndoAction::DeleteClip(trashed)));
}

// ----------------------------------------------------- multi-level deletes

/// New: three deletes undo newest first, and each can be redone.
#[test]
fn three_deletes_undo_in_order() {
    let mut c = UndoController::default();
    let (a, b, d) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    for id in [a, b, d] {
        assert!(c.push(delete(id)).is_empty(), "nothing is evicted");
    }
    assert_eq!(undo(&mut c), Some(delete(d)));
    assert_eq!(undo(&mut c), Some(delete(b)));
    assert_eq!(undo(&mut c), Some(delete(a)));
    assert_eq!(undo(&mut c), None);

    assert_eq!(redo(&mut c), Some(delete(a)));
    assert_eq!(redo(&mut c), Some(delete(b)));
    assert_eq!(redo(&mut c), Some(delete(d)));
    assert_eq!(redo(&mut c), None);
}

/// New: a delete the cap drops is returned, so its trashed file can be
/// shredded, and its clip's edits leave both stacks.
#[test]
fn a_cap_dropped_delete_is_returned_and_its_edits_purged() {
    let mut c = UndoController::default();
    let gone = Uuid::new_v4();
    let _ = c.push(edit_of(gone));
    let _ = c.push(delete(gone));
    for _ in 0..STACK_CAP - 2 {
        assert!(c.push(edit()).is_empty());
    }
    // Full. This push drops the front edit of `gone` (not a delete) and adds
    // another edit of `gone`, which the purge must reach.
    assert!(c.push(edit_of(gone)).is_empty());
    assert_eq!(c.undo_stack().len(), STACK_CAP);

    let evicted = c.push(edit());
    assert_eq!(ids(&evicted), [gone]);
    assert!(!c
        .undo_stack()
        .iter()
        .any(|a| matches!(a, UndoAction::EditClip { id, .. } if *id == gone)));
    assert_eq!(c.undo_stack().len(), STACK_CAP - 1);
}

/// New: `evict_deletes` returns every delete on the undo stack and purges
/// their edits from both stacks, keeping everything else in order.
#[test]
fn evict_deletes_purges_their_edits() {
    let mut c = UndoController::default();
    let (a, b, live) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let reorder = UndoAction::ReorderClips {
        before: vec![a, b],
        after: vec![b, a],
    };
    let _ = c.push(edit_of(a));
    let _ = c.push(delete(a));
    let _ = c.push(reorder.clone());
    let _ = c.push(edit_of(live));
    let _ = c.push(delete(b));

    let evicted = c.evict_deletes();

    assert_eq!(ids(&evicted), [a, b]);
    assert_eq!(c.undo_stack(), [reorder, edit_of(live)]);
    assert!(c.evict_deletes().is_empty());
}

/// New: a delete on the redo stack is a live clip (it was restored), so
/// source changes leave it alone; it is re-snapshotted when redone.
#[test]
fn evict_deletes_leaves_a_redo_delete() {
    let mut c = UndoController::default();
    let id = Uuid::new_v4();
    let _ = c.push(delete(id));
    undo(&mut c);

    assert!(c.evict_deletes().is_empty());
    assert_eq!(c.redo_stack(), [delete(id)]);
}

// ------------------------------------------------------------ clear

/// Ported: `test_clearAll_drops_both_stacks`.
#[test]
fn clear_drops_both_stacks() {
    let mut c = UndoController::default();
    let _ = c.push(edit());
    let _ = c.push(edit());
    undo(&mut c);
    assert!(!c.undo_stack().is_empty() && !c.redo_stack().is_empty());

    c.clear();

    assert!(c.undo_stack().is_empty() && c.redo_stack().is_empty());
}
