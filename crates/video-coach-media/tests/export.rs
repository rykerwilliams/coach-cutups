//! Export end to end through the real graph: the GPU where there is one,
//! llvmpipe on CI.
//!
//! **720p, short entries.** llvmpipe composites a 1080p frame in 75 ms with
//! one pad and 92 ms with three (measured); the export ships three pads, so
//! the whole suite renders at 720p and keeps every target to a second or two.
//!
//! The sources are counter fixtures, so every output frame is checked against
//! the schedule by the number it shows.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_pbutils as pbutils;
use uuid::Uuid;
use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::{frame_schedule, Compilation, FrameSpec, OUTPUT_FPS};
use video_coach_core::layout::{bar_rect, pip_rect};
use video_coach_core::plan::{CompilationPlan, PlanEntry};
use video_coach_core::project::{Clip, Quality, Resolution};
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_core::zoom::Zoom;
use video_coach_media::fixtures::{
    self, block_centre, counter_video, counter_video_with, decode_counters, one_entry,
    read_counter, CounterKind, CounterQuirks, COUNTER_BITS,
};
use video_coach_media::{EntryMedia, ExportDone, ExportError, ExportJob, ExportMessage, Exporter};

/// Far beyond any export here, even on a loaded llvmpipe runner; only a hang
/// reaches it.
const TIMEOUT: Duration = Duration::from_secs(120);

/// The suite's output size (see the module comment).
const OUT_W: i32 = 1280;
const OUT_H: i32 = 720;

const BLUE: u32 = 0x0000_00ff;
const GREEN: u32 = 0x0000_ff00;

/// How far a channel may be from the colour that was encoded. I420, the
/// mixer's conversions and H.264 at QP 24 all move it a little, but nowhere
/// near the gap between the colours these tests use.
const TOLERANCE: i32 = 40;

/// A source fixture: 5 s of counter video.
struct Source {
    path: PathBuf,
    fps: u32,
    frames: u32,
}

fn source(dir: &Path, kind: CounterKind) -> Source {
    gst::init().unwrap();
    let (name, fps) = match kind {
        CounterKind::Vp8WebmWithAudio => ("src.webm", 25),
        CounterKind::H264Mp4BFrames => ("src.mp4", 60),
    };
    let frames = 5 * fps;
    let path = counter_video(&dir.join(name), 640, 360, fps, frames, kind);
    Source { path, fps, frames }
}

/// Runs `job` to its `Finished`, calling `on_progress` for each progress
/// message on the export thread.
fn export_with(
    job: ExportJob,
    mut on_progress: impl FnMut(u8) + Send + 'static,
    with_exporter: impl FnOnce(&Exporter),
) -> Result<ExportDone, ExportError> {
    let frames = job.compilation.frames.len();
    let (tx, rx) = mpsc::channel();
    let begun = Instant::now();
    let exporter = Exporter::start(job, move |msg| match msg {
        ExportMessage::Progress(p) => on_progress(p),
        ExportMessage::Finished(result) => {
            let _ = tx.send(result);
        }
    });
    with_exporter(&exporter);
    let result = rx.recv_timeout(TIMEOUT).expect("the export finished");
    eprintln!(
        "export: {frames} frames in {:.2} s via {:?}",
        begun.elapsed().as_secs_f64(),
        result.as_ref().map(|d| (&d.encoder, &d.diagnostics))
    );
    result
}

fn export(job: ExportJob) -> Result<ExportDone, ExportError> {
    export_with(job, |_| {}, |_| {})
}

fn clip(start: f64, duration: f64, events: Vec<CommentaryEvent>) -> Clip {
    Clip {
        id: Uuid::nil(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: start,
        recording_duration: duration,
        recording_filename: "c.mkv".into(),
        events,
        show_pip: false,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// A one-entry export of `frames` from `source`, with no picture-in-picture
/// and no text bar: the plain picture, which most of these tests are about.
fn job(source: PathBuf, frames: Vec<FrameSpec>, path: PathBuf) -> ExportJob {
    let clip = clip(0.0, frames.len() as f64 / f64::from(OUTPUT_FPS), Vec::new());
    ExportJob {
        compilation: one_entry(&clip, frames, ""),
        sources: vec![source],
        entries: vec![EntryMedia {
            // Unread: `show_pip` is off, so the pad takes the filler.
            recording: PathBuf::new(),
            clip,
        }],
        path,
        resolution: Resolution::R720,
        quality: Quality::Medium,
    }
}

/// Plays, a freeze, skips forward (near and far) and back, with every anchor
/// off frame boundaries at 25 and 60 fps, and identity zoom: 87 frames.
fn steered_clip() -> Clip {
    let event = |t: f64, kind: EventKind| CommentaryEvent::new(t, kind);
    clip(
        1.013,
        2.9,
        vec![
            event(
                0.8,
                EventKind::Pause {
                    source_time: 1.8137,
                },
            ),
            event(
                1.3,
                EventKind::Play {
                    source_time: 1.8137,
                },
            ),
            // Within the 0.5 s pull-ahead: pulled forward.
            event(1.6, EventKind::Skip { delta: 0.3 }),
            // Beyond it: an accurate seek.
            event(1.9, EventKind::Skip { delta: 1.2 }),
            // Backwards: a seek.
            event(2.4, EventKind::Skip { delta: -3.0 }),
        ],
    )
}

/// The frame a correct export shows for `source_time`: the last one with PTS
/// `floor(i·1e9/fps)` at or before `round(source_time·1e9)`, in integers.
fn oracle(source_time: f64, fps: u32, frames: u32) -> u32 {
    let target = (source_time * 1e9).round() as u128;
    let i = ((target + 1) * u128::from(fps) - 1) / 1_000_000_000;
    (i as u32).min(frames - 1)
}

/// Width, height and frame rate of `path`'s video stream, and the file's
/// duration in seconds.
fn shape(path: &Path) -> (u32, u32, gst::Fraction, f64) {
    let uri = gst::glib::filename_to_uri(path, None).unwrap();
    let info = pbutils::Discoverer::new(gst::ClockTime::from_seconds(10))
        .unwrap()
        .discover_uri(&uri)
        .unwrap();
    let video = info
        .video_streams()
        .into_iter()
        .next()
        .expect("a video stream");
    let duration = info.duration().expect("a duration").nseconds() as f64 / 1e9;
    (video.width(), video.height(), video.framerate(), duration)
}

/// The top-level MP4 box types, in file order.
fn top_level_boxes(path: &Path) -> Vec<String> {
    let data = std::fs::read(path).unwrap();
    let mut boxes = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        boxes.push(String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned());
        let size = match size {
            1 => u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) as usize,
            0 => data.len() - at,
            n => n,
        };
        at += size.max(8);
    }
    boxes
}

/// Asserts `got` is `expected` frame for frame, naming the first few that
/// differ.
fn counters_match(got: &[u32], expected: &[u32]) {
    let wrong: Vec<_> = (0..expected.len().max(got.len()))
        .filter(|&n| got.get(n) != expected.get(n))
        .map(|n| (n, got.get(n), expected.get(n)))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} frames wrong (frame, got, expected): {:?}",
        wrong.len(),
        expected.len(),
        &wrong[..wrong.len().min(10)]
    );
}

/// Asserts `path`'s duration is the schedule's, within a frame. `glvideomixer`
/// ignores `identity eos-after=N` and runs to the demuxer's segment end
/// instead, which is how a "600-frame" benchmark produced an unreadable
/// 88.9 s file: the duration is the assertion that catches it.
fn duration_is_the_schedule_s(path: &Path, frames: usize) {
    let (w, h, rate, duration) = shape(path);
    assert_eq!(
        (w, h, rate),
        (OUT_W as u32, OUT_H as u32, gst::Fraction::new(30, 1))
    );
    let expected = frames as f64 / f64::from(OUTPUT_FPS);
    assert!(
        (duration - expected).abs() <= 1.0 / f64::from(OUTPUT_FPS),
        "{duration} s of output for a {expected} s schedule"
    );
}

fn round_trip(kind: CounterKind) {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), kind);
    let counters = decode_counters(&src.path);
    assert_eq!(counters, (0..src.frames).collect::<Vec<_>>());
}

#[test]
fn a_vp8_counter_fixture_decodes_to_its_frame_numbers() {
    round_trip(CounterKind::Vp8WebmWithAudio);
}

#[test]
fn an_h264_counter_fixture_decodes_to_its_frame_numbers() {
    round_trip(CounterKind::H264Mp4BFrames);
}

/// The fixture must carry the edit-list trap, or the fiducial test on it
/// proves nothing about stream time.
#[test]
fn the_h264_fixture_has_an_edit_list() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), CounterKind::H264Mp4BFrames);
    let pipeline = gst::parse::launch("filesrc name=in ! qtdemux ! appsink name=sink sync=false")
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline
        .by_name("in")
        .unwrap()
        .set_property("location", &src.path);
    let sink = pipeline
        .by_name("sink")
        .and_downcast::<gst_app::AppSink>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let sample = sink
        .try_pull_sample(gst::ClockTime::from_seconds(10))
        .unwrap();
    let _ = pipeline.set_state(gst::State::Null);
    let start = sample
        .segment()
        .and_then(|s| s.downcast_ref::<gst::ClockTime>())
        .and_then(|s| s.start())
        .unwrap();
    assert!(start > gst::ClockTime::ZERO, "segment start {start}");
}

fn fiducial(kind: CounterKind) {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), kind);
    let frames = frame_schedule(&steered_clip(), f64::from(src.frames) / f64::from(src.fps));
    assert_eq!(frames.len(), 87);
    let expected: Vec<u32> = frames
        .iter()
        .map(|f| oracle(f.source_time, src.fps, src.frames))
        .collect();
    let path = dir.path().join("out.mp4");
    let done = export(job(src.path.clone(), frames, path.clone())).unwrap();
    assert_eq!(done.path, path);

    counters_match(&decode_counters(&path), &expected);
    duration_is_the_schedule_s(&path, expected.len());
    let boxes = top_level_boxes(&path);
    let position = |t: &str| boxes.iter().position(|b| b == t).unwrap();
    assert!(position("moov") < position("mdat"), "boxes: {boxes:?}");
    assert!(!dir.path().join("out.mp4.part").exists());
}

#[test]
fn a_vp8_export_shows_the_scheduled_frame_every_frame() {
    fiducial(CounterKind::Vp8WebmWithAudio);
}

/// Also the edit-list trap: raw PTS would put every frame two frames early.
#[test]
fn an_h264_export_shows_the_scheduled_frame_every_frame() {
    fiducial(CounterKind::H264Mp4BFrames);
}

/// Two clips of different sizes and frame rates, one after the other in one
/// file: every output frame shows the source frame its entry's schedule asked
/// for, read out of that entry's own rect.
///
/// That is the per-entry geometry, the caps change at the join and the
/// concatenation, in one assertion. It is **not** a reproduction of the
/// four-frames-early race: setting a rect from the pushing thread only lands
/// early while frames are still queued, and this schedule is short enough that
/// a machine keeping up drains between entries (checked: the test still passes
/// with the rect set eagerly). The race is measured in the spec; keying the
/// rect to the buffer's PTS is what removes it, and this test is what says the
/// keying itself is right.
#[test]
fn a_two_clip_export_shows_each_entry_s_frames_in_its_own_rect() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 16:9 at 25 fps, then 4:3 at 50 fps: the fit rect and the source's frame
    // rate both change at the join. Both rates divide 1000, since WebM's
    // timecodes are milliseconds and the oracle below counts in nanoseconds.
    let wide = counter_video(
        &dir.path().join("wide.webm"),
        640,
        360,
        25,
        60,
        CounterKind::Vp8WebmWithAudio,
    );
    let narrow = counter_video(
        &dir.path().join("narrow.webm"),
        480,
        360,
        50,
        60,
        CounterKind::Vp8WebmWithAudio,
    );
    let per_entry = 18;
    let clips = [clip(0.0, 0.6, Vec::new()), clip(0.0, 0.6, Vec::new())];
    let frames: Vec<FrameSpec> = (0..2 * per_entry)
        .map(|n| FrameSpec {
            entry: (n / per_entry) as usize,
            source_time: f64::from(n % per_entry) / f64::from(OUTPUT_FPS),
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let path = dir.path().join("out.mp4");
    export(ExportJob {
        compilation: two_entries(&clips, frames.clone(), per_entry as usize),
        sources: vec![wide, narrow],
        entries: clips
            .iter()
            .map(|clip| EntryMedia {
                recording: PathBuf::new(),
                clip: clip.clone(),
            })
            .collect(),
        path: path.clone(),
        resolution: Resolution::R720,
        quality: Quality::Medium,
    })
    .unwrap();

    // Entry 0 fills the frame; entry 1 is pillarboxed to (160, 0, 960, 720),
    // so its counter has to be read out of that rect.
    let out = fixtures::decode_gray(&path);
    assert_eq!(
        out.len(),
        frames.len(),
        "one output frame per schedule frame"
    );
    let got: Vec<u32> = out
        .iter()
        .enumerate()
        .map(|(n, frame)| {
            assert_eq!(
                (frame.width, frame.height),
                (OUT_W as usize, OUT_H as usize)
            );
            match n < per_entry as usize {
                true => read_counter(frame),
                false => read_counter(&frame.crop(160, 0, 960, 720)),
            }
        })
        .collect();
    let expected: Vec<u32> = frames
        .iter()
        .map(|f| match f.entry {
            0 => oracle(f.source_time, 25, 60),
            _ => oracle(f.source_time, 50, 60),
        })
        .collect();
    counters_match(&got, &expected);

    // The counter check above is the geometry check: entry 0's frames are read
    // out of the whole frame and entry 1's out of its pillarbox, so a rect
    // that arrived four frames early (the measured race) would make the last
    // frames of entry 0 unreadable. That only means anything if the two rects
    // really do read differently, which this pins.
    let last_of_entry_0 = &out[per_entry as usize - 1];
    assert_ne!(
        read_counter(&last_of_entry_0.crop(160, 0, 960, 720)),
        expected[per_entry as usize - 1],
        "the two entries' rects read the same, so the check above is vacuous"
    );

    duration_is_the_schedule_s(&path, frames.len());
}

/// A two-entry compilation of `frames`, `per_entry` frames each.
fn two_entries(clips: &[Clip; 2], frames: Vec<FrameSpec>, per_entry: usize) -> Compilation {
    let entry = |i: usize, clip: &Clip| PlanEntry {
        clip_id: clip.id,
        source_index: i,
        recording_filename: clip.recording_filename.clone(),
        show_pip: false,
        segments: Vec::new(),
        recording_duration: clip.recording_duration,
        start_frame: i * per_entry,
        frames: per_entry,
        text: String::new(),
    };
    Compilation {
        plan: CompilationPlan {
            total_duration_seconds: frames.len() as f64 / f64::from(OUTPUT_FPS),
            entries: vec![entry(0, &clips[0]), entry(1, &clips[1])],
        },
        frames,
    }
}

/// An export of `times` from `source` shows `expected`.
fn exports_as(source: PathBuf, times: &[f64], expected: &[u32]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.mp4");
    let frames = times
        .iter()
        .map(|&source_time| FrameSpec {
            entry: 0,
            source_time,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    export(job(source, frames, path.clone())).unwrap();
    assert_eq!(decode_counters(&path), expected);
}

/// A seek landing in a gap between frames (VFR, a dropped frame) answers
/// with the frame before it, although that frame's duration ends before the
/// target: an accurate seek drops it.
#[test]
fn a_seek_into_a_gap_shows_the_frame_before_it() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Frame 10 at 0.40 s lasts to 0.44 s; frame 11 is at 0.64 s, 12 at 0.68 s.
    let source = counter_video_with(
        &dir.path().join("src.webm"),
        640,
        360,
        25,
        50,
        CounterKind::Vp8WebmWithAudio,
        CounterQuirks {
            gap: Some((10, 5)),
            audio_tail: 0,
        },
    );
    exports_as(source, &[0.5, 0.5, 0.7], &[10, 10, 12]);
}

/// Past the video's end, where the audio runs on, the export shows the last
/// frame: an accurate seek there finds no video at all.
#[test]
fn a_seek_past_the_video_shows_its_last_frame() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 2 s of video, 3 s of audio.
    let source = counter_video_with(
        &dir.path().join("src.webm"),
        640,
        360,
        25,
        50,
        CounterKind::Vp8WebmWithAudio,
        CounterQuirks {
            gap: None,
            audio_tail: 25,
        },
    );
    exports_as(source, &[2.5, 2.6], &[49, 49]);
}

/// A 4:3 source is pillarboxed into black bars, and a zoom moves the counter
/// where the fit rect and the zoom mapping predict, clipped to the picture.
#[test]
fn a_4_3_source_is_pillarboxed_and_zoomed_as_predicted() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = counter_video(
        &dir.path().join("src.webm"),
        480,
        360,
        25,
        50,
        CounterKind::Vp8WebmWithAudio,
    );
    // Frame 40 = bits 3 and 5; bit 4 is off.
    let shown = 40;
    let zoom = Zoom::new(2.0, 0.2, -0.2);
    let frames = [Zoom::IDENTITY, zoom, zoom]
        .map(|zoom| FrameSpec {
            entry: 0,
            source_time: 1.6,
            zoom,
        })
        .to_vec();
    let path = dir.path().join("out.mp4");
    export(job(source, frames, path.clone())).unwrap();

    let out = fixtures::decode_gray(&path);
    assert_eq!(out.len(), 3);
    // The fit rect of 4:3 in 1280×720.
    let (fx, fw, fh) = (160.0, 960.0, 720.0);
    for frame in &out {
        assert_eq!(
            (frame.width, frame.height),
            (OUT_W as usize, OUT_H as usize)
        );
        for x in [15, 80, 145, 1135, 1200, 1265] {
            for y in [15, 360, 705] {
                let v = frame.mean(x, y, 8);
                assert!(v < 40.0, "bar pixel ({x}, {y}) is {v}, not black");
            }
        }
    }
    assert_eq!(read_counter(&out[0].crop(160, 0, 960, 720)), shown);

    // Zoomed: the source point (0.5 + pan) sits at the picture's centre, and
    // distances from it scale by s.
    let mut checked = 0;
    for bit in 0..COUNTER_BITS {
        let (u, v) = block_centre(bit);
        let x = fx + fw * (0.5 + (u - 0.5 - zoom.pan_x) * zoom.scale);
        let y = fh * (0.5 + (v - 0.5 - zoom.pan_y) * zoom.scale);
        // Only blocks whose centre is well inside the picture.
        if x < fx + 40.0 || x > fx + fw - 40.0 || !(40.0..fh - 40.0).contains(&y) {
            continue;
        }
        let lit = shown >> bit & 1 == 1;
        let level = out[1].mean(x as usize, y as usize, 6);
        assert_eq!(
            level > 128.0,
            lit,
            "bit {bit} at ({x:.0}, {y:.0}) reads {level:.0}, expected lit={lit}"
        );
        checked += 1;
    }
    assert_eq!(checked, 3, "bits 3, 4 and 5 should be in view");
}

/// A horizontal stroke across the picture at `y`, from `x = 0.2` to `x = 0.8`,
/// logged (as the recorder does) at pen-up.
fn stroke(y: f64, color: Rgba) -> CommentaryEvent {
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
        0.05,
        EventKind::Stroke(Stroke {
            id: Uuid::new_v4(),
            color,
            line_width: 0.05,
            points,
            auto_clear_after_seconds: None,
        }),
    )
}

/// The clip the layout test exports: a pillarboxed blue source with two
/// strokes on it, `show_pip` as given, and a line for the bar.
fn laid_out_job(dir: &Path, show_pip: bool) -> (ExportJob, PathBuf) {
    gst::init().unwrap();
    // 4:3, so the picture is pillarboxed to (160, 0, 960, 720) and a stroke
    // rasterized at the output size would land in the wrong place.
    let source = fixtures::solid_video(&dir.join("src.webm"), 640, 480, 30, 30, BLUE, false);
    let recording = fixtures::solid_video(&dir.join("rec.webm"), 640, 360, 30, 30, GREEN, true);
    let translucent = Rgba {
        a: 0.5,
        ..Rgba::RED
    };
    let clip = Clip {
        show_pip,
        events: vec![stroke(0.5, Rgba::RED), stroke(0.75, translucent)],
        ..clip(0.0, 0.2, Vec::new())
    };
    let frames = (0..6)
        .map(|_| FrameSpec {
            entry: 0,
            source_time: 0.2,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let path = dir.join(format!("out-{show_pip}.mp4"));
    (
        ExportJob {
            compilation: one_entry(&clip, frames, "1 / 2 | Demo"),
            sources: vec![source],
            entries: vec![EntryMedia { recording, clip }],
            path: path.clone(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        },
        path,
    )
}

/// Asserts the pixel at `(x, y)` is `expected` as `0xRRGGBB`, within
/// [`TOLERANCE`].
fn assert_rgb(frame: &fixtures::RgbFrame, what: &str, (x, y): (usize, usize), expected: u32) {
    let actual = frame.at(x, y);
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

/// The three pads land where `core::layout` says, in z-order: the source
/// pillarboxed at the bottom, the PiP above it and clear of the bar, and the
/// overlay — the strokes mapped into the picture, the bar over the whole
/// width — on top of both.
#[test]
fn the_export_stacks_the_picture_the_pip_and_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let (job, path) = laid_out_job(dir.path(), true);
    export(job).unwrap();
    let out = fixtures::decode_rgb(&path);
    let frame = out.last().expect("frames out");
    assert_eq!(
        (frame.width, frame.height),
        (OUT_W as usize, OUT_H as usize)
    );

    // The 4:3 source is pillarboxed: bars left and right, picture between.
    assert_rgb(frame, "the left bar", (80, 360), 0x000000);
    assert_rgb(frame, "the right bar", (1200, 100), 0x000000);
    assert_rgb(frame, "the picture", (300, 200), BLUE);

    // The PiP is the recording, flush to the right edge in output space --
    // overlapping the right pillarbox bar, which is the point of putting it
    // there -- and sitting ON the text bar rather than under it.
    let pip = pip_rect(f64::from(OUT_W), f64::from(OUT_H), 16.0 / 9.0);
    let bar = bar_rect(f64::from(OUT_W), f64::from(OUT_H));
    assert!(pip.y + pip.h <= bar.y, "the PiP overlaps the bar");
    let pip_centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    assert_rgb(frame, "the PiP", pip_centre, GREEN);
    assert_rgb(
        frame,
        "just left of the PiP",
        (pip.x as usize - 20, pip_centre.1),
        BLUE,
    );

    // The overlay's strokes are mapped into the picture rect, so the middle of
    // a stroke drawn at x = 0.5 is at 160 + 0.5*960, not 0.5*1280.
    assert_rgb(frame, "the stroke", (640, 360), 0xff3333);
    assert_rgb(frame, "past the stroke's end", (1000, 360), BLUE);
    assert_rgb(
        frame,
        "inside the left bar at the stroke's height",
        (80, 360),
        0,
    );

    // Premultiplied-over: red at half alpha over blue keeps half of the red.
    // With the source blend function left at `src-alpha` the red would be
    // halved twice, to about 64.
    let translucent = frame.at(640, 540);
    assert!(
        (100..=160).contains(&i32::from(translucent[0])),
        "the translucent stroke reads {translucent:?}; straight alpha would be ~64"
    );

    // The bar tints the bottom strip and nothing above it, and its glyphs are
    // the only thing in there brighter than the tint.
    let bar_mid = (bar.y + bar.h / 2.0) as usize;
    assert_rgb(frame, "the bar's tint", (900, bar_mid), 0x000066);
    assert_rgb(frame, "just above the bar", (900, bar.y as usize - 8), BLUE);
    let glyphs = (bar.y as usize..OUT_H as usize)
        .flat_map(|y| (0..640).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.at(x, y)[0] > 150)
        .count();
    assert!(glyphs > 50, "only {glyphs} glyph pixels in the bar");
}

/// With `show_pip` off the pad takes a 1×1 transparent filler, which is
/// invisible — and, being fed at all, is what keeps the mixer running: an
/// unfed pad produces no output frames whatever (measured).
#[test]
fn show_pip_off_leaves_the_inset_empty_and_the_export_running() {
    let dir = tempfile::tempdir().unwrap();
    let (job, path) = laid_out_job(dir.path(), false);
    let frames = job.compilation.frames.len();
    export(job).unwrap();
    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), frames, "the export produced no frames");
    let frame = out.last().expect("frames out");

    let pip = pip_rect(f64::from(OUT_W), f64::from(OUT_H), 16.0 / 9.0);
    let centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    // The inset's centre falls in the right pillarbox bar, so with no PiP it
    // is the mixer's black background.
    assert_rgb(frame, "where the PiP would be", centre, 0x000000);
    // And the picture is untouched.
    assert_rgb(frame, "the picture", (300, 200), BLUE);
}

/// Cancelling mid-export leaves no `.part`, and the file already at the path
/// untouched.
#[test]
fn cancel_leaves_nothing_and_keeps_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), CounterKind::Vp8WebmWithAudio);
    let path = dir.path().join("out.mp4");
    std::fs::write(&path, b"the previous export").unwrap();
    let frames = (0..60)
        .map(|n| FrameSpec {
            entry: 0,
            source_time: 1.0 + f64::from(n) / 30.0,
            zoom: Zoom::IDENTITY,
        })
        .collect();

    // The export thread stops in its progress callback until the test has
    // cancelled, so the cancel lands mid-export however fast the machine.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let mut stopped = false;
    let result = export_with(
        job(src.path, frames, path.clone()),
        move |percent| {
            if percent >= 30 && !stopped {
                stopped = true;
                let _ = reached_tx.send(percent);
                let _ = go_rx.recv_timeout(TIMEOUT);
            }
        },
        |exporter| {
            // An export that fails first is reported by the assert below.
            if reached_rx.recv_timeout(TIMEOUT).is_ok() {
                exporter.cancel();
                let _ = go_tx.send(());
            }
        },
    );
    assert_eq!(result, Err(ExportError::Cancelled));
    assert!(!dir.path().join("out.mp4.part").exists());
    assert_eq!(std::fs::read(&path).unwrap(), b"the previous export");
}
