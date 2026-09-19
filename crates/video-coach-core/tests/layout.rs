//! The composite's layout ratios.

use video_coach_core::layout::{pip_rect, stroke_line_width, Rect, PIP_WIDTH_RATIO};

#[test]
fn the_pip_sits_in_the_bottom_right_corner_at_the_ratio_table_s_size() {
    // 1920×1080 with a 16:9 camera: 422.4 × 237.6, 23.76 from each edge.
    let r = pip_rect(1920.0, 1080.0, 16.0 / 9.0);
    assert_eq!(
        r,
        Rect {
            x: 1920.0 - 23.76 - 422.4,
            y: 1080.0 - 23.76 - 237.6,
            w: 422.4,
            h: 237.6,
        }
    );
    // Flush to both edges by the same margin.
    assert!((1920.0 - (r.x + r.w) - (1080.0 - (r.y + r.h))).abs() < 1e-9);
}

#[test]
fn the_pip_is_the_same_fraction_of_the_frame_at_every_output_size() {
    let big = pip_rect(1920.0, 1080.0, 16.0 / 9.0);
    let small = pip_rect(1280.0, 720.0, 16.0 / 9.0);
    assert!((big.w / 1920.0 - small.w / 1280.0).abs() < 1e-12);
    assert!((big.h / 1080.0 - small.h / 720.0).abs() < 1e-12);
    assert!((big.x / 1920.0 - small.x / 1280.0).abs() < 1e-12);
    assert!((big.y / 1080.0 - small.y / 720.0).abs() < 1e-12);
}

#[test]
fn a_camera_of_another_aspect_changes_the_pip_s_height_only() {
    let wide = pip_rect(1920.0, 1080.0, 16.0 / 9.0);
    let four_three = pip_rect(1920.0, 1080.0, 4.0 / 3.0);
    assert_eq!(four_three.w, wide.w);
    assert_eq!(four_three.w, PIP_WIDTH_RATIO * 1920.0);
    // Taller, and it grows upward: the bottom edge stays put.
    assert!(four_three.h > wide.h);
    assert!((four_three.y + four_three.h - (wide.y + wide.h)).abs() < 1e-9);
    // The camera is neither stretched nor letterboxed inside the inset.
    assert!((four_three.w / four_three.h - 4.0 / 3.0).abs() < 1e-12);
}

#[test]
fn stroke_line_width_scales_with_the_picture_s_height() {
    assert_eq!(stroke_line_width(0.01, 1080.0), 10.8);
    // Half the picture, half the pen: weight is constant per resolution.
    assert_eq!(
        stroke_line_width(0.01, 540.0),
        stroke_line_width(0.01, 1080.0) / 2.0
    );
}
