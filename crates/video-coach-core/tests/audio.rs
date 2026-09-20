//! The audio edit: which span of which file is heard at each emitted sample,
//! and the fades at its edges.
//!
//! Positions are **emitted** samples — the output timeline with the AAC
//! priming head dropped — so the numbers here are `frame · 1600 − 1024`.

use uuid::Uuid;

use video_coach_core::audio::{
    audio_regions, envelope, Region, Track, AUDIO_SAMPLE_RATE, PRIMING_SAMPLES, RAMP_SAMPLES,
};
use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::compilation_schedule;
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Clip, Project, SourceRef};

/// Audio samples per output frame: 48000 / 30.
const SPF: u64 = 1600;
const RATE: f64 = AUDIO_SAMPLE_RATE as f64;
const DUR: f64 = 1000.0;

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

fn pause(t: f64, anchor: f64) -> CommentaryEvent {
    CommentaryEvent::new(
        t,
        EventKind::Pause {
            source_time: anchor,
        },
    )
}
fn play(t: f64, anchor: f64) -> CommentaryEvent {
    CommentaryEvent::new(
        t,
        EventKind::Play {
            source_time: anchor,
        },
    )
}

/// Every region of a compilation of `clips`, at the given volumes.
fn regions_at(clips: Vec<Clip>, source: f64, commentary: f64) -> Vec<Region> {
    let mut p = Project::new("p");
    p.source_videos.push(SourceRef {
        relative_path: "film.mp4".into(),
        display_name: "film".into(),
        duration_seconds: DUR,
        display_aspect: 16.0 / 9.0,
    });
    p.clips = clips;
    p.preferences.preview_source_volume = source;
    p.preferences.preview_commentary_volume = commentary;
    let c = compilation_schedule(&p, &ExportTarget::AllClips);
    audio_regions(&c, &p.preferences)
}

fn regions(clips: Vec<Clip>) -> Vec<Region> {
    regions_at(clips, 1.0, 1.0)
}

fn track(rs: &[Region], t: Track) -> Vec<Region> {
    rs.iter().filter(|r| r.track == t).cloned().collect()
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// A region of `len` emitted samples at gain 1, for envelope tests.
fn span(len: u64) -> Region {
    Region {
        entry: 0,
        track: Track::Game,
        out_samples: 0..len,
        source_offset: 0.0,
        gain: 1.0,
    }
}

#[test]
fn a_playing_clip_is_one_game_region_and_one_commentary_region() {
    let rs = regions(vec![clip(10.0, 2.0, vec![])]);
    assert_eq!(rs.len(), 2);

    let game = &rs[0];
    assert_eq!(game.track, Track::Game);
    assert_eq!(game.entry, 0);
    assert_eq!(game.out_samples, 0..60 * SPF - PRIMING_SAMPLES);
    // The dropped head is skipped in the source too, not replayed late.
    assert!(approx(
        game.source_offset,
        10.0 + PRIMING_SAMPLES as f64 / RATE
    ));

    let mic = &rs[1];
    assert_eq!(mic.track, Track::Commentary);
    assert_eq!(mic.out_samples, game.out_samples);
    // The recording's own zero is the entry's record time zero.
    assert!(approx(mic.source_offset, PRIMING_SAMPLES as f64 / RATE));
}

#[test]
fn a_freeze_is_silent_on_the_game_track_and_not_on_the_commentary() {
    // Play 0–1 s, freeze 1–2 s on 11.0, play 2–3 s from 11.0.
    let c = clip(10.0, 3.0, vec![pause(1.0, 11.0), play(2.0, 11.0)]);
    let rs = regions(vec![c]);

    let game = track(&rs, Track::Game);
    assert_eq!(game.len(), 2);
    assert_eq!(game[0].out_samples, 0..30 * SPF - PRIMING_SAMPLES);
    assert_eq!(
        game[1].out_samples,
        60 * SPF - PRIMING_SAMPLES..90 * SPF - PRIMING_SAMPLES
    );
    assert!(approx(game[1].source_offset, 11.0));
    // Nothing covers the freeze.
    let mid = 45 * SPF - PRIMING_SAMPLES;
    assert!(game.iter().all(|r| !r.out_samples.contains(&mid)));

    let mic = track(&rs, Track::Commentary);
    assert_eq!(mic.len(), 1);
    assert!(mic[0].out_samples.contains(&mid));
}

#[test]
fn a_clip_that_never_plays_has_no_game_region() {
    let c = clip(10.0, 2.0, vec![pause(0.0, 10.0)]);
    let rs = regions(vec![c]);
    assert!(track(&rs, Track::Game).is_empty());
    assert_eq!(track(&rs, Track::Commentary).len(), 1);
}

#[test]
fn a_segment_boundary_lands_on_the_same_frame_as_the_picture() {
    // [0.03, 0.04) contains 1/30, so the freeze owns frame 1 and only frame 1
    // — `tests/export.rs` pins the same clip's frames.
    let c = clip(10.0, 1.0, vec![pause(0.03, 50.0), play(0.04, 20.0)]);
    let game = track(&regions(vec![c]), Track::Game);
    assert_eq!(game.len(), 2);
    assert_eq!(game[0].out_samples, 0..SPF - PRIMING_SAMPLES);
    assert_eq!(
        game[1].out_samples,
        2 * SPF - PRIMING_SAMPLES..30 * SPF - PRIMING_SAMPLES
    );
    assert!(approx(game[1].source_offset, 20.0));
}

#[test]
fn entries_abut_on_whole_frames_and_only_the_first_is_shortened() {
    // 0.55 s quantizes to 17 frames, so entry 1 starts at frame 17.
    let rs = regions(vec![clip(10.0, 0.55, vec![]), clip(20.0, 0.55, vec![])]);
    let game = track(&rs, Track::Game);
    assert_eq!(game.len(), 2);
    assert_eq!(game[0].entry, 0);
    assert_eq!(game[1].entry, 1);
    assert_eq!(game[0].out_samples, 0..17 * SPF - PRIMING_SAMPLES);
    assert_eq!(
        game[1].out_samples,
        17 * SPF - PRIMING_SAMPLES..34 * SPF - PRIMING_SAMPLES
    );
    // Only the first region loses samples to the priming drop.
    assert!(approx(game[1].source_offset, 20.0));

    let mic = track(&rs, Track::Commentary);
    assert_eq!(mic[1].out_samples, game[1].out_samples);
    assert!(approx(mic[1].source_offset, 0.0));
}

#[test]
fn an_empty_entry_contributes_nothing() {
    let rs = regions(vec![clip(10.0, 0.0, vec![]), clip(20.0, 1.0, vec![])]);
    assert!(rs.iter().all(|r| r.entry == 1));
    assert_eq!(rs.len(), 2);
}

#[test]
fn gains_come_from_the_preview_volumes() {
    let rs = regions_at(vec![clip(10.0, 1.0, vec![])], 0.25, 0.75);
    assert_eq!(track(&rs, Track::Game)[0].gain, 0.25);
    assert_eq!(track(&rs, Track::Commentary)[0].gain, 0.75);
}

#[test]
fn the_envelope_ramps_in_and_out_over_five_milliseconds() {
    let r = span(48_000);
    assert_eq!(envelope(&r, 0), 0.0);
    assert!(approx(envelope(&r, RAMP_SAMPLES / 2), 0.5));
    assert_eq!(envelope(&r, RAMP_SAMPLES), 1.0);
    assert_eq!(envelope(&r, 24_000), 1.0);
    assert_eq!(envelope(&r, 47_999), 0.0);
    assert!(approx(envelope(&r, 47_999 - RAMP_SAMPLES / 2), 0.5));
    assert_eq!(envelope(&r, 47_999 - RAMP_SAMPLES), 1.0);
}

#[test]
fn the_envelope_scales_by_the_regions_gain() {
    let mut r = span(48_000);
    r.gain = 0.5;
    assert_eq!(envelope(&r, 24_000), 0.5);
    assert!(approx(envelope(&r, RAMP_SAMPLES / 2), 0.25));
}

#[test]
fn a_region_shorter_than_two_ramps_never_reaches_full_gain() {
    // 100 samples, ~2 ms: it rises to 50/240 in the middle and back.
    let r = span(100);
    assert_eq!(envelope(&r, 0), 0.0);
    assert!(approx(envelope(&r, 50), 49.0 / RAMP_SAMPLES as f64));
    assert_eq!(envelope(&r, 99), 0.0);
    for s in 0..100 {
        assert!(envelope(&r, s) < 1.0, "sample {s}");
    }
    // Degenerate lengths stay finite rather than dividing by a zero span.
    assert_eq!(envelope(&span(1), 0), 0.0);
}

#[test]
fn the_envelope_is_zero_outside_the_region() {
    let r = Region {
        out_samples: 1_000..2_000,
        ..span(0)
    };
    assert_eq!(envelope(&r, 999), 0.0);
    assert_eq!(envelope(&r, 2_000), 0.0);
    assert_eq!(envelope(&r, 1_500), 1.0);
}

#[test]
fn the_first_region_fades_in_on_the_emitted_stream() {
    // The whole fade must survive the priming drop: it starts at emitted 0, not
    // part-way up a ramp the encoder never sees.
    let rs = regions(vec![clip(10.0, 2.0, vec![])]);
    for r in &rs {
        assert_eq!(r.out_samples.start, 0);
        assert_eq!(envelope(r, 0), 0.0);
        assert_eq!(envelope(r, RAMP_SAMPLES), 1.0);
    }
}
