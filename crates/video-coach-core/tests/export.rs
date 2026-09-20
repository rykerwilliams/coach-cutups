//! The compilation schedule: which source time and zoom land at each output
//! frame, and where one entry ends and the next begins.
//!
//! Target filtering and empty targets are `tests/plan.rs`'s; these tests take
//! the selection as given and check the frames it produces.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::{compilation_schedule, Compilation, FrameSpec, OUTPUT_FPS};
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Clip, Project, SourceRef};
use video_coach_core::zoom::Zoom;

fn clip(start: f64, duration: f64, events: Vec<CommentaryEvent>) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: start,
        recording_duration: duration,
        recording_filename: "c.mkv".into(),
        events,
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// Every clip of `clips`, over one source `source_duration` seconds long.
fn compile(clips: Vec<Clip>, source_duration: f64) -> Compilation {
    let mut p = Project::new("p");
    p.source_videos.push(SourceRef {
        relative_path: "film.mp4".into(),
        display_name: "film".into(),
        duration_seconds: source_duration,
        display_aspect: 16.0 / 9.0,
    });
    p.clips = clips;
    compilation_schedule(&p, &ExportTarget::AllClips)
}

/// The frames of a one-entry compilation — the single-clip export.
fn schedule(c: Clip, source_duration: f64) -> Vec<FrameSpec> {
    compile(vec![c], source_duration).frames
}

fn play(t: f64, anchor: f64) -> CommentaryEvent {
    CommentaryEvent::new(
        t,
        EventKind::Play {
            source_time: anchor,
        },
    )
}
fn pause(t: f64, anchor: f64) -> CommentaryEvent {
    CommentaryEvent::new(
        t,
        EventKind::Pause {
            source_time: anchor,
        },
    )
}
fn skip(t: f64, delta: f64) -> CommentaryEvent {
    CommentaryEvent::new(t, EventKind::Skip { delta })
}
fn zoom(t: f64, scale: f64) -> CommentaryEvent {
    CommentaryEvent::new(t, EventKind::Zoom(Zoom::new(scale, 0.0, 0.0)))
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

const DUR: f64 = 1000.0;
const FPS: f64 = OUTPUT_FPS as f64;

#[test]
fn frame_count_covers_the_total() {
    assert_eq!(schedule(clip(0.0, 2.0, vec![]), DUR).len(), 60);
    // A partial trailing interval still gets its frame at 2.0 s.
    assert_eq!(schedule(clip(0.0, 2.01, vec![]), DUR).len(), 61);
    assert!(schedule(clip(0.0, 0.0, vec![]), DUR).is_empty());
}

#[test]
fn float_noise_in_the_total_adds_no_frame() {
    // 8.3 · 30 = 249.00000000000003.
    const { assert!(8.3 * FPS > 249.0) };
    assert_eq!(schedule(clip(0.0, 8.3, vec![]), DUR).len(), 249);
    // The same total built from a sum of segments.
    let c = clip(0.0, 8.3, vec![pause(0.1, 0.1), play(0.2, 0.1)]);
    assert_eq!(schedule(c, DUR).len(), 249);
}

#[test]
fn play_maps_output_time_onto_the_source() {
    let s = schedule(clip(10.0, 1.0, vec![]), DUR);
    for (n, f) in s.iter().enumerate() {
        assert!(approx(f.source_time, 10.0 + n as f64 / FPS), "frame {n}");
    }
}

#[test]
fn freeze_holds_the_pause_anchor() {
    // Pause at 1 s with a captured anchor off the computed cursor, resume at 2 s.
    let c = clip(10.0, 3.0, vec![pause(1.0, 11.013), play(2.0, 11.013)]);
    let s = schedule(c, DUR);
    assert_eq!(s.len(), 90);
    assert!(approx(s[29].source_time, 10.0 + 29.0 / FPS));
    for f in &s[30..60] {
        assert_eq!(f.source_time, 11.013);
    }
    assert!(approx(s[60].source_time, 11.013));
    assert!(approx(s[75].source_time, 11.013 + 0.5));
}

#[test]
fn skip_jumps_the_source() {
    let c = clip(10.0, 2.0, vec![skip(1.0, 5.0)]);
    let s = schedule(c, DUR);
    assert!(approx(s[29].source_time, 10.0 + 29.0 / FPS));
    assert!(approx(s[30].source_time, 16.0));
    assert!(approx(s[45].source_time, 16.5));
}

#[test]
fn a_sub_frame_segment_gets_a_frame_iff_it_contains_a_frame_time() {
    // [0.03, 0.04) contains 1/30 ≈ 0.0333: frame 1 is the freeze.
    let c = clip(10.0, 1.0, vec![pause(0.03, 50.0), play(0.04, 20.0)]);
    let s = schedule(c, DUR);
    assert_eq!(s[1].source_time, 50.0);
    assert_eq!(s.iter().filter(|f| f.source_time == 50.0).count(), 1);

    // [0.04, 0.05) lies between 1/30 and 2/30: the freeze gets no frame.
    let c = clip(10.0, 1.0, vec![pause(0.04, 50.0), play(0.05, 20.0)]);
    let s = schedule(c, DUR);
    assert!(s.iter().all(|f| f.source_time != 50.0));
    assert!(approx(s[1].source_time, 10.0 + 1.0 / FPS));
    assert!(approx(s[2].source_time, 20.0 + (2.0 / FPS - 0.05)));
}

#[test]
fn a_frame_on_a_segment_boundary_belongs_to_the_later_segment() {
    // The pause sits exactly on frame 3; the boundary is the sum 0.1 + 0.2,
    // which is 0.30000000000000004 and would otherwise keep frame 9 in play.
    let c = clip(10.0, 1.0, vec![pause(0.1, 50.0), play(0.1 + 0.2, 20.0)]);
    let s = schedule(c, DUR);
    assert!(approx(s[2].source_time, 10.0 + 2.0 / FPS));
    assert_eq!(s[3].source_time, 50.0);
    assert_eq!(s[8].source_time, 50.0);
    // Frame 9 is the start of the play, never a hair before its anchor.
    assert!(s[9].source_time >= 20.0);
    assert!(approx(s[9].source_time, 20.0));
}

#[test]
fn zoom_is_looked_up_per_frame() {
    let c = clip(0.0, 1.0, vec![zoom(0.0, 1.0), zoom(1.0, 2.0)]);
    let s = schedule(c, DUR);
    assert_eq!(s[0].zoom, Zoom::new(1.0, 0.0, 0.0));
    assert!(approx(s[15].zoom.scale, 1.5));
    assert!(approx(s[29].zoom.scale, 1.0 + 29.0 / FPS));
    // Zoom events do not split playback.
    assert!(approx(s[29].source_time, 29.0 / FPS));
}

#[test]
fn playing_off_the_source_end_freezes_short_of_it() {
    // 1 s of source left, 2 s of recording: play to the end, then hold the
    // last frame, capped 50 ms before the end so the decoder has a sample.
    let s = schedule(clip(9.0, 2.0, vec![]), 10.0);
    assert_eq!(s.len(), 60);
    assert!(approx(s[29].source_time, 9.0 + 29.0 / FPS));
    for f in &s[30..] {
        assert!(approx(f.source_time, 9.95));
    }
}

#[test]
fn a_one_entry_compilation_tags_every_frame_with_entry_zero() {
    let c = compile(vec![clip(0.0, 1.0, vec![])], DUR);
    assert_eq!(c.plan.entries.len(), 1);
    assert_eq!(c.plan.entries[0].start_frame, 0);
    assert_eq!(c.plan.entries[0].frames, 30);
    assert_eq!(c.plan.total_frames(), 30);
    assert!(c.frames.iter().all(|f| f.entry == 0));
}

/// Each entry is rounded **up** to a whole frame and the next starts on the
/// boundary, so record time stays output time inside every entry. The price is
/// that the rendered video is longer than the plan's segment sum — which is
/// why nothing measures the output with `total_duration_seconds`.
#[test]
fn entries_are_quantized_to_whole_frames() {
    // 2.01 s is 60.3 frames: 61 each, so the second entry starts at 61 rather
    // than at 2.01 s · 30 = 60.3.
    let c = compile(
        vec![clip(10.0, 2.01, vec![]), clip(20.0, 2.01, vec![])],
        DUR,
    );

    let [a, b] = &c.plan.entries[..] else {
        panic!("two entries")
    };
    assert_eq!((a.start_frame, a.frames), (0, 61));
    assert_eq!((b.start_frame, b.frames), (61, 61));
    assert_eq!(c.plan.total_frames(), 122);
    assert_eq!(c.frames.len(), 122);

    // The frame count exceeds the segment sum by the rounding, one frame per
    // entry: 122/30 = 4.0667 s against 4.02 s.
    assert!(approx(c.plan.total_duration_seconds, 4.02));
    assert!(c.plan.total_frames() as f64 / FPS > c.plan.total_duration_seconds);

    // The last frame of the first entry, then the first of the second.
    assert_eq!(c.frames[60].entry, 0);
    assert!(approx(c.frames[60].source_time, 10.0 + 2.0));
    assert_eq!(c.frames[61].entry, 1);
    assert!(approx(c.frames[61].source_time, 20.0));
}

/// The record time is derived from the entry and the global frame index, so an
/// entry's clock restarts at zero however many frames precede it.
#[test]
fn record_time_is_derived_from_the_entry_and_the_frame_index() {
    let c = compile(vec![clip(0.0, 2.01, vec![]), clip(0.0, 1.0, vec![])], DUR);
    let second = &c.plan.entries[1];

    assert_eq!(second.record_time(second.start_frame), 0.0);
    assert!(approx(second.record_time(second.start_frame + 15), 0.5));
    assert!(approx(
        second.record_time(second.start_frame + second.frames - 1),
        29.0 / FPS
    ));
}

/// The entry carries what its frames don't: which source to pull from, which
/// recording is the PiP, and whether to show it.
#[test]
fn an_entry_carries_its_source_recording_and_pip_flag() {
    let mut a = clip(0.0, 1.0, vec![]);
    a.source_index = 0;
    a.recording_filename = "a.mkv".into();
    a.show_pip = false;

    let c = compile(vec![a], DUR);
    let entry = &c.plan.entries[0];
    assert_eq!(entry.source_index, 0);
    assert_eq!(entry.recording_filename, "a.mkv");
    assert!(!entry.show_pip);
}

#[test]
fn the_text_line_numbers_the_clip_within_its_target() {
    let mut a = clip(0.0, 1.0, vec![]);
    a.name = "Back post header".into();
    a.tags = vec!["shot".into(), "set piece".into()];
    let mut b = clip(0.0, 1.0, vec![]);
    b.name = "Turnover".into();
    b.tags = vec![];

    let c = compile(vec![a, b], DUR);
    assert_eq!(
        c.plan.entries[0].text,
        "1 / 2 | Back post header | shot, set piece"
    );
    // No tags: the part and its separator both go.
    assert_eq!(c.plan.entries[1].text, "2 / 2 | Turnover");
}

#[test]
fn the_text_line_collapses_an_empty_name_and_empty_tags() {
    let mut a = clip(0.0, 1.0, vec![]);
    a.name = "   ".into();
    a.tags = vec!["shot".into()];
    let mut b = clip(0.0, 1.0, vec![]);
    b.name = String::new();
    b.tags = vec![];

    let c = compile(vec![a, b], DUR);
    assert_eq!(c.plan.entries[0].text, "1 / 2 | shot");
    assert_eq!(c.plan.entries[1].text, "2 / 2");
}

/// `<total>` is the **target's** clip count, not the project's.
#[test]
fn the_text_line_counts_only_the_targets_clips() {
    let mut a = clip(0.0, 1.0, vec![]);
    a.name = "a".into();
    a.tags = vec!["shot".into()];
    let mut b = clip(0.0, 1.0, vec![]);
    b.name = "b".into();
    let id = b.id;

    let mut p = Project::new("p");
    p.source_videos.push(SourceRef {
        relative_path: "film.mp4".into(),
        display_name: "film".into(),
        duration_seconds: DUR,
        display_aspect: 16.0 / 9.0,
    });
    p.clips = vec![a, b];

    let tag = compilation_schedule(&p, &ExportTarget::Tag("shot".into()));
    assert_eq!(tag.plan.entries[0].text, "1 / 1 | a | shot");

    let one = compilation_schedule(&p, &ExportTarget::Clip(id));
    assert_eq!(one.plan.entries.len(), 1);
    assert_eq!(one.plan.entries[0].text, "1 / 1 | b");
    assert_eq!(one.frames.len(), 30);
}
