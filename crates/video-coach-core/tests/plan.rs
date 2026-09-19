//! Compilation planning: target filtering, ordering, and duration accounting.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::plan::{compilation_plan, ExportTarget};
use video_coach_core::project::{Clip, Project, SourceRef};

fn clip(name: &str, sort_index: i64, tags: &[&str]) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: name.into(),
        notes: String::new(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        source_index: 0,
        start_source_seconds: 10.0,
        recording_duration: 5.0,
        recording_filename: format!("{name}.mkv"),
        events: Vec::new(),
        show_pip: true,
        sort_index,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

fn project_with(clips: Vec<Clip>) -> Project {
    let mut p = Project::new("p");
    p.source_videos.push(SourceRef {
        relative_path: "film.mp4".into(),
        display_name: "film".into(),
        duration_seconds: 1000.0,
        display_aspect: 16.0 / 9.0,
    });
    p.clips = clips;
    p
}

#[test]
fn an_empty_project_plans_nothing() {
    let p = project_with(vec![]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    assert!(plan.entries.is_empty());
    assert_eq!(plan.total_duration_seconds, 0.0);
}

#[test]
fn a_single_clip_plans_one_entry() {
    let p = project_with(vec![clip("a", 0, &["shot"])]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.total_duration_seconds, 5.0);
}

/// The stored order is the order (Phase 3 spec C3): `store::read` keeps
/// `clips` sorted, so the plan doesn't re-sort by `sort_index`.
#[test]
fn clips_are_planned_in_stored_order() {
    let p = project_with(vec![
        clip("third", 30, &[]),
        clip("first", 10, &[]),
        clip("second", 20, &[]),
    ]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    let ids: Vec<_> = plan.entries.iter().map(|e| e.clip_id).collect();
    let expect: Vec<_> = p.clips.iter().map(|c| c.id).collect();
    assert_eq!(ids, expect);
}

#[test]
fn a_tag_target_selects_only_matching_clips() {
    let p = project_with(vec![
        clip("a", 0, &["shot", "transition"]),
        clip("b", 1, &["transition"]),
        clip("c", 2, &["set piece"]),
    ]);
    let plan = compilation_plan(&p, &ExportTarget::Tag("transition".into()));
    assert_eq!(plan.entries.len(), 2);
    assert_eq!(plan.total_duration_seconds, 10.0);
}

#[test]
fn a_tag_matching_nothing_plans_nothing() {
    let p = project_with(vec![clip("a", 0, &["shot"])]);
    let plan = compilation_plan(&p, &ExportTarget::Tag("nope".into()));
    assert!(plan.entries.is_empty());
}

#[test]
fn all_clips_ignores_tags_entirely() {
    let p = project_with(vec![clip("a", 0, &[]), clip("b", 1, &["shot"])]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    assert_eq!(plan.entries.len(), 2);
}

/// The source duration comes from `SourceRef` — the single authority.
#[test]
fn source_duration_comes_from_the_project_by_default() {
    let mut c = clip("a", 0, &[]);
    c.start_source_seconds = 995.0;
    c.recording_duration = 20.0;
    let p = project_with(vec![c]); // source is 1000s long
    let plan = compilation_plan(&p, &ExportTarget::AllClips);

    // Only 5s of source remains, so it plays 5s then freezes for 15s.
    let segs = &plan.entries[0].segments;
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].out_duration, 5.0);
    assert_eq!(segs[1].out_duration, 15.0);
}

/// When a clip's source is missing, the fallback must be large enough that the
/// segment builder never clamps a forward skip it should not have.
#[test]
fn a_missing_source_falls_back_to_a_covering_duration() {
    let mut c = clip("a", 0, &[]);
    c.source_index = 7; // no such source
    let p = project_with(vec![c]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);

    // start 10 + duration 5 = 15 of covering source, so the whole clip plays.
    let segs = &plan.entries[0].segments;
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].out_duration, 5.0);
    assert_eq!(plan.total_duration_seconds, 5.0);
}

/// The duration comes from `SourceRef` and nowhere else. An earlier draft took
/// a map of probed durations that took precedence over it, which is exactly the
/// two-duration-sources disagreement the design exists to prevent.
#[test]
fn a_shorter_source_truncates_the_clip() {
    let mut c = clip("a", 0, &[]);
    c.start_source_seconds = 0.0;
    c.recording_duration = 20.0;
    let mut p = project_with(vec![c]);
    p.source_videos[0].duration_seconds = 8.0;

    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    let segs = &plan.entries[0].segments;
    assert_eq!(segs[0].out_duration, 8.0, "plays the available 8s");
    assert_eq!(segs[1].out_duration, 12.0, "then freezes for the rest");
}

/// `total_duration_seconds` is the sum of segment durations, not of recording
/// durations. An event past the end of the recording makes those disagree, and
/// since this value is the export-progress denominator, the wrong one would
/// show progress climbing past 100%.
#[test]
fn total_duration_comes_from_segments_not_recording_duration() {
    let mut c = clip("a", 0, &[]);
    c.recording_duration = 5.0;
    // An event beyond the recording's end — a recorder bug, but the plan must
    // stay self-consistent.
    c.events = vec![CommentaryEvent::new(
        9.0,
        EventKind::Pause { source_time: 12.0 },
    )];
    let p = project_with(vec![c]);

    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    let seg_sum: f64 = plan.entries[0]
        .segments
        .iter()
        .map(|s| s.out_duration)
        .sum();

    assert_eq!(plan.total_duration_seconds, seg_sum);
    assert_ne!(
        plan.total_duration_seconds, plan.entries[0].recording_duration,
        "this is the case where the two sums diverge"
    );
}

#[test]
fn entries_carry_their_clip_id_and_recording_duration() {
    let c = clip("a", 0, &[]);
    let id = c.id;
    let p = project_with(vec![c]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    assert_eq!(plan.entries[0].clip_id, id);
    assert_eq!(plan.entries[0].recording_duration, 5.0);
}
