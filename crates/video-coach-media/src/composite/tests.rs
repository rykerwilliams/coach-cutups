//! The composite's shared geometry: the picture rect and the zoom mapping.

use video_coach_core::zoom::Zoom;

use super::*;

fn info(w: u32, h: u32, par: (i32, i32)) -> gst_video::VideoInfo {
    gst::init().unwrap();
    gst_video::VideoInfo::builder(gst_video::VideoFormat::Rgba, w, h)
        .par(gst::Fraction::new(par.0, par.1))
        .build()
        .unwrap()
}

#[test]
fn fit_rect_letterboxes_and_pillarboxes_by_display_aspect() {
    let fit = |w, h, par| fit_rect(&info(w, h, par), 1920, 1080);
    // 16:9 fills the frame.
    assert_eq!(fit(640, 360, (1, 1)), (0, 0, 1920, 1080));
    // 4:3 is pillarboxed.
    assert_eq!(fit(480, 360, (1, 1)), (240, 0, 1440, 1080));
    // 2.35:1 is letterboxed.
    assert_eq!(fit(1880, 800, (1, 1)), (0, 131, 1920, 817));
    // Anamorphic: 1440x1080 with 4:3 pixels is 16:9.
    assert_eq!(fit(1440, 1080, (4, 3)), (0, 0, 1920, 1080));
}

/// The same source lands proportionally at preview's size: one layout, two
/// outputs (spec P1).
#[test]
fn fit_rect_scales_with_the_output_size() {
    assert_eq!(
        fit_rect(&info(480, 360, (1, 1)), 1280, 720),
        (160, 0, 960, 720)
    );
}

/// A seek names a frame by its time, and a buffer by its PTS; both come back
/// through `frame_index`, so it has to invert `frame_time` exactly -- at 30
/// fps neither is a whole number of nanoseconds.
#[test]
fn frame_index_inverts_frame_time() {
    for n in [0u64, 1, 29, 30, 31, 899, 54_000] {
        assert_eq!(frame_index(frame_time(n)), n, "frame {n}");
    }
    // And rounds to the nearest frame either side of one.
    let frame_30 = frame_time(30);
    assert_eq!(
        frame_index(frame_30 - gst::ClockTime::from_mseconds(16)),
        30
    );
    assert_eq!(
        frame_index(frame_30 + gst::ClockTime::from_mseconds(16)),
        30
    );
}

#[test]
fn zoom_params_follow_the_measured_mapping() {
    assert_eq!(zoom_params(Zoom::IDENTITY), (1.0, 0.0, 0.0));
    // Scale s; translation -pan*s on both axes.
    assert_eq!(zoom_params(Zoom::new(2.0, 0.25, -0.125)), (2.0, -0.5, 0.25));
}
