//! Bus end to end: transport (spec D4, D8) — skip bursts over the concat
//! timeline, scrub release, EOS advance and the end-of-source clamp.
//!
//! Positions are checked against the pipeline's own position query plus the
//! bus's latest published source index, and waited for by polling with a
//! timeout. Fixtures are short so EOS tests play out in about a second.

use tempfile::TempDir;
use video_coach_app::bus::{Command, Event};
use video_coach_core::project::Project;
use video_coach_harness::{write_project, Harness};

const FRAME: f64 = 1.0 / 30.0;

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
