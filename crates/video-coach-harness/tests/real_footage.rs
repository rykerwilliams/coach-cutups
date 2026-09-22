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

use std::path::PathBuf;
use std::time::{Duration, Instant};

use video_coach_app::bus::{Command, Event};
use video_coach_core::project::{Project, SourceRef};
use video_coach_core::store;
use video_coach_harness::Harness;
use video_coach_media::probe;

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
    let footage: PathBuf = std::env::var_os("COACH_FOOTAGE")
        .expect("COACH_FOOTAGE names a real video file with an audio track")
        .into();
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("project");
    std::fs::create_dir(&folder).unwrap();
    let p = probe(&footage).expect("probe the footage");
    assert!(p.duration_seconds > 60.0, "needs a minute of footage");
    let mut project = Project::new("Game");
    project.source_videos.push(SourceRef {
        // Absolute: `join` keeps it as is.
        relative_path: footage.to_string_lossy().into_owned(),
        display_name: "footage".into(),
        duration_seconds: p.duration_seconds,
        display_aspect: p.display_aspect,
    });
    store::write(&folder, &mut project).unwrap();

    let mut h = Harness::production(&tmp.path().join("config"));
    h.send(Command::OpenProject(folder));
    h.wait_opened();
    h.wait_settled();
    h.send(Command::SetVolume {
        value: 0.0,
        commit: false,
    });

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
