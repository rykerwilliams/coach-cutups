//! The source list: concat → source lookup, remove and move remaps, and the
//! aspect gate.
//!
//! All tests here are **new**. The Swift equivalents (`Workspace.sourceTime`,
//! `removeSourceVideo`, `reorderSourceVideos`, `aspectsMatch`) live in the
//! macOS app target and have no tests.

use uuid::Uuid;

use video_coach_core::project::{AspectMismatch, Clip, Project, SourceRef, SourceReferenced};
use video_coach_core::scoreboard::{MatchEventKind, MatchEventRecord};

const WIDE: f64 = 16.0 / 9.0;

fn source(name: &str, duration: f64, aspect: f64) -> SourceRef {
    SourceRef {
        relative_path: format!("{name}.mp4"),
        display_name: name.into(),
        duration_seconds: duration,
        display_aspect: aspect,
    }
}

/// A project whose sources are named by index ("s0", "s1", ...) so a test can
/// tell which physical file an index points at after a remap.
fn project(durations: &[f64]) -> Project {
    let mut p = Project::new("p");
    for (i, &d) in durations.iter().enumerate() {
        p.source_videos.push(source(&format!("s{i}"), d, WIDE));
    }
    p
}

fn clip_on(source_index: usize) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: format!("clip on {source_index}"),
        notes: String::new(),
        tags: Vec::new(),
        source_index,
        start_source_seconds: 1.0,
        recording_duration: 1.0,
        recording_filename: "x.mkv".into(),
        events: Vec::new(),
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

fn match_event_on(source_index: usize) -> MatchEventRecord {
    MatchEventRecord {
        id: Uuid::new_v4(),
        kind: MatchEventKind::HomeGoal,
        source_index,
        source_seconds: 1.0,
    }
}

/// The display name each clip's and match event's index resolves to — the
/// "same physical file" invariant a remap must preserve.
fn referenced_names(p: &Project) -> (Vec<String>, Vec<String>) {
    let name = |i: usize| p.source_videos[i].display_name.clone();
    (
        p.clips.iter().map(|c| name(c.source_index)).collect(),
        p.match_events
            .iter()
            .map(|m| name(m.source_index))
            .collect(),
    )
}

// ------------------------------------------------------------------- locate

#[test]
fn locate_finds_the_containing_source() {
    let p = project(&[10.0, 20.0, 30.0]);
    assert_eq!(p.locate(0.0), (0, 0.0));
    assert_eq!(p.locate(4.5), (0, 4.5));
    assert_eq!(p.locate(12.5), (1, 2.5));
    assert_eq!(p.locate(59.5), (2, 29.5));
}

/// An instant exactly on a boundary belongs to the next source, at 0 — the
/// strict `<` in `abs < cumulative + duration`.
#[test]
fn a_boundary_instant_belongs_to_the_next_source() {
    let p = project(&[10.0, 20.0, 30.0]);
    assert_eq!(p.locate(10.0), (1, 0.0));
    assert_eq!(p.locate(30.0), (2, 0.0));
}

/// The end of the last source, and anything past it, clamps to
/// `(last, last_duration)`.
#[test]
fn locate_clamps_past_the_end() {
    let p = project(&[10.0, 20.0]);
    assert_eq!(p.locate(30.0), (1, 20.0));
    assert_eq!(p.locate(1e9), (1, 20.0));
}

#[test]
fn locate_on_an_empty_project_is_zero() {
    assert_eq!(Project::new("p").locate(42.0), (0, 0.0));
}

/// Before the start clamps to 0 within the first source, never negative.
#[test]
fn locate_before_the_start_is_the_first_source_at_zero() {
    assert_eq!(project(&[10.0]).locate(-5.0), (0, 0.0));
}

/// A zero-length source can't contain an instant: its boundary instant goes to
/// the next source.
#[test]
fn locate_skips_a_zero_length_source() {
    let p = project(&[10.0, 0.0, 5.0]);
    assert_eq!(p.locate(10.0), (2, 0.0));
}

/// `abs_seconds` is the inverse. Durations and instants are dyadic so the
/// round trip is exact, not approximately equal.
#[test]
fn locate_round_trips_through_abs_seconds() {
    let p = project(&[10.5, 20.25, 30.125]);
    for abs in [0.25, 5.0, 10.75, 20.5, 30.5, 45.0, 60.5] {
        let (i, s) = p.locate(abs);
        assert_eq!(p.abs_seconds(i, s), abs, "abs {abs} -> ({i}, {s})");
    }
}

// ------------------------------------------------------------ source_is_referenced

#[test]
fn a_source_is_referenced_by_a_clip_or_a_match_event() {
    let mut p = project(&[10.0, 10.0, 10.0]);
    p.clips.push(clip_on(0));
    p.match_events.push(match_event_on(2));
    assert!(p.source_is_referenced(0));
    assert!(!p.source_is_referenced(1));
    assert!(p.source_is_referenced(2));
}

// ------------------------------------------------------------ remove_source

#[test]
fn remove_refuses_a_source_used_by_a_clip() {
    let mut p = project(&[10.0, 10.0]);
    p.clips.push(clip_on(1));
    let before = p.clone();
    assert_eq!(p.remove_source(1, 0), Err(SourceReferenced { index: 1 }));
    assert_eq!(p, before, "a refused remove changes nothing");
}

/// macOS counted clips only, so it would remove a source a match event used.
#[test]
fn remove_refuses_a_source_used_by_a_match_event() {
    let mut p = project(&[10.0, 10.0]);
    p.match_events.push(match_event_on(0));
    assert_eq!(p.remove_source(0, 1), Err(SourceReferenced { index: 0 }));
    assert_eq!(p.source_videos.len(), 2);
}

/// Higher indices drop by one in clips **and** match events, so everything
/// keeps pointing at the same physical file.
#[test]
fn remove_remaps_higher_indices_in_clips_and_match_events() {
    let mut p = project(&[10.0, 10.0, 10.0, 10.0]);
    p.clips = vec![clip_on(0), clip_on(2), clip_on(3)];
    p.match_events = vec![match_event_on(3), match_event_on(0)];
    let before = referenced_names(&p);

    assert_eq!(p.remove_source(1, 3), Ok(Some(2)));

    assert_eq!(p.source_videos.len(), 3);
    assert_eq!(referenced_names(&p), before);
    let clip_indices: Vec<_> = p.clips.iter().map(|c| c.source_index).collect();
    assert_eq!(clip_indices, [0, 1, 2]);
}

#[test]
fn remove_keeps_a_lower_current_index() {
    let mut p = project(&[10.0, 10.0, 10.0]);
    assert_eq!(p.remove_source(2, 1), Ok(Some(1)));
}

#[test]
fn removing_the_current_source_returns_none() {
    let mut p = project(&[10.0, 10.0, 10.0]);
    assert_eq!(p.remove_source(1, 1), Ok(None));
    assert_eq!(p.source_videos.len(), 2);
}

// -------------------------------------------------------------- move_source

fn names(p: &Project) -> Vec<&str> {
    p.source_videos
        .iter()
        .map(|s| s.display_name.as_str())
        .collect()
}

/// Every index in `0..n` is referenced by one clip and one match event, so the
/// whole permutation is checked, not just the moved source.
fn fully_referenced(n: usize) -> Project {
    let mut p = project(&vec![10.0; n]);
    p.clips = (0..n).map(clip_on).collect();
    p.match_events = (0..n).map(match_event_on).collect();
    p
}

#[test]
fn move_forward_remaps_everything() {
    let mut p = fully_referenced(4);
    let before = referenced_names(&p);

    let current = p.move_source(0, 2, 1);

    assert_eq!(names(&p), ["s1", "s2", "s0", "s3"]);
    assert_eq!(referenced_names(&p), before);
    assert_eq!(p.source_videos[current].display_name, "s1");
}

#[test]
fn move_backward_remaps_everything() {
    let mut p = fully_referenced(4);
    let before = referenced_names(&p);

    let current = p.move_source(3, 1, 2);

    assert_eq!(names(&p), ["s0", "s3", "s1", "s2"]);
    assert_eq!(referenced_names(&p), before);
    assert_eq!(p.source_videos[current].display_name, "s2");
}

#[test]
fn moving_the_current_source_moves_the_current_index() {
    let mut p = fully_referenced(3);
    assert_eq!(p.move_source(0, 2, 0), 2);
    assert_eq!(p.move_source(2, 0, 2), 0);
}

/// Duplicate paths are allowed: the move is by index, not by identity, so two
/// sources with the same path stay distinguishable. (macOS mapped by bookmark
/// and would have collapsed them.)
#[test]
fn move_distinguishes_duplicate_paths() {
    let mut p = Project::new("p");
    p.source_videos = vec![
        source("same", 10.0, WIDE),
        source("same", 10.0, WIDE),
        source("other", 10.0, WIDE),
    ];
    p.clips = vec![clip_on(0), clip_on(1)];
    p.move_source(0, 2, 0);
    let clip_indices: Vec<_> = p.clips.iter().map(|c| c.source_index).collect();
    assert_eq!(clip_indices, [2, 0]);
}

#[test]
fn a_move_to_the_same_place_is_a_no_op() {
    let mut p = fully_referenced(3);
    let before = p.clone();
    assert_eq!(p.move_source(1, 1, 1), 1);
    assert_eq!(p, before);
}

// ------------------------------------------------------------- check_aspect

#[test]
fn an_empty_project_has_no_aspect_gate() {
    assert_eq!(Project::new("p").check_aspect(4.0 / 3.0, None), Ok(()));
}

#[test]
fn a_matching_aspect_passes_and_a_different_one_fails() {
    let p = project(&[10.0]);
    assert_eq!(p.check_aspect(WIDE, None), Ok(()));
    assert_eq!(
        p.check_aspect(4.0 / 3.0, None),
        Err(AspectMismatch {
            existing: WIDE,
            attempted: 4.0 / 3.0,
        })
    );
}

/// Phone footage lands a pixel off (1920×1078) and must still pass.
#[test]
fn a_one_pixel_phone_crop_matches() {
    assert_eq!(project(&[10.0]).check_aspect(1920.0 / 1078.0, None), Ok(()));
}

/// The 0.5% edge is strict, tested from both sides of the reference.
#[test]
fn the_tolerance_edge_is_strict_in_both_directions() {
    let mut p = Project::new("p");
    p.source_videos.push(source("ref", 10.0, 2.0));
    // |a - b| / max(a, b): 2.0 is the max below the reference and the
    // candidate is the max above it.
    let just_inside_below = 2.0 * (1.0 - 0.0049);
    let just_outside_below = 2.0 * (1.0 - 0.0051);
    let just_inside_above = 2.0 / (1.0 - 0.0049);
    let just_outside_above = 2.0 / (1.0 - 0.0051);
    assert!(p.check_aspect(just_inside_below, None).is_ok());
    assert!(p.check_aspect(just_outside_below, None).is_err());
    assert!(p.check_aspect(just_inside_above, None).is_ok());
    assert!(p.check_aspect(just_outside_above, None).is_err());
}

/// Both aspects must be positive; NaN fails that test too.
#[test]
fn a_non_positive_or_nan_aspect_never_matches() {
    let p = project(&[10.0]);
    assert!(p.check_aspect(0.0, None).is_err());
    assert!(p.check_aspect(-WIDE, None).is_err());
    assert!(p.check_aspect(f64::NAN, None).is_err());

    let mut unprobed = Project::new("p");
    unprobed.source_videos.push(source("zero", 10.0, 0.0));
    assert!(unprobed.check_aspect(WIDE, None).is_err());
}

/// A sole source may be relinked to a new aspect: excluding it leaves no
/// reference, so there is no gate (macOS's stated relink intent).
#[test]
fn relinking_the_sole_source_is_ungated() {
    let p = project(&[10.0]);
    assert_eq!(p.check_aspect(4.0 / 3.0, Some(0)), Ok(()));
}

/// Relinking source 0 gates against source 1, not against itself.
#[test]
fn relink_gates_against_the_first_other_source() {
    let mut p = Project::new("p");
    p.source_videos = vec![source("a", 10.0, 4.0 / 3.0), source("b", 10.0, WIDE)];
    assert_eq!(p.check_aspect(WIDE, Some(0)), Ok(()));
    assert_eq!(
        p.check_aspect(4.0 / 3.0, Some(0)),
        Err(AspectMismatch {
            existing: WIDE,
            attempted: 4.0 / 3.0,
        })
    );
}
