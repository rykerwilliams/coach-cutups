//! One export read back through `openh264dec`.
//!
//! CI installs `gstreamer1.0-libav` for `avenc_aac`, which brings `avdec_h264`
//! (rank 256) with it, and the dev machine ranks `vah264dec` (257) above both.
//! Either way `openh264dec` (marginal, 64) stops being auto-plugged anywhere,
//! and the one decoder the port could rely on being present on a bare runner
//! would go untested. This pins it by rank.
//!
//! **Its own test binary.** A rank is process-global, so lowering one in a
//! suite that runs its tests in parallel would change which decoder the other
//! tests use, at a moment nothing controls. Setting it here, before anything
//! else in the process decodes, is the whole reason this file exists.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use uuid::Uuid;
use video_coach_core::export::{FrameSpec, OUTPUT_FPS};
use video_coach_core::project::{Clip, Quality, Resolution};
use video_coach_core::zoom::Zoom;
use video_coach_media::fixtures::{self, CounterKind};
use video_coach_media::{EntryMedia, ExportJob, ExportMessage, Exporter};

/// Far beyond this export, even on a loaded llvmpipe runner.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Lowers every other H.264 decoder out of the way and returns the one
/// `decodebin3` is now bound to pick.
fn top_h264_decoder() -> String {
    gst::init().unwrap();
    let h264 = gst::Caps::builder("video/x-h264").build();
    let decoders: Vec<_> = gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_VIDEO,
        gst::Rank::MARGINAL,
    )
    .into_iter()
    .filter(|f| f.can_sink_any_caps(&h264))
    .collect();
    assert!(
        decoders.iter().any(|f| f.name() == "openh264dec"),
        "openh264dec is missing: install gstreamer1.0-plugins-bad"
    );
    for factory in &decoders {
        if factory.name() != "openh264dec" {
            factory.set_rank(gst::Rank::NONE);
        }
    }
    decoders
        .into_iter()
        .max_by_key(|f| f.rank())
        .expect("openh264dec is in there")
        .name()
        .into()
}

/// An H.264 export decodes back to its schedule with `openh264dec` doing the
/// decoding — on both sides, since the source is H.264 too.
#[test]
fn an_h264_export_round_trips_through_openh264dec() {
    assert_eq!(top_h264_decoder(), "openh264dec");

    let dir = tempfile::tempdir().unwrap();
    let source = fixtures::counter_video(
        &dir.path().join("src.mp4"),
        640,
        360,
        30,
        60,
        CounterKind::H264Mp4BFrames,
    );
    // Mid-frame source times, so which frame answers is never a question of
    // rounding: `tests/export.rs` is where that is pinned.
    let frames: Vec<FrameSpec> = (0..20)
        .map(|n| FrameSpec {
            entry: 0,
            source_time: (f64::from(n) + 0.5) / f64::from(OUTPUT_FPS),
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let clip = Clip {
        id: Uuid::nil(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: 0.0,
        recording_duration: frames.len() as f64 / f64::from(OUTPUT_FPS),
        recording_filename: "c.mkv".into(),
        events: Vec::new(),
        show_pip: false,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    };
    let path = dir.path().join("out.mp4");
    let job = ExportJob {
        compilation: fixtures::one_entry(&clip, frames.clone(), ""),
        sources: vec![source],
        entries: vec![EntryMedia {
            recording: PathBuf::new(),
            clip,
        }],
        audio: Vec::new(),
        path: path.clone(),
        resolution: Resolution::R720,
        quality: Quality::Medium,
    };

    let (tx, rx) = mpsc::channel();
    let _exporter = Exporter::start(job, move |msg| {
        if let ExportMessage::Finished(result) = msg {
            let _ = tx.send(result);
        }
    });
    rx.recv_timeout(TIMEOUT)
        .expect("the export finished")
        .expect("the export succeeded");

    // The source is 30 fps and the schedule walks it frame for frame.
    let expected: Vec<u32> = (0..frames.len() as u32).collect();
    assert_eq!(fixtures::decode_counters(&path), expected);
}
