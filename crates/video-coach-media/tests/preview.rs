//! The preview through the real composite graph: the GPU where there is one,
//! llvmpipe on CI. Short 720p clips, since the preview paces itself against
//! the clock and a test's wall time is its content's.
//!
//! Two things are checked here that nothing else covers: that the mixer puts
//! the PiP and the overlay where the layout says (Phase 5's fiducial already
//! covers the schedule and the pump, through this same code), and that a seek
//! lands on the frame it asked for with no frame from before it left behind.
//!
//! **No rate assertion.** The gate's 30 fps was measured on the reference
//! laptop (spec "Gates"); llvmpipe can't hold it and drops frames instead, so
//! these assert on content, not on timing.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use uuid::Uuid;
use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::frame_schedule;
use video_coach_core::layout::pip_rect;
use video_coach_core::project::Clip;
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_media::fixtures::{self, counter_video, read_counter, CounterKind, GrayFrame};
use video_coach_media::{
    Frame, FrameMailbox, Gl, Preview, PreviewJob, PreviewMessage, PreviewPosition, PreviewStats,
};

/// The composite's output size (`composite::preview`), which every pixel
/// assertion here is in.
const OUT_W: usize = 1280;
const OUT_H: usize = 720;
/// The schedule's frame rate, so a frame index is a time.
const FPS: f64 = 30.0;

/// Far beyond any preview here, even on a loaded llvmpipe runner; only a hang
/// reaches it.
const TIMEOUT: Duration = Duration::from_secs(120);
/// How long anything still in flight is given to land before the picture is
/// read. A frame arriving after this is one the graph should not have sent.
const SETTLE: Duration = Duration::from_millis(500);

/// How far a channel may be from the colour that was encoded: I420, VP8 and
/// the mixer's conversions all move it a little, but nowhere near the gap
/// between the colours these tests use.
const TOLERANCE: i32 = 32;

const BLUE: u32 = 0x0000_00ff;
const GREEN: u32 = 0x0000_ff00;

/// A clip with `events`, `duration` seconds of commentary from the source's
/// start, and no zoom or skips: output frame `n` is source frame `n`.
fn clip(duration: f64, show_pip: bool, events: Vec<CommentaryEvent>) -> Clip {
    Clip {
        id: Uuid::nil(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: 0.0,
        recording_duration: duration,
        recording_filename: "c.webm".into(),
        events,
        show_pip,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// A horizontal stroke across the middle of the picture at `y`, from `x = 0.2`
/// to `x = 0.8`, logged (as the recorder does) at pen-up.
fn bar(y: f64, color: Rgba) -> CommentaryEvent {
    let points = [0.2, 0.5, 0.8]
        .into_iter()
        .enumerate()
        .map(|(i, x)| StrokePoint {
            x,
            y,
            t: i as f64 * 0.05,
        })
        .collect();
    CommentaryEvent::new(
        0.1,
        EventKind::Stroke(Stroke {
            id: Uuid::new_v4(),
            color,
            line_width: 0.05,
            points,
            auto_clear_after_seconds: None,
        }),
    )
}

/// A preview running on the process's surfaceless context (spec P1: "no
/// private GL context" is an app rule, not a test rule).
struct Running {
    preview: Preview,
    mailbox: FrameMailbox,
    /// Where the preview is, as the bus's readout reads it: the owner holds
    /// it, as it holds the mailbox.
    position: PreviewPosition,
    messages: mpsc::Receiver<PreviewMessage>,
}

impl Running {
    fn start(source: PathBuf, recording: PathBuf, clip: Clip, source_duration: f64) -> Running {
        gst::init().unwrap();
        let frames = frame_schedule(&clip, source_duration);
        assert!(!frames.is_empty(), "the clip has no frames to preview");
        let job = PreviewJob {
            source,
            recording,
            clip,
            frames,
            commentary_volume: 0.0,
        };
        let mailbox = FrameMailbox::default();
        let position = PreviewPosition::default();
        let (tx, messages) = mpsc::channel();
        let preview = Preview::start(
            job,
            Gl::shared().expect("a surfaceless EGL context"),
            mailbox.clone(),
            position.clone(),
            move |msg| {
                let _ = tx.send(msg);
            },
        );
        Running {
            preview,
            mailbox,
            position,
            messages,
        }
    }

    /// The output frame the pump has reached, from [`Running::position`].
    fn frame(&self) -> f64 {
        self.position.seconds() * FPS
    }

    /// Waits until `cond` holds of the pump's frame, failing the test if the
    /// preview gives up meanwhile. It consumes the messages that arrive
    /// while it waits, so it doesn't mix with [`Running::play_out`].
    fn poll_frame(&self, what: &str, cond: impl Fn(f64) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !cond(self.frame()) {
            if let Ok(PreviewMessage::Failed(e)) = self.messages.try_recv() {
                panic!("the preview failed waiting for {what}: {e}");
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits for the schedule to run out, and reports what the sink saw.
    fn play_out(&self) -> PreviewStats {
        match self.messages.recv_timeout(TIMEOUT) {
            Ok(PreviewMessage::Ended) => self.preview.stats(),
            other => panic!("expected the schedule to end, got {other:?}"),
        }
    }

    /// The picture once everything in flight has landed: it waits for a frame,
    /// then keeps taking for [`SETTLE`] and returns the last one. A frame
    /// pushed from before a seek would be that last one.
    fn settled_picture(&self) -> Picture {
        let deadline = Instant::now() + TIMEOUT;
        let mut latest = self.mailbox.take();
        while latest.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            latest = self.mailbox.take();
        }
        let mut latest = latest.expect("a composited frame reached the mailbox");
        let until = Instant::now() + SETTLE;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
            if let Some(frame) = self.mailbox.take() {
                latest = frame;
            }
        }
        Picture::from(&latest)
    }
}

/// One composited frame's pixels. The mailbox hands over GL memory; mapping it
/// for reading downloads it, which is what a test wants.
struct Picture {
    width: usize,
    height: usize,
    /// RGBA, tightly packed.
    data: Vec<u8>,
}

impl Picture {
    fn from(frame: &Frame) -> Picture {
        let info = &frame.info;
        assert_eq!(info.format(), gst_video::VideoFormat::Rgba);
        let mapped = gst_video::VideoFrameRef::from_buffer_ref_readable(&frame.buffer, info)
            .expect("a composited frame maps for reading");
        let (width, height) = (info.width() as usize, info.height() as usize);
        let stride = mapped.plane_stride()[0] as usize;
        let plane = mapped.plane_data(0).expect("RGBA has one plane");
        Picture {
            width,
            height,
            data: plane
                .chunks(stride)
                .take(height)
                .flat_map(|row| row[..width * 4].iter().copied())
                .collect(),
        }
    }

    fn at(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        self.data[i..i + 4].try_into().expect("four channels")
    }

    /// Asserts the pixel at `(x, y)` is `expected` as `0xRRGGBB`, within
    /// [`TOLERANCE`]. `what` names the place in a failure.
    fn assert_rgb(&self, what: &str, (x, y): (usize, usize), expected: u32) {
        let actual = self.at(x, y);
        let want = [
            (expected >> 16) as u8,
            (expected >> 8) as u8,
            expected as u8,
        ];
        let off = (0..3).any(|c| (i32::from(actual[c]) - i32::from(want[c])).abs() > TOLERANCE);
        assert!(
            !off,
            "{what} at ({x}, {y}): expected #{expected:06x}, got {actual:?}"
        );
    }

    /// The red channel as luma, for [`read_counter`]: the counter fixture is
    /// black and white, so any channel reads it.
    fn as_gray(&self) -> GrayFrame {
        GrayFrame {
            width: self.width,
            height: self.height,
            data: self.data.iter().step_by(4).copied().collect(),
        }
    }
}

/// The mixer lays the three pads out as `core::layout` says: the source
/// pillarboxed, the overlay over the *picture* and not the output, and the
/// PiP as chrome in output space. And the overlay blends premultiplied.
#[test]
fn the_composite_places_the_pip_and_the_overlay_on_the_picture() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 4:3, so the picture is pillarboxed to (160, 0, 960, 720) and a stroke
    // rasterized at the output size would land in the wrong place.
    let source = fixtures::solid_video(&dir.path().join("src.webm"), 640, 480, 30, 90, BLUE, false);
    let recording =
        fixtures::solid_video(&dir.path().join("rec.webm"), 640, 360, 30, 90, GREEN, true);
    let translucent = Rgba {
        a: 0.5,
        ..Rgba::RED
    };
    let clip = clip(1.0, true, vec![bar(0.5, Rgba::RED), bar(0.75, translucent)]);

    let running = Running::start(source, recording, clip, 3.0);
    running.play_out();
    let picture = running.settled_picture();
    drop(running);
    assert_eq!((picture.width, picture.height), (OUT_W, OUT_H));

    // The 4:3 source is pillarboxed: bars left and right, picture between.
    picture.assert_rgb("the left bar", (80, 360), 0x000000);
    picture.assert_rgb("the right bar", (1200, 100), 0x000000);
    picture.assert_rgb("the picture", (300, 200), BLUE);

    // The PiP is the recording, flush to the bottom-right in output space --
    // overlapping the right bar, which is the point of putting it there.
    let pip = pip_rect(OUT_W as f64, OUT_H as f64, 16.0 / 9.0);
    let pip_centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    picture.assert_rgb("the PiP", pip_centre, GREEN);
    picture.assert_rgb(
        "just left of the PiP",
        (pip.x as usize - 20, pip_centre.1),
        BLUE,
    );

    // The overlay is normalized to the picture rect, so the middle of a stroke
    // drawn at x = 0.5 is at 160 + 0.5*960, not 0.5*1280.
    picture.assert_rgb("the stroke", (640, 360), 0xff3333);
    picture.assert_rgb("past the stroke's end", (1000, 360), BLUE);
    picture.assert_rgb("inside the left bar at the stroke's height", (80, 360), 0);

    // Premultiplied-over: red at half alpha over blue is half of each. With
    // the source blend function left at `src-alpha` the red would be halved
    // twice, to about 64.
    picture.assert_rgb("the translucent stroke", (640, 540), 0x7f197f);
}

/// A seek lands on the frame it asked for, and nothing from before it
/// survives: a push that crosses the flush is accepted silently, so the
/// pump's generation check is the only thing stopping it.
#[test]
fn a_seek_lands_on_the_frame_it_asked_for() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 16:9, so the picture fills the output and the counter can be read off
    // the composite directly.
    let source = counter_video(
        &dir.path().join("src.webm"),
        640,
        360,
        30,
        150,
        CounterKind::Vp8WebmWithAudio,
    );
    let recording =
        fixtures::solid_video(&dir.path().join("rec.webm"), 640, 360, 30, 150, GREEN, true);
    let total = 120;
    let running = Running::start(
        source,
        recording,
        clip(total as f64 / FPS, false, Vec::new()),
        5.0,
    );

    // Held, so the picture is whatever the last seek prerolled -- which is
    // how the scrubber is dragged (spec P3).
    running.preview.set_playing(false);
    let last_seek = 90;
    for target in [45u32, 10, last_seek] {
        running.preview.seek(f64::from(target) / FPS);
        let counter = read_counter(&running.settled_picture().as_gray());
        assert_eq!(
            counter, target,
            "the seek to frame {target} landed on {counter}"
        );
    }

    // ... and the pump picks up from there and runs to the end of the
    // schedule, where the picture freezes. *Which* of the last frames is held
    // isn't asserted: the sink drops a late one rather than let the picture
    // slide behind the words, and an unoptimized build on llvmpipe drops a
    // third of them.
    running.preview.set_playing(true);
    running.play_out();
    let held = read_counter(&running.settled_picture().as_gray());
    assert!(
        (last_seek..total).contains(&held),
        "the preview ended holding frame {held}, not one from after the seek"
    );
    // The position reads the end of the clip (spec P3).
    let position = running.position.seconds();
    assert!(
        (position - f64::from(total) / FPS).abs() < 1e-9,
        "the position ended at {position} s"
    );

    // And a scrub back out of that freeze still works: the appsrcs were sent
    // EOS to flush the tail, and a flushing seek has to undo it.
    running.preview.seek(f64::from(last_seek) / FPS);
    let counter = read_counter(&running.settled_picture().as_gray());
    assert_eq!(
        counter, last_seek,
        "a seek out of the end landed on {counter}"
    );
}

/// A seek while the end-of-schedule tail is draining leaves the preview
/// running: it used to wedge the pump, which waited out its stall timeout for
/// frames the flush had thrown away and then failed the preview.
#[test]
fn a_seek_while_the_tail_drains_keeps_the_preview_running() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let frames = 45;
    let source = counter_video(
        &dir.path().join("src.webm"),
        640,
        360,
        30,
        90,
        CounterKind::Vp8WebmWithAudio,
    );
    let recording = fixtures::solid_video(
        &dir.path().join("rec.webm"),
        640,
        360,
        30,
        frames,
        GREEN,
        true,
    );
    let total = f64::from(frames);
    let running = Running::start(source, recording, clip(total / FPS, false, Vec::new()), 3.0);

    // The pump has pushed the last frame of the schedule, so it is in the
    // drain -- or just past it, which the seek must also survive.
    running.poll_frame("the end of the schedule", |n| n >= total - 1.0);
    running.preview.seek(0.0);
    // A drain that finished first paused the preview on its last frame.
    running.preview.set_playing(true);

    // It picks the schedule up from the seek and composes it again, rather
    // than failing with "the preview stopped composing frames".
    running.poll_frame("the seek to take", |n| n < total / 2.0);
    running.poll_frame("the schedule a second time", |n| n >= total - 1.0);
}
