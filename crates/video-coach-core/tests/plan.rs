//! Compilation planning: target filtering, ordering, length accounting and the
//! text bar's line.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::plan::{compilation_plan, ExportTarget, PlanEntry};
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

/// The entry's length in seconds: what its segments add up to.
fn segment_sum(entry: &PlanEntry) -> f64 {
    entry.segments.iter().map(|s| s.out_duration).sum()
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
}

#[test]
fn a_single_clip_plans_one_entry() {
    let p = project_with(vec![clip("a", 0, &["shot"])]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(segment_sum(&plan.entries[0]), 5.0);
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
    let expect: Vec<_> = p.clips.iter().map(|c| Some(c.id)).collect();
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

/// An entry's length is its segments', not its recording's. An event past the
/// end of the recording makes those disagree, and the segments are what the
/// plan quantizes into output frames.
#[test]
fn an_entrys_length_comes_from_its_segments_not_its_recording() {
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
    let seconds = segment_sum(&plan.entries[0]);
    assert!(
        seconds > 5.0,
        "this is the case where the two diverge: {seconds}"
    );
    // The frame count is the ceil of the segment sum, not of the recording.
    assert_eq!(plan.total_frames(), (seconds * 30.0).ceil() as usize);
}

#[test]
fn entries_carry_their_clip_id_and_source() {
    let mut c = clip("a", 0, &[]);
    c.source_index = 1;
    let id = c.id;
    let mut p = project_with(vec![c]);
    p.source_videos.push(SourceRef {
        relative_path: "second.mp4".into(),
        display_name: "second".into(),
        duration_seconds: 1000.0,
        display_aspect: 16.0 / 9.0,
    });

    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    assert_eq!(plan.entries[0].clip_id, Some(id));
    assert_eq!(plan.entries[0].source_index, 1);
}

// ── The bar's line ─────────────────────────────────────────────────────────

#[test]
fn the_text_line_numbers_the_clip_within_its_target() {
    let mut a = clip("Back post header", 0, &["shot", "set piece"]);
    a.recording_duration = 1.0;
    let mut b = clip("Turnover", 1, &[]);
    b.recording_duration = 1.0;

    let plan = compilation_plan(&project_with(vec![a, b]), &ExportTarget::AllClips);
    assert_eq!(
        plan.entries[0].text,
        "1 / 2 | Back post header | shot, set piece"
    );
    // No tags: the part and its separator both go.
    assert_eq!(plan.entries[1].text, "2 / 2 | Turnover");
}

#[test]
fn the_text_line_collapses_an_empty_name_and_empty_tags() {
    let a = clip("   ", 0, &["shot"]);
    let b = clip("", 1, &[]);

    let plan = compilation_plan(&project_with(vec![a, b]), &ExportTarget::AllClips);
    assert_eq!(plan.entries[0].text, "1 / 2 | shot");
    assert_eq!(plan.entries[1].text, "2 / 2");
}

/// `<total>` is the **target's** clip count, not the project's.
#[test]
fn the_text_line_counts_only_the_targets_clips() {
    let a = clip("a", 0, &["shot"]);
    let b = clip("b", 1, &[]);
    let id = b.id;
    let p = project_with(vec![a, b]);

    let tag = compilation_plan(&p, &ExportTarget::Tag("shot".into()));
    assert_eq!(tag.entries[0].text, "1 / 1 | a | shot");

    let one = compilation_plan(&p, &ExportTarget::Clip(id));
    assert_eq!(one.entries.len(), 1);
    assert_eq!(one.entries[0].text, "1 / 1 | b");
}

/// A chapter starts on its entry's first output frame, not at a sum of
/// durations: a 5.01 s clip takes 151 frames, so the next starts at 151 / 30
/// rather than 5.01.
#[test]
fn chapters_start_on_each_entrys_first_frame() {
    let mut a = clip("a", 0, &[]);
    a.recording_duration = 5.01;
    let p = project_with(vec![a, clip("b", 1, &["shot"]), clip("c", 2, &[])]);
    let plan = compilation_plan(&p, &ExportTarget::AllClips);
    assert_eq!(plan.entries[1].start_frame, 151);
    let chapters = plan.chapters();
    let expect: Vec<(f64, &str)> = plan
        .entries
        .iter()
        .map(|e| (e.start_frame as f64 / 30.0, e.text.as_str()))
        .collect();
    assert_eq!(chapters, expect);
    assert_eq!(chapters[0], (0.0, "1 / 3 | a"));
    assert_eq!(chapters[1], (151.0 / 30.0, "2 / 3 | b | shot"));
}

#[test]
fn fewer_than_two_entries_get_no_chapters() {
    let none = compilation_plan(&project_with(vec![]), &ExportTarget::AllClips);
    assert!(none.chapters().is_empty());
    let one = compilation_plan(
        &project_with(vec![clip("a", 0, &[])]),
        &ExportTarget::AllClips,
    );
    assert!(one.chapters().is_empty());
}
