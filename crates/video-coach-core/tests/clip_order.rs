//! Clip order and clip edits (Phase 3 spec C2, C3). All new: macOS kept
//! `sortIndex` loosely and snapshotted whole clips for edits.

use uuid::Uuid;

use video_coach_core::project::{Clip, Project};
use video_coach_core::undo::ClipEdit;

fn clip(n: u128, source_index: usize, start: f64) -> Clip {
    Clip {
        id: Uuid::from_u128(n),
        name: format!("c{n}"),
        notes: String::new(),
        tags: Vec::new(),
        source_index,
        start_source_seconds: start,
        recording_duration: 1.0,
        recording_filename: format!("{n}.mkv"),
        events: Vec::new(),
        show_pip: true,
        sort_index: 0,
        created_at: String::new(),
        transcript: String::new(),
    }
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// Clips 1..=n, in order and numbered.
fn project(n: u128) -> Project {
    let mut p = Project::new("p");
    p.clips = (1..=n).map(|i| clip(i, 0, 0.0)).collect();
    p.apply_clip_order(&[]);
    p
}

fn names(p: &Project) -> Vec<u128> {
    p.clips.iter().map(|c| c.id.as_u128()).collect()
}

fn assert_numbered(p: &Project) {
    for (i, c) in p.clips.iter().enumerate() {
        assert_eq!(c.sort_index, i as i64, "sort_index == position");
    }
}

// ------------------------------------------------------- apply_clip_order

#[test]
fn apply_clip_order_orders_and_renumbers() {
    let mut p = project(3);
    p.apply_clip_order(&[id(3), id(1), id(2)]);
    assert_eq!(names(&p), [3, 1, 2]);
    assert_numbered(&p);
}

/// An order captured before a delete or an add still applies: stale and
/// repeated ids are skipped, and clips it doesn't name follow in their current
/// order.
#[test]
fn apply_clip_order_skips_stale_ids_and_keeps_the_rest() {
    let mut p = project(4);
    p.apply_clip_order(&[id(9), id(3), id(3), id(1)]);
    assert_eq!(names(&p), [3, 1, 2, 4]);
    assert_numbered(&p);
}

#[test]
fn clip_order_lists_ids() {
    assert_eq!(project(2).clip_order(), [id(1), id(2)]);
}

// ------------------------------------------------------------ moved_order

#[test]
fn moved_order_follows_remove_then_insert() {
    let p = project(4);
    assert_eq!(p.moved_order(0, 2), [id(2), id(3), id(1), id(4)]);
    assert_eq!(p.moved_order(3, 0), [id(4), id(1), id(2), id(3)]);
    assert_eq!(p.moved_order(1, 1), p.clip_order(), "unchanged");
    assert_eq!(names(&p), [1, 2, 3, 4], "pure");
}

// ---------------------------------------------------- source_sorted_order

#[test]
fn source_sorted_order_is_by_source_then_start_and_stable() {
    let mut p = Project::new("p");
    p.clips = vec![
        clip(1, 1, 5.0),
        clip(2, 0, 30.0),
        clip(3, 1, 2.0),
        clip(4, 0, 30.0),
        clip(5, 0, 10.0),
    ];
    assert_eq!(p.source_sorted_order(), [id(5), id(2), id(4), id(3), id(1)]);
    assert_eq!(names(&p), [1, 2, 3, 4, 5], "pure");
}

// ------------------------------------------------- remove and insert

/// Delete then undo: the clip comes back to the same position.
#[test]
fn remove_then_insert_restores_the_position() {
    let mut p = project(4);
    let c = p.remove_clip(id(3)).unwrap();
    assert_eq!(c.sort_index, 2, "remembers where it was");
    assert_eq!(names(&p), [1, 2, 4]);
    assert_numbered(&p);

    p.insert_clip(c);
    assert_eq!(names(&p), [1, 2, 3, 4]);
    assert_numbered(&p);
}

#[test]
fn remove_of_a_missing_clip_is_none() {
    let mut p = project(2);
    assert!(p.remove_clip(id(9)).is_none());
    assert_eq!(names(&p), [1, 2]);
}

/// Deletes restored after the list shrank land at the end, not past it.
#[test]
fn insert_clamps_to_the_end() {
    let mut p = project(4);
    let c4 = p.remove_clip(id(4)).unwrap();
    let c3 = p.remove_clip(id(3)).unwrap();
    let c2 = p.remove_clip(id(2)).unwrap();
    // Undo in reverse order restores exactly.
    p.insert_clip(c2);
    p.insert_clip(c3);
    p.insert_clip(c4.clone());
    assert_eq!(names(&p), [1, 2, 3, 4]);

    let mut q = project(1);
    q.insert_clip(c4);
    assert_eq!(names(&q), [1, 4]);
    assert_numbered(&q);
}

#[test]
fn insert_with_a_negative_index_goes_first() {
    let mut p = project(2);
    let mut c = clip(9, 0, 0.0);
    c.sort_index = -5;
    p.insert_clip(c);
    assert_eq!(names(&p), [9, 1, 2]);
    assert_numbered(&p);
}

#[test]
fn insert_of_a_present_clip_is_a_no_op() {
    let mut p = project(3);
    let mut dup = p.clips[2].clone();
    dup.sort_index = 0;
    dup.name = "dup".into();
    p.insert_clip(dup);
    assert_eq!(names(&p), [1, 2, 3]);
    assert_eq!(p.clips[2].name, "c3");
}

// ------------------------------------------------------------ apply_edit

/// Every variant sets its one field and returns the previous value as the
/// same variant; nothing else on the clip changes (C2).
#[test]
fn apply_edit_sets_one_field_and_returns_the_old_value() {
    let cases = [
        (
            ClipEdit::Name("new".into()),
            ClipEdit::Name("c2".into()),
            (|c: &Clip| c.name == "new") as fn(&Clip) -> bool,
        ),
        (
            ClipEdit::Tags(vec!["wing".into()]),
            ClipEdit::Tags(Vec::new()),
            |c| c.tags == ["wing"],
        ),
        (
            ClipEdit::Notes("note".into()),
            ClipEdit::Notes(String::new()),
            |c| c.notes == "note",
        ),
        (ClipEdit::ShowPip(false), ClipEdit::ShowPip(true), |c| {
            !c.show_pip
        }),
    ];
    for (edit, old, applied) in cases {
        let mut p = project(3);
        let before = p.clips[1].clone();
        assert_eq!(p.apply_edit(id(2), edit.clone()), Some(old.clone()));
        assert!(applied(&p.clips[1]), "{edit:?} applied");

        // Applying the old value restores the clip exactly.
        assert_eq!(p.apply_edit(id(2), old), Some(edit));
        assert_eq!(p.clips[1], before);
        assert_eq!(names(&p), [1, 2, 3]);
    }
}

#[test]
fn apply_edit_of_a_missing_clip_is_none() {
    let mut p = project(1);
    let before = p.clone();
    assert_eq!(p.apply_edit(id(9), ClipEdit::Name("x".into())), None);
    assert_eq!(p, before);
}
