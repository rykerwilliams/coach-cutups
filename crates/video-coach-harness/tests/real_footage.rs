//! The app's own playback path on real footage: the GL video sink fed by a
//! hardware decoder (VA into DMABufs on the reference laptop) and
//! `autoaudiosink` on the desktop's sound server. The generated WebM fixtures
//! the other tests use have no audio track and play through `fakesink`, so
//! nothing else here reaches the sound server.
//!
//! `#[ignore]`d: it needs a GPU, a sound server and a real video file of a
//! minute or more **with an audio track**, none of which CI has. Run it on
//! the reference laptop with
//!
//! ```text
//! COACH_FOOTAGE=/path/to/game.mp4 \
//!     cargo test -p video-coach-harness --test real_footage -- --ignored --nocapture
//! ```
//!
//! Checked on a phone's 1440p HEVC MP4 (`vah265dec`) and on a 1080p H.264
//! MP4 repackaged from an HLS stream (`vah264dec`), both with AAC audio,
//! locally and on a FUSE cloud mount. It plays the file on the speakers,
//! muted; the file is only read.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use video_coach_app::bus::{Command, Event};
use video_coach_harness::{round_trip, write_one_source_project, Harness, Landing};

/// The footage `COACH_FOOTAGE` names.
fn footage() -> PathBuf {
    std::env::var_os("COACH_FOOTAGE")
        .expect("COACH_FOOTAGE names a real video file with an audio track")
        .into()
}

/// Opens `folder` on `h` paused, with the speakers muted.
fn open_muted(h: &mut Harness, folder: &Path) {
    h.send(Command::OpenProject(folder.to_owned()));
    h.wait_opened();
    h.wait_settled();
    h.send(Command::SetVolume {
        value: 0.0,
        commit: false,
    });
}

/// Waits `ms`, receiving events meanwhile.
fn wait(h: &mut Harness, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    h.poll_until(&format!("{ms} ms to pass"), |_| Instant::now() >= until);
}

/// Asserts playback runs: the position moves on by most of a second in one.
fn assert_advancing(h: &mut Harness, after: &str) {
    wait(h, 500);
    let before = h.position_secs().expect("a position");
    wait(h, 1000);
    let now = h.position_secs().expect("a position");
    eprintln!("after {after}: {before:.3} -> {now:.3}");
    assert!(
        now - before > 0.7,
        "playback stalled after {after}: {before:.3} -> {now:.3} in a second"
    );
    let playing = h.log().iter().rev().find_map(|e| match e {
        Event::Playing(p) => Some(*p),
        _ => None,
    });
    assert_eq!(playing, Some(true), "after {after}");
}

/// Before `keep_pulsesink_out`, `autoaudiosink` was `pulsesink`, and on
/// PipeWire 1.0's pulse server a burst of flushing seeks while playing
/// wedged it for good: the drag below froze the picture and the clock at
/// its release, every run. A pause and a play first made it deterministic.
#[test]
#[ignore]
fn real_footage_keeps_playing_through_seeks_while_playing() {
    gstreamer::init().unwrap();
    let footage = footage();
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("project");
    std::fs::create_dir(&folder).unwrap();
    let project = write_one_source_project(&folder, &footage);
    assert!(
        project.source_videos[0].duration_seconds > 60.0,
        "needs a minute of footage"
    );

    let mut h = Harness::production(&tmp.path().join("config"));
    open_muted(&mut h, &folder);

    // A scrub while paused, played at once; then a pause and a play.
    h.send(Command::ScrubMove { abs: 20.0 });
    h.toggle_play();
    assert_advancing(&mut h, "a scrub while paused");
    h.toggle_play();
    wait(&mut h, 500);
    h.toggle_play();
    assert_advancing(&mut h, "a pause and a play");

    // A drag while playing: a move per frame for a second, then the release.
    for i in 0..60 {
        h.send(Command::ScrubMove {
            abs: 30.0 + f64::from(i) * 0.2,
        });
        std::thread::sleep(Duration::from_millis(16));
    }
    h.send(Command::ScrubRelease { abs: 42.0 });
    assert_advancing(&mut h, "a drag while playing");

    // A held skip key while playing (auto-repeat is ~30 Hz).
    for _ in 0..10 {
        h.skip(-3.0);
        std::thread::sleep(Duration::from_millis(33));
    }
    assert_advancing(&mut h, "a held skip key while playing");
    h.shutdown();
}

/// BACKLOG #67 and spec H6 on real footage: a scrub released while paused
/// lands within a frame of its target, and shows the frame export picks for
/// the position it reports. 20 targets across the whole file.
///
/// Twice: with the System sinks, and with the app's own (`production`), since
/// the position may come from `autoaudiosink`'s clock. Both runs are printed
/// side by side before either is checked.
#[test]
#[ignore]
fn real_footage_scrubs_land_on_the_frame_export_picks() {
    gstreamer::init().unwrap();
    let footage = footage();
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("project");
    std::fs::create_dir(&folder).unwrap();
    let project = write_one_source_project(&folder, &footage);
    let duration = project.source_videos[0].duration_seconds;
    let targets: Vec<f64> = (0..20)
        .map(|i| (f64::from(i) + 0.5) * duration / 20.0)
        .collect();

    let run = |mut h: Harness, sinks: &str| -> Vec<Landing> {
        eprintln!("{sinks} sinks:");
        open_muted(&mut h, &folder);
        let landed = round_trip(&mut h, &footage, &targets);
        h.shutdown();
        landed
    };
    let system = run(Harness::new(&tmp.path().join("system")), "System");
    let production = run(Harness::production(&tmp.path().join("app")), "production");

    // Per run: the reported position (off the target by), and the end of the
    // frame shown and of the frame export picks, which match when they are
    // the same frame.
    let cell = |l: &Landing| {
        let shown = l
            .displayed
            .1
            .map_or_else(|| "none".to_owned(), |t| format!("{t:.4}"));
        format!(
            "{:>10.4} ({:+.4}) {shown:>10} {:>10.4}",
            l.reported,
            l.reported - l.target,
            l.export.end
        )
    };
    let head = format!(
        "{:>10} ({:>7}) {:>10} {:>10}",
        "reported", "off", "shown end", "export end"
    );
    eprintln!("{:>10} | System: {head} | production: {head}", "target");
    for (s, p) in system.iter().zip(&production) {
        eprintln!(
            "{:>10.4} | System: {} | production: {}",
            s.target,
            cell(s),
            cell(p)
        );
    }
    for landing in system.iter().chain(&production) {
        landing.check();
    }
}
