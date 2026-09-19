//! The pure pieces of export, and a mid-stream failure, which needs the
//! private injection hook. The graph end to end is in `tests/export.rs`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use video_coach_core::zoom::Zoom;

use super::encode::{choose_encoder, fit_rect, zoom_params};
use super::*;
use crate::fixtures::{self, CounterKind};

fn info(w: u32, h: u32, par: (i32, i32)) -> gstreamer_video::VideoInfo {
    gst::init().unwrap();
    gstreamer_video::VideoInfo::builder(gstreamer_video::VideoFormat::Rgba, w, h)
        .par(gst::Fraction::new(par.0, par.1))
        .build()
        .unwrap()
}

#[test]
fn fit_rect_letterboxes_and_pillarboxes_by_display_aspect() {
    // 16:9 fills the frame.
    assert_eq!(fit_rect(&info(640, 360, (1, 1))), (0, 0, 1920, 1080));
    // 4:3 is pillarboxed.
    assert_eq!(fit_rect(&info(480, 360, (1, 1))), (240, 0, 1440, 1080));
    // 2.35:1 is letterboxed.
    assert_eq!(fit_rect(&info(1880, 800, (1, 1))), (0, 131, 1920, 817));
    // Anamorphic: 1440×1080 with 4:3 pixels is 16:9.
    assert_eq!(fit_rect(&info(1440, 1080, (4, 3))), (0, 0, 1920, 1080));
}

#[test]
fn zoom_params_follow_the_measured_mapping() {
    assert_eq!(zoom_params(Zoom::IDENTITY), (1.0, 0.0, 0.0));
    // Scale s; translation −pan·s on both axes.
    assert_eq!(zoom_params(Zoom::new(2.0, 0.25, -0.125)), (2.0, -0.5, 0.25));
}

#[test]
fn encoders_prefer_va_then_x264() {
    let choice = |has: fn(&str) -> bool| choose_encoder(has).map(|(name, _)| name);
    assert_eq!(choice(|_| true), Some("vah264lpenc"));
    assert_eq!(choice(|f| f == "x264enc"), Some("x264enc"));
    assert_eq!(choice(|_| false), None);
}

#[test]
fn part_path_appends_to_the_file_name() {
    assert_eq!(
        part_path(Path::new("/a/b c.mp4")),
        PathBuf::from("/a/b c.mp4.part")
    );
}

/// An element erroring mid-stream, downstream of `appsrc`, ends the export:
/// a blocking push would hang here forever.
#[test]
fn a_mid_stream_error_fails_the_export_without_hanging() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = fixtures::counter_video(
        &dir.path().join("src.webm"),
        640,
        360,
        25,
        75,
        CounterKind::Vp8WebmWithAudio,
    );
    let path = dir.path().join("out.mp4");
    let frames = (0..60)
        .map(|n| FrameSpec {
            source_time: f64::from(n) / 30.0,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let job = ExportJob {
        source,
        frames,
        path: path.clone(),
    };
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let _exporter = Exporter::spawn(
        job,
        move |msg| {
            let _ = tx.send(msg);
        },
        Some("identity error-after=10"),
    )
    .unwrap();

    let deadline = Duration::from_secs(60);
    let result = loop {
        match rx.recv_timeout(deadline.saturating_sub(started.elapsed())) {
            Ok(ExportMessage::Finished(result)) => break result,
            Ok(ExportMessage::Progress(_)) => {}
            Err(_) => panic!("no Finished within {deadline:?}"),
        }
    };
    assert!(
        matches!(result, Err(ExportError::Failed(_))),
        "expected a failure, got {result:?}"
    );
    assert!(!path.exists());
    assert!(!part_path(&path).exists());
}
