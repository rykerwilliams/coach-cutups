//! Headless player tests: a system-memory video sink, `fakesink sync=true`
//! audio, and the Task 2 fixtures. Assertions use the mailbox frame's PTS and
//! size, never its pixel format, which depends on the decoder the machine
//! picks.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use crate::fixtures;

const FRAME: f64 = 1.0 / 30.0;
/// Generous: every wait here normally finishes in milliseconds, except EOS,
/// which plays a one-second fixture in real time.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Stands in for the bus thread: owns the player, receives its forwarded
/// messages, and logs every event.
struct Rig {
    player: SourcePlayer,
    mailbox: FrameMailbox,
    rx: mpsc::Receiver<gst::Message>,
    log: Vec<PlayerEvent>,
    /// Events before this index have been consumed by `wait_for`.
    cursor: usize,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        gst::init().unwrap();
        let (tx, rx) = mpsc::channel();
        let mailbox = FrameMailbox::default();
        let player = SourcePlayer::new(SinkKind::System, mailbox.clone(), move |m| {
            let _ = tx.send(m);
        });
        Rig {
            player,
            mailbox,
            rx,
            log: Vec::new(),
            cursor: 0,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// A 30 fps WebM fixture of `secs` seconds at `w`×`h`, as a URI.
    fn fixture(&self, name: &str, secs: u32, w: u32, h: u32) -> String {
        uri(&fixtures::webm(self.dir.path(), name, secs, w, h, 30, 15))
    }

    fn seek(&mut self, uri: &str, secs: f64, accurate: bool, origin: Origin) -> Vec<PlayerEvent> {
        let events = self.player.seek_to(uri, secs, accurate, origin);
        self.log.extend(events.iter().cloned());
        events
    }

    /// Feeds forwarded messages to the player until an unconsumed event
    /// matches `pred`, and consumes everything up to and including it.
    fn wait_for(&mut self, what: &str, pred: impl Fn(&PlayerEvent) -> bool) -> PlayerEvent {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(i) = self.log[self.cursor..].iter().position(&pred) {
                let event = self.log[self.cursor + i].clone();
                self.cursor += i + 1;
                return event;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(msg) => {
                    let events = self.player.handle(&msg);
                    self.log.extend(events);
                }
                Err(_) => panic!(
                    "timed out waiting for {what}; unconsumed events: {:?}",
                    &self.log[self.cursor..]
                ),
            }
        }
    }

    fn wait_done(&mut self, origin: Origin) {
        self.wait_for("SeekDone", |e| *e == PlayerEvent::SeekDone { origin });
    }

    /// Feeds messages for `period` and returns the events it produced, plus
    /// any not yet consumed.
    fn drain(&mut self, period: Duration) -> Vec<PlayerEvent> {
        let deadline = Instant::now() + period;
        while let Ok(msg) = self
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            let events = self.player.handle(&msg);
            self.log.extend(events);
        }
        let rest = self.log[self.cursor..].to_vec();
        self.cursor = self.log.len();
        rest
    }

    /// PTS in seconds and size of the newest delivered frame.
    fn frame(&self) -> (f64, u32, u32) {
        let frame = self.mailbox.take().expect("a frame was delivered");
        let pts = frame.buffer.pts().expect("frame has a PTS");
        (seconds(pts), frame.info.width(), frame.info.height())
    }

    fn load(&mut self, uri: &str, secs: f64) {
        self.seek(uri, secs, true, Origin::System);
        self.wait_done(Origin::System);
    }
}

fn uri(path: &Path) -> String {
    gst::glib::filename_to_uri(path, None).unwrap().to_string()
}

/// `pts` is the frame on screen at `target`: it starts at most one frame
/// before it.
fn assert_lands(pts: f64, target: f64) {
    assert!(
        pts <= target + 1e-6 && target - pts < FRAME + 1e-6,
        "frame at {pts} for target {target}"
    );
}

#[test]
fn load_prerolls_the_target_frame_and_reports_diagnostics() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    assert!(rig.seek(&a, 0.0, true, Origin::System).is_empty());

    let loaded = rig.wait_for("Loaded", |e| matches!(e, PlayerEvent::Loaded { .. }));
    let PlayerEvent::Loaded { diagnostics } = loaded else {
        unreachable!()
    };
    assert!(diagnostics.decoder.is_some(), "{diagnostics:?}");
    assert_eq!(diagnostics.glupload_caps, None, "no GL with a system sink");
    assert_eq!(diagnostics.gl_platform, None, "no GL with a system sink");

    rig.wait_done(Origin::System);
    let (pts, w, h) = rig.frame();
    assert_lands(pts, 0.0);
    assert_eq!((w, h), (320, 180));
}

#[test]
fn accurate_seek_lands_within_one_frame() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 0.0);
    for target in [1.234, 0.4, 1.9] {
        assert!(rig.seek(&a, target, true, Origin::Skip).is_empty());
        rig.wait_done(Origin::Skip);
        assert_lands(rig.frame().0, target);
        let position = rig.player.position_handle().query_position().unwrap();
        assert!((position - target).abs() < FRAME, "position {position}");
    }
}

#[test]
fn latest_request_wins_and_the_displaced_one_is_reported() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 0.0);

    assert!(rig.seek(&a, 0.5, true, Origin::Skip).is_empty());
    assert!(rig.seek(&a, 1.0, true, Origin::Scrub).is_empty());
    assert_eq!(
        rig.seek(&a, 1.5, true, Origin::System),
        vec![PlayerEvent::SeekDisplaced {
            origin: Origin::Scrub
        }]
    );

    // Two flights: the first request, then the latest. The middle one is
    // never issued.
    let events = rig.drain(Duration::from_millis(500));
    let dones: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, PlayerEvent::SeekDone { .. }))
        .collect();
    assert_eq!(
        dones,
        [
            &PlayerEvent::SeekDone {
                origin: Origin::Skip
            },
            &PlayerEvent::SeekDone {
                origin: Origin::System
            }
        ],
        "{events:?}"
    );
    assert_lands(rig.frame().0, 1.5);
}

#[test]
fn each_flight_completes_exactly_once() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 0.0);
    for (target, accurate) in [(0.3, true), (1.1, false), (1.7, true), (0.9, false)] {
        rig.seek(&a, target, accurate, Origin::Scrub);
        rig.wait_done(Origin::Scrub);
    }
    // Nothing late: no second completion for any flight.
    let late = rig.drain(Duration::from_millis(500));
    assert!(late.is_empty(), "{late:?}");
}

#[test]
fn cross_source_load_lands_on_its_target_with_the_new_size() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    let b = rig.fixture("b.webm", 2, 320, 240);
    rig.load(&a, 0.5);
    assert_eq!(rig.frame().1, 320);

    rig.seek(&b, 1.0, true, Origin::Skip);
    rig.wait_for("Loaded", |e| matches!(e, PlayerEvent::Loaded { .. }));
    rig.wait_done(Origin::Skip);
    let (pts, w, h) = rig.frame();
    assert_lands(pts, 1.0);
    assert_eq!((w, h), (320, 240));

    // And back, to a target in the first source.
    rig.seek(&a, 1.5, true, Origin::Skip);
    rig.wait_done(Origin::Skip);
    let (pts, w, h) = rig.frame();
    assert_lands(pts, 1.5);
    assert_eq!((w, h), (320, 180));
}

#[test]
fn seek_in_the_final_second_stays_in_the_source() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 0.0);
    // `total − 0.05` is the bus's clamp (spec D8).
    for target in [1.5, 1.95] {
        rig.seek(&a, target, true, Origin::Skip);
        rig.wait_done(Origin::Skip);
        let (pts, ..) = rig.frame();
        assert_lands(pts, target);
        assert!(pts < 2.0, "frame at {pts} is past the end");
    }
    let rest = rig.drain(Duration::from_millis(300));
    assert!(!rest.contains(&PlayerEvent::Eos), "{rest:?}");
}

#[test]
fn eos_arrives_at_the_end_and_playing_waits_for_the_load() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 1, 320, 180);
    rig.seek(&a, 0.0, true, Origin::System);
    rig.player.set_playing(true);
    // Not applied mid-load: the pipeline is still heading for PAUSED.
    let (_, current, pending) = rig.player.pipeline.state(gst::ClockTime::ZERO);
    assert_ne!(current, gst::State::Playing);
    assert_ne!(pending, gst::State::Playing);

    rig.wait_done(Origin::System);
    rig.wait_for("Eos", |e| *e == PlayerEvent::Eos);
}

#[test]
fn eos_is_ignored_while_a_flight_is_busy() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 0.0);
    rig.seek(&a, 1.0, true, Origin::Skip);
    // The seek's ASYNC_DONE hasn't been handled yet, so the flight is busy.
    let stale = gst::message::Eos::new();
    assert!(rig.player.handle(&stale).is_empty());
    rig.wait_done(Origin::Skip);
    assert_eq!(rig.player.handle(&stale), vec![PlayerEvent::Eos]);
}

#[test]
fn a_failed_load_does_not_wedge_the_slot() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    let missing = uri(&rig.dir.path().join("missing.webm"));

    // Where the failure surfaces is GStreamer's choice: `filesrc` fails the
    // PAUSED transition synchronously (SeekFailed) and posts its errors on
    // the way. Handle everything queued, as the bus would before its next
    // command, then check both were reported.
    rig.seek(&missing, 0.0, true, Origin::Skip);
    let events = rig.drain(Duration::from_millis(300));
    assert!(
        events.contains(&PlayerEvent::SeekFailed {
            origin: Origin::Skip
        }) || events.iter().any(|e| matches!(e, PlayerEvent::Error(_))),
        "{events:?}"
    );

    rig.seek(&a, 1.0, true, Origin::Skip);
    rig.wait_done(Origin::Skip);
    assert_lands(rig.frame().0, 1.0);
}

#[test]
fn a_rejected_seek_fails_without_wedging_the_slot() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 0.0);
    // Pull the media out from under the player: a seek on a READY pipeline
    // is rejected.
    rig.player.pipeline.set_state(gst::State::Ready).unwrap();
    assert_eq!(
        rig.seek(&a, 1.0, true, Origin::Scrub),
        vec![PlayerEvent::SeekFailed {
            origin: Origin::Scrub
        }]
    );
    // The slot is free: the next request is issued, not queued.
    rig.player.pipeline.set_state(gst::State::Paused).unwrap();
    rig.drain(Duration::from_millis(300));
    rig.seek(&a, 1.0, true, Origin::Skip);
    rig.wait_done(Origin::Skip);
    assert_lands(rig.frame().0, 1.0);
}

#[test]
fn unload_drops_the_source_and_its_frame_and_nothing_plays() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 1.0);
    // Leave the frame in the mailbox, and a request pending behind a flight.
    rig.seek(&a, 0.5, true, Origin::Skip);
    rig.seek(&a, 1.5, true, Origin::Scrub);

    rig.player.unload();
    assert!(rig.mailbox.take().is_none(), "a stale frame remains");
    assert_eq!(rig.player.pipeline.current_state(), gst::State::Ready);
    assert!(matches!(rig.player.flight, Flight::Idle));
    assert!(rig.player.pending.is_none());

    // Play is refused with nothing loaded, and the dropped requests never
    // complete.
    rig.player.set_playing(true);
    let late = rig.drain(Duration::from_millis(300));
    assert!(
        !late
            .iter()
            .any(|e| matches!(e, PlayerEvent::SeekDone { .. })),
        "{late:?}"
    );
    assert_eq!(rig.player.pipeline.current_state(), gst::State::Ready);
    assert!(rig.mailbox.take().is_none());

    // The same source loads again (it isn't taken as still loaded), and the
    // recorded play is applied once it has.
    rig.seek(&a, 1.0, true, Origin::System);
    rig.wait_for("Loaded", |e| matches!(e, PlayerEvent::Loaded { .. }));
    rig.wait_done(Origin::System);
    rig.wait_for("Eos", |e| *e == PlayerEvent::Eos);
}

#[test]
fn a_seek_right_after_a_pause_is_not_completed_by_the_pause() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 4, 320, 180);
    rig.load(&a, 0.0);
    // Whether the pause's ASYNC_DONE beats the seek's flush is a race, so
    // several rounds.
    for target in [2.5, 1.0, 3.0, 0.5] {
        rig.player.set_playing(true);
        rig.drain(Duration::from_millis(150));

        // Nothing drained in between: the pause's own ASYNC_DONE is still
        // to come when the seek is requested.
        rig.player.set_playing(false);
        rig.seek(&a, target, true, Origin::Skip);
        assert_eq!(rig.player.target_secs(), Some(target));
        rig.wait_done(Origin::Skip);

        let position = rig.player.position_handle().query_position();
        assert!(
            position.is_some_and(|p| (p - target).abs() < FRAME),
            "position {position:?} for target {target}"
        );
        assert_lands(rig.frame().0, target);
        assert!(rig.player.is_idle());
        assert_eq!(rig.player.target_secs(), None);
    }
    let late = rig.drain(Duration::from_millis(300));
    assert!(late.is_empty(), "{late:?}");
}

#[test]
fn a_pause_keeps_the_displayed_frame() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 4, 320, 180);
    rig.load(&a, 0.0);
    rig.player.set_playing(true);
    rig.drain(Duration::from_millis(1000));

    // Empty the slot, so anything in it afterwards came from the pause.
    rig.mailbox.take();
    rig.player.set_playing(false);
    let (settled, current, _) = rig.player.pipeline.state(gst::ClockTime::from_seconds(5));
    assert_eq!(
        (settled, current),
        (Ok(gst::StateChangeSuccess::Success), gst::State::Paused)
    );
    rig.drain(Duration::from_millis(200));

    // PLAYING→PAUSED prerolls the frame after the displayed one; the position
    // stays on the displayed one, so that preroll must not be shown.
    let position = rig.player.position_handle().query_position().unwrap();
    if let Some(frame) = rig.mailbox.take() {
        let pts = seconds(frame.buffer.pts().expect("frame has a PTS"));
        let dur = frame.buffer.duration().map_or(FRAME, seconds);
        assert!(
            pts <= position + 1e-6 && position < pts + dur,
            "frame [{pts}, {}) does not cover position {position}",
            pts + dur
        );
    }
}

#[test]
fn a_seek_while_playing_completes_and_playback_continues() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 4, 320, 180);
    rig.load(&a, 0.0);
    rig.player.set_playing(true);
    rig.drain(Duration::from_millis(200));

    rig.seek(&a, 1.0, true, Origin::Skip);
    rig.wait_done(Origin::Skip);
    rig.drain(Duration::from_millis(400));
    let position = rig.player.position_handle().query_position().unwrap();
    assert!(position > 1.2, "playback stalled at {position}");
    assert_eq!(rig.player.pipeline.current_state(), gst::State::Playing);

    // Not wedged: the next request is issued at once and completes.
    assert!(rig.player.is_idle());
    rig.seek(&a, 0.5, true, Origin::Scrub);
    rig.wait_done(Origin::Scrub);
}

#[test]
fn a_pause_while_a_flushing_seek_recovers_playing_sticks() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 4, 320, 180);
    rig.load(&a, 0.0);
    for target in [2.5, 1.0, 3.0] {
        rig.player.set_playing(true);
        rig.drain(Duration::from_millis(150));

        // A flushing seek in PLAYING drops the pipeline to PAUSED, pending
        // PAUSED, and it returns to PLAYING by itself once prerolled. Issued
        // behind the slot's back, so the pause meets that window with the
        // slot idle: its only chance to stop the return.
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        let position = super::seconds_to_clock(target);
        rig.player.pipeline.seek_simple(flags, position).unwrap();
        rig.player.set_playing(false);
        rig.drain(Duration::from_millis(300));
        assert!(rig.player.is_idle());

        assert_eq!(
            rig.player.pipeline.state(gst::ClockTime::ZERO),
            (
                Ok(gst::StateChangeSuccess::Success),
                gst::State::Paused,
                gst::State::VoidPending
            )
        );
        let before = rig.player.position_handle().query_position().unwrap();
        rig.drain(Duration::from_millis(300));
        let after = rig.player.position_handle().query_position().unwrap();
        assert!(
            (after - target).abs() < FRAME && (after - before).abs() < FRAME,
            "played on from {target}: {before} then {after}"
        );
    }
}

#[test]
fn clear_during_a_seek_settles_before_the_next_request() {
    let mut rig = Rig::new();
    let a = rig.fixture("a.webm", 2, 320, 180);
    rig.load(&a, 0.0);

    for (dropped, target) in [(1.0, 1.5), (0.2, 0.8), (1.8, 0.4)] {
        rig.seek(&a, dropped, true, Origin::Scrub);
        rig.player.clear();
        assert!(matches!(rig.player.flight, Flight::Settling));
        assert_eq!(rig.player.target_secs(), None);
        assert!(rig.player.holds(&a), "clear keeps a landed source");

        // Waits for the dropped seek's ASYNC_DONE, then is issued.
        assert!(rig.seek(&a, target, true, Origin::Skip).is_empty());
        assert_eq!(rig.player.target_secs(), Some(target));
        rig.wait_done(Origin::Skip);
        assert_lands(rig.frame().0, target);
    }

    // The dropped seek was never reported.
    assert!(
        !rig.log.contains(&PlayerEvent::SeekDone {
            origin: Origin::Scrub
        }),
        "{:?}",
        rig.log
    );
    let late = rig.drain(Duration::from_millis(300));
    assert!(late.is_empty(), "{late:?}");
}

#[test]
fn volume_is_the_cube_of_the_slider() {
    let rig = Rig::new();
    let volume = || rig.player.pipeline.property::<f64>("volume");
    for (slider, expected) in [(0.0, 0.0), (0.5, 0.125), (0.8, 0.512), (1.0, 1.0)] {
        rig.player.set_volume(slider);
        assert!((volume() - expected).abs() < 1e-9, "{slider}: {}", volume());
    }
    rig.player.set_volume(2.0);
    assert_eq!(volume(), 1.0);
    rig.player.set_volume(f64::NAN);
    assert_eq!(volume(), 0.0);
}

#[test]
fn a_gl_sink_holds_the_pipeline_in_null_until_the_context_arrives() {
    gst::init().unwrap();
    if gst::ElementFactory::find("glupload").is_none() {
        eprintln!("skipped: glupload is not installed (gstreamer1.0-gl)");
        return;
    }
    let mut player = SourcePlayer::new(SinkKind::Gl, FrameMailbox::default(), |_| {});
    let dir = tempfile::tempdir().unwrap();
    let a = uri(&fixtures::webm(dir.path(), "a.webm", 1, 320, 180, 30, 15));

    assert!(player.seek_to(&a, 0.0, true, Origin::System).is_empty());
    player.set_playing(true);
    assert_eq!(player.pipeline.current_state(), gst::State::Null);
    assert_eq!(player.pipeline.pending_state(), gst::State::VoidPending);
    assert!(matches!(player.flight, Flight::Loading(_)));
}

#[test]
fn frame_boundaries_round_trip_to_the_exact_nanosecond() {
    // Truncation put 1.7-1.9% of k/30 boundaries 1 ns early.
    for k in 0..3000u64 {
        let secs = k as f64 / 30.0;
        let exact = k * 1_000_000_000 / 30;
        let got = super::seconds_to_clock(secs).nseconds();
        assert!(
            got.abs_diff(exact) <= 1 && got >= exact,
            "frame {k}: {got} vs {exact}"
        );
    }
}
