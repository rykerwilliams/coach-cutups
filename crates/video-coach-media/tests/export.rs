//! Export end to end through the real graph: the GPU where there is one,
//! llvmpipe on CI. Each export is at most ~90 frames, since llvmpipe takes
//! ~0.45 CPU-s per 1080p frame.
//!
//! The sources are counter fixtures, so every output frame is checked
//! against the schedule by the number it shows.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_pbutils as pbutils;
use uuid::Uuid;
use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::{frame_schedule, FrameSpec};
use video_coach_core::project::Clip;
use video_coach_core::zoom::Zoom;
use video_coach_media::fixtures::{
    self, block_centre, counter_video, decode_counters, read_counter, CounterKind, COUNTER_BITS,
};
use video_coach_media::{ExportDone, ExportError, ExportJob, ExportMessage, Exporter};

/// Far beyond any export here, even on a loaded llvmpipe runner; only a hang
/// reaches it.
const TIMEOUT: Duration = Duration::from_secs(120);

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
    let frames = job.frames.len();
    let (tx, rx) = mpsc::channel();
    let begun = Instant::now();
    let exporter = Exporter::start(job, move |msg| match msg {
        ExportMessage::Progress(p) => on_progress(p),
        ExportMessage::Finished(result) => {
            let _ = tx.send(result);
        }
    })
    .unwrap();
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
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
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

/// Width, height and frame rate of `path`'s video stream.
fn shape(path: &Path) -> (u32, u32, gst::Fraction) {
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
    (video.width(), video.height(), video.framerate())
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
    let done = export(ExportJob {
        source: src.path.clone(),
        frames,
        path: path.clone(),
    })
    .unwrap();
    assert_eq!(done.path, path);

    let got = decode_counters(&path);
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
    assert_eq!(shape(&path), (1920, 1080, gst::Fraction::new(30, 1)));
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
            source_time: 1.6,
            zoom,
        })
        .to_vec();
    let path = dir.path().join("out.mp4");
    export(ExportJob {
        source,
        frames,
        path: path.clone(),
    })
    .unwrap();

    let out = fixtures::decode_gray(&path);
    assert_eq!(out.len(), 3);
    // The fit rect of 4:3 in 1920×1080.
    let (fx, fw, fh) = (240.0, 1440.0, 1080.0);
    for frame in &out {
        assert_eq!((frame.width, frame.height), (1920, 1080));
        for x in [20, 120, 220, 1700, 1800, 1900] {
            for y in [20, 540, 1060] {
                let v = frame.mean(x, y, 8);
                assert!(v < 40.0, "bar pixel ({x}, {y}) is {v}, not black");
            }
        }
    }
    assert_eq!(read_counter(&out[0].crop(240, 0, 1440, 1080)), shown);

    // Zoomed: the source point (0.5 + pan) sits at the picture's centre, and
    // distances from it scale by s.
    let mut checked = 0;
    for bit in 0..COUNTER_BITS {
        let (u, v) = block_centre(bit);
        let x = fx + fw * (0.5 + (u - 0.5 - zoom.pan_x) * zoom.scale);
        let y = fh * (0.5 + (v - 0.5 - zoom.pan_y) * zoom.scale);
        // Only blocks whose centre is well inside the picture.
        if x < fx + 60.0 || x > fx + fw - 60.0 || !(60.0..fh - 60.0).contains(&y) {
            continue;
        }
        let lit = shown >> bit & 1 == 1;
        let level = out[1].mean(x as usize, y as usize, 10);
        assert_eq!(
            level > 128.0,
            lit,
            "bit {bit} at ({x:.0}, {y:.0}) reads {level:.0}, expected lit={lit}"
        );
        checked += 1;
    }
    assert_eq!(checked, 3, "bits 3, 4 and 5 should be in view");
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
        ExportJob {
            source: src.path,
            frames,
            path: path.clone(),
        },
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
