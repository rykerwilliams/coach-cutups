//! Bus end to end: transport (spec D4, D8) — skip bursts over the concat
//! timeline, scrub release, EOS advance and the end-of-source clamp.
//!
//! Positions are checked against the pipeline's own position query plus the
//! bus's latest published source index, and waited for by polling with a
//! timeout. Fixtures are short so EOS tests play out in about a second.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use video_coach_app::bus::{Command, Event, RecordingStatus, ScanStep};
use video_coach_core::project::Project;
use video_coach_harness::{
    landings, open_one_source_project, round_trip, shown_end, write_project, Harness, FRAME,
    SAME_FRAME,
};
use video_coach_media::fixtures::{counter_video_with, CounterKind, CounterQuirks};

/// A project in `<tmp>/project` whose sources are 16:9 30 fps WebM fixtures
/// of the given lengths in `<tmp>/media`, opened on a fresh bus that has
/// settled on the first source.
struct Rig {
    h: Harness,
    project: Project,
    _tmp: TempDir,
}

impl Rig {
    fn open(videos: &[(&str, u32)]) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&media).unwrap();
        let project = write_project(&folder, &media, videos);

        let h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder));
        let mut rig = Rig {
            h,
            project,
            _tmp: tmp,
        };
        rig.settle_at(0, 0.0);
        rig
    }

    fn duration(&self, index: usize) -> f64 {
        self.project.source_videos[index].duration_seconds
    }

    fn offset(&self, index: usize) -> f64 {
        self.project.cumulative_offset(index)
    }

    /// Waits until no seek is outstanding, the bus's current source is
    /// `index`, and the pipeline is within a frame of `secs` in it.
    fn settle_at(&mut self, index: usize, secs: f64) {
        self.h
            .poll_until(&format!("settled at {secs} in source {index}"), |h| {
                latest_position(h) == Some((index, None))
                    && h.position_secs().is_some_and(|p| (p - secs).abs() < FRAME)
            });
    }

    fn skips(&self, deltas: &[f64]) {
        // Back to back: all queued before the first seek lands, so they fall
        // in one burst.
        for &delta in deltas {
            self.h.skip(delta);
        }
    }
}

/// The latest `Position` the bus published: source index and target.
fn latest_position(h: &Harness) -> Option<(usize, Option<f64>)> {
    h.log().iter().rev().find_map(|e| match e {
        Event::Position {
            source_index,
            target_abs,
        } => Some((*source_index, *target_abs)),
        _ => None,
    })
}

/// Waits for `Playing(playing)`, skipping any earlier `Playing` events.
fn wait_playing(h: &mut Harness, playing: bool) {
    h.wait_map(&format!("Playing({playing})"), |e| {
        matches!(e, Event::Playing(p) if *p == playing).then_some(())
    });
}

#[test]
fn a_skip_burst_across_a_source_boundary_lands_on_the_accumulated_target() {
    let mut rig = Rig::open(&[("a.webm", 2), ("b.webm", 2)]);
    // 3.2 s is in b, between keyframes (every 0.5 s), so a keyframe landing
    // alone doesn't pass.
    rig.skips(&[0.8, 0.8, 0.8, 0.8]);
    let in_b = 3.2 - rig.offset(1);
    rig.settle_at(1, in_b);

    // And back across it.
    rig.skips(&[-0.8, -0.8]);
    rig.settle_at(0, 1.6);
    rig.h.shutdown();
}

#[test]
fn a_skip_burst_then_a_scrub_release_never_sticks() {
    let mut rig = Rig::open(&[("a.webm", 2), ("b.webm", 2)]);
    rig.skips(&[0.8, 0.8, 0.8, 0.8]);
    rig.h.send(Command::ScrubRelease { abs: 0.5 });
    rig.settle_at(0, 0.5);

    // The abandoned burst doesn't resume (its debounce would land at 3.2 and
    // this skip would then start from there), and the next skip works.
    rig.skips(&[1.0]);
    rig.settle_at(0, 1.5);
    rig.skips(&[0.7, 0.7]);
    let in_b = 2.9 - rig.offset(1);
    rig.settle_at(1, in_b);
    rig.h.shutdown();
}

#[test]
fn eos_advances_to_the_next_source_and_keeps_playing() {
    let mut rig = Rig::open(&[("a.webm", 1), ("b.webm", 1)]);
    rig.h.toggle_play();
    wait_playing(&mut rig.h, true);
    let started = rig.h.log().len();

    // Settled in b and moving: it was loaded and is playing on its own.
    rig.h.poll_until("playback past 0.3 s in b", |h| {
        latest_position(h) == Some((1, None)) && h.position_secs().is_some_and(|p| p > 0.3)
    });
    let paused = rig.h.log()[started..]
        .iter()
        .any(|e| matches!(e, Event::Playing(false)));
    assert!(!paused, "playback stopped at the boundary");
    rig.h.shutdown();
}

#[test]
fn eos_on_the_last_source_leaves_it_paused_at_the_end() {
    let mut rig = Rig::open(&[("a.webm", 1)]);
    rig.h.toggle_play();
    wait_playing(&mut rig.h, true);
    wait_playing(&mut rig.h, false);

    assert_eq!(latest_position(&rig.h), Some((0, None)));
    let end = rig.duration(0);
    let at = rig.h.position_secs().expect("a position at the end");
    assert!(end - at < 2.0 * FRAME, "at {at}, end {end}");

    let rest = rig.h.shutdown();
    assert!(
        !rest.iter().any(|e| matches!(e, Event::Position { .. })),
        "nothing reloaded after the end: {rest:#?}"
    );
}

#[test]
fn a_seek_in_the_final_second_stays_in_its_source() {
    let mut rig = Rig::open(&[("a.webm", 2), ("b.webm", 2)]);
    let end_a = rig.duration(0);

    rig.h.send(Command::ScrubRelease { abs: end_a - 0.5 });
    rig.settle_at(0, end_a - 0.5);
    // Just short of the boundary: clamped short of a's end, not into b.
    rig.h.send(Command::ScrubRelease { abs: end_a - 0.01 });
    rig.settle_at(0, end_a - 0.05);
    // A skip to the same place, from the start.
    rig.h.send(Command::ScrubRelease { abs: 0.0 });
    rig.settle_at(0, 0.0);
    rig.skips(&[end_a - 0.01]);
    rig.settle_at(0, end_a - 0.05);

    // Past the end of the timeline: short of the last source's end.
    let end_b = rig.duration(1);
    rig.skips(&[10.0]);
    rig.settle_at(1, end_b - 0.05);
    rig.h.send(Command::ScrubRelease { abs: 100.0 });
    rig.settle_at(1, end_b - 0.05);
    rig.h.shutdown();
}

#[test]
fn the_position_survives_removing_an_earlier_source() {
    let mut rig = Rig::open(&[("a.webm", 2), ("b.webm", 2), ("c.webm", 2)]);
    rig.h.send(Command::ScrubRelease {
        abs: rig.offset(2) + 0.7,
    });
    rig.settle_at(2, 0.7);

    rig.h.send(Command::RemoveSource(0));
    rig.h.wait_changed();
    // c is now source 1, still at 0.7: nothing reloaded.
    rig.settle_at(1, 0.7);

    // Skips work from the new offsets.
    rig.skips(&[0.5]);
    rig.settle_at(1, 1.2);
    rig.h.shutdown();
}

/// The 10 s edit-listed MP4 whose stream times sit a nanosecond off round
/// numbers, open on a fresh `Harness::new` and settled.
fn open_edit_listed(tmp: &Path) -> (Harness, PathBuf) {
    gstreamer::init().unwrap();
    let source = counter_video_with(
        &tmp.join("src.mp4"),
        640,
        360,
        30,
        300,
        CounterKind::H264Mp4BFrames,
        CounterQuirks::default(),
    );
    let mut h = Harness::new(&tmp.join("config"));
    open_one_source_project(&mut h, &tmp.join("project"), &source, false);
    (h, source)
}

/// A scrub released while paused shows the frame export picks for the
/// position it reports, within a frame of its target (spec H6), on an
/// edit-listed MP4: raw PTS runs two frames ahead of stream time there, the
/// trap both sides must avoid. `real_footage.rs` runs the same round trip on
/// real footage.
#[test]
fn a_paused_scrub_shows_the_frame_export_picks() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut h, source) = open_edit_listed(tmp.path());
    // 20 targets over 0.1..9.4 s at every phase of a frame, starting on a
    // boundary: 0.1 s is frame 3's, which stream time puts 1 ns later.
    let targets: Vec<f64> = (0..20).map(|i| 0.1 + f64::from(i) * 0.487).collect();
    for landing in round_trip(&mut h, &source, &targets) {
        landing.check_export();
        landing.check_target();
    }
    h.shutdown();
}

/// `,` and `.` step one frame while paused (plan Task 0.3). A forward step
/// lands on the next frame's start; a step back lands inside the previous
/// frame. Either way the position reported is the displayed frame's stream
/// time, which is the frame export picks for it. Frames are told apart by
/// their ends: a seek clips the frame it lands inside to its target.
#[test]
fn a_paused_step_moves_exactly_one_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut h, source) = open_edit_listed(tmp.path());
    let step = |forward| Command::StepFrame { forward };

    // Mid-frame, so the first frame shown is clipped and a step back can't
    // take its start for the frame's own.
    let (_, first) = h.seek_and_settle(Command::ScrubRelease { abs: 5.02 });
    let start_end = shown_end(&first);
    let mut landed = Vec::new();
    for (forward, n) in [(true, 1..=5), (false, 1..=5)] {
        for i in n {
            let (reported, frame) = h.seek_and_settle(step(forward));
            let frames = if forward { i } else { 5 - i };
            let expected = start_end + f64::from(frames) * FRAME;
            assert!(
                (shown_end(&frame) - expected).abs() <= SAME_FRAME,
                "step {i} {} shows the frame ending at {}, not {expected}",
                if forward { "forward" } else { "back" },
                shown_end(&frame)
            );
            let shown = frame.stream_time.expect("a timed frame");
            assert!(
                (reported - shown).abs() <= SAME_FRAME,
                "reported {reported}, but the frame shown starts at {shown}"
            );
            // A step has no target of its own: it lands where it reports.
            landed.push((reported, reported, shown_end(&frame)));
        }
    }
    // What P2 relies on: a key placed at the reported position is drawn on
    // the frame the coach saw.
    for landing in landings(&source, &landed) {
        landing.check_export();
    }

    // Back from the first frame: nothing moves. Play is the barrier: the
    // step was handled before it.
    h.seek_and_settle(Command::ScrubRelease { abs: 0.0 });
    let from = h.log().len();
    h.send(step(false));
    h.toggle_play();
    wait_playing(&mut h, true);
    // And while playing, nothing either. Pause is the barrier.
    h.send(step(true));
    h.toggle_play();
    wait_playing(&mut h, false);
    let seeks: Vec<&Event> = h.log()[from..]
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::Position {
                    target_abs: Some(_),
                    ..
                }
            )
        })
        .collect();
    assert!(seeks.is_empty(), "a step was taken: {seeks:#?}");
    h.shutdown();
}

/// A 60 s counter video with an audio track in `<tmp>/src.webm`, as the one
/// source of a project in `<tmp>/project`, opened on `h` and settled, paused,
/// with the speakers muted (the production sinks reach the real ones).
fn open_minute(h: &mut Harness, tmp: &Path) {
    gstreamer::init().unwrap();
    let source = counter_video_with(
        &tmp.join("src.webm"),
        384,
        216,
        30,
        1800,
        CounterKind::Vp8WebmWithAudio,
        CounterQuirks::default(),
    );
    open_one_source_project(h, &tmp.join("project"), &source, true);
}

/// Asserts the picture runs at `speed` for 2 s from now, and keeps up with
/// the reported position.
fn assert_displayed_speed(h: &mut Harness, speed: f64, after: &str) {
    let seen = h.watch_displayed(2.0);
    eprintln!("after {after}: {seen}");
    assert!(
        (seen.rate / speed - 1.0).abs() <= 0.25,
        "after {after}, the picture runs at {:.2}x, not {speed}x",
        seen.rate
    );
    assert!(
        seen.max_lag <= 0.5,
        "after {after}, the picture lags {:.3} s",
        seen.max_lag
    );
}

/// From 1x while playing, `L` twice: 4x, once its seek settles.
fn four_times(h: &mut Harness) {
    h.send(Command::ScanSpeed(ScanStep::Faster));
    h.send(Command::ScanSpeed(ScanStep::Faster));
    h.wait_speed(4.0);
    h.wait_settled();
}

/// Plays at 4x and scrubs while fast, timing each from the seek that set it
/// going (spec S1, S3, S5).
fn check_four_times(h: &mut Harness) {
    h.toggle_play();
    wait_playing(h, true);
    four_times(h);
    assert_displayed_speed(h, 4.0, "4x");

    // A scrub keeps the speed: the seek carries the rate.
    h.send(Command::ScrubRelease { abs: 20.0 });
    h.wait_settled();
    assert_displayed_speed(h, 4.0, "a scrub at 4x");
}

/// Fast scanning (plan Task 0.4, spec S): the displayed frame runs at the
/// chosen speed and stays with the position, through a scrub; any pause
/// returns to 1x, a recording's start included; and a speed is refused
/// while paused or recording.
#[test]
fn fast_scanning_runs_at_the_chosen_speed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = Harness::new(&tmp.path().join("config"));
    open_minute(&mut h, tmp.path());

    // Refused while paused: play is the barrier.
    let from = h.log().len();
    h.send(Command::ScanSpeed(ScanStep::Faster));
    h.toggle_play();
    wait_playing(&mut h, true);
    let speeds: Vec<&Event> = h.log()[from..]
        .iter()
        .filter(|e| matches!(e, Event::ScanSpeed(_)))
        .collect();
    assert!(
        speeds.is_empty(),
        "a speed was set while paused: {speeds:#?}"
    );
    h.toggle_play();
    wait_playing(&mut h, false);

    check_four_times(&mut h);

    // A pause returns to 1x, and the next play runs at 1x.
    h.toggle_play();
    wait_playing(&mut h, false);
    h.wait_speed(1.0);
    h.wait_settled();
    h.toggle_play();
    wait_playing(&mut h, true);
    assert_displayed_speed(&mut h, 1.0, "a pause and a play");

    // A recording's start pauses, and so returns to 1x (spec S2).
    h.send(Command::ScanSpeed(ScanStep::Faster));
    h.wait_speed(2.0);
    h.send(Command::ToggleRecording {
        zoom: video_coach_core::zoom::Zoom::IDENTITY,
    });
    h.wait_speed(1.0);
    h.wait_map("the recording", |e| {
        matches!(e, Event::Recording(RecordingStatus::Recording { .. })).then_some(())
    });
    // And while recording, a speed is refused: pause is the barrier.
    h.toggle_play();
    wait_playing(&mut h, true);
    let from = h.log().len();
    h.send(Command::ScanSpeed(ScanStep::Faster));
    h.toggle_play();
    wait_playing(&mut h, false);
    let speeds: Vec<&Event> = h.log()[from..]
        .iter()
        .filter(|e| matches!(e, Event::ScanSpeed(_)))
        .collect();
    assert!(
        speeds.is_empty(),
        "a speed was set while recording: {speeds:#?}"
    );
    h.send(Command::StopRecording);
    h.wait_map("the recording's end", |e| {
        matches!(e, Event::Recording(RecordingStatus::Idle)).then_some(())
    });
    h.shutdown();
}

/// [`fast_scanning_runs_at_the_chosen_speed`]'s 4x on the app's own sinks: the
/// GL sink, and the real `autoaudiosink`, which a burst of fast flushing seeks
/// is what wedged when it was `pulsesink` (CLAUDE.md). Muted.
#[test]
fn fast_scanning_runs_at_the_chosen_speed_on_the_apps_sinks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = Harness::production(&tmp.path().join("config"));
    open_minute(&mut h, tmp.path());
    check_four_times(&mut h);
    h.shutdown();
}

/// A pause at speed stays on the frame on screen (spec S3, S5), not on the
/// position, which the picture trails while fast: the seek back to 1x goes
/// to the frame shown. Within a frame, since the sink may put up one more
/// between the frame taken here and the pause reaching the bus. Headless, the
/// picture keeps within a frame of the position even at 32x, so this pins
/// the rule; the lag it guards against needs real footage (CLAUDE.md).
#[test]
fn a_pause_at_speed_stays_on_the_frame_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = Harness::new(&tmp.path().join("config"));
    open_minute(&mut h, tmp.path());
    h.toggle_play();
    wait_playing(&mut h, true);
    four_times(&mut h);
    h.watch_displayed(0.5);
    let mut shown = None;
    h.poll_until("a frame at 4x", |h| {
        shown = h.take_frame();
        shown.is_some()
    });
    h.toggle_play();
    let paused_on = shown_end(&shown.expect("polled until some"));
    wait_playing(&mut h, false);
    h.wait_speed(1.0);
    h.wait_settled();
    let mut after = None;
    h.poll_until("the frame the pause settles on", |h| {
        after = h.take_frame();
        after.is_some()
    });
    let after = shown_end(&after.expect("polled until some"));
    // In frames, rounded: WebM's timestamps are whole milliseconds.
    let moved = ((after - paused_on) / FRAME).round();
    eprintln!("paused on ..{paused_on:.4}, settled on ..{after:.4}");
    assert!(
        moved == 0.0 || moved == 1.0,
        "paused on the frame ending at {paused_on}, but settled {moved} frames on"
    );
    h.shutdown();
}

/// At speed, running into the next source keeps the speed: the load's seek
/// carries the rate (spec S3). The end of the last source pauses, which
/// returns to 1x.
#[test]
fn the_next_source_keeps_the_speed_and_the_end_returns_to_1x() {
    let mut rig = Rig::open(&[("a.webm", 2), ("b.webm", 2)]);
    let h = &mut rig.h;
    h.toggle_play();
    wait_playing(h, true);
    four_times(h);
    let from = h.log().len();
    h.wait_map("the advance into b", |e| {
        matches!(
            e,
            Event::Position {
                source_index: 1,
                ..
            }
        )
        .then_some(())
    });
    let advanced = std::time::Instant::now();
    wait_playing(h, false);
    let played = advanced.elapsed().as_secs_f64();
    let speeds: Vec<f64> = h.log()[from..]
        .iter()
        .filter_map(|e| match e {
            Event::ScanSpeed(s) => Some(*s),
            _ => None,
        })
        .collect();
    assert!(
        speeds.is_empty(),
        "the speed changed before the end: {speeds:?}"
    );
    // 2 s of b at 4x is half a second.
    assert!(played < 1.2, "b played for {played:.2} s: not at 4x");
    h.wait_speed(1.0);
    rig.h.shutdown();
}

/// The bus keeps `pulsesink` out of `autoaudiosink`'s choice, which a burst of
/// seeks while playing wedged on PipeWire 1.0's pulse server
/// (`keep_pulsesink_out`; `real_footage.rs` reproduces it on real hardware).
#[test]
fn the_bus_keeps_pulsesink_out_of_the_speakers() {
    use gstreamer::prelude::*;
    gstreamer::init().unwrap();
    let Some(pulse) = gstreamer::Registry::get().lookup_feature("pulsesink") else {
        eprintln!("skipped: pulsesink is not installed (gstreamer1.0-pulseaudio)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let h = video_coach_harness::Harness::new(&tmp.path().join("config"));
    assert_eq!(pulse.rank(), gstreamer::Rank::NONE);
    h.shutdown();
}
