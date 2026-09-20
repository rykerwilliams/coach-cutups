//! The composite's layout ratios.

use video_coach_core::layout::{
    bar_rect, pip_rect, scoreboard_rects, stroke_line_width, Rect, BAR_HEIGHT_RATIO,
    PIP_WIDTH_RATIO,
};

#[test]
fn the_pip_sits_above_the_text_bar_at_the_ratio_table_s_size() {
    // 1920×1080 with a 16:9 camera: 422.4 × 237.6, 23.76 from the right edge
    // and the same again above the 86.4 px bar.
    let r = pip_rect(1920.0, 1080.0, 16.0 / 9.0);
    assert_eq!(
        r,
        Rect {
            x: 1920.0 - 23.76 - 422.4,
            y: 1080.0 - 86.4 - 23.76 - 237.6,
            w: 422.4,
            h: 237.6,
        }
    );
    // The same margin from the right edge as from the bar's top (spec E2):
    // the inset sits on the bar rather than over it.
    let bar = bar_rect(1920.0, 1080.0);
    assert!((1920.0 - (r.x + r.w) - (bar.y - (r.y + r.h))).abs() < 1e-9);
}

#[test]
fn the_text_bar_is_a_full_width_strip_along_the_bottom() {
    let bar = bar_rect(1920.0, 1080.0);
    assert_eq!(
        bar,
        Rect {
            x: 0.0,
            y: 1080.0 - BAR_HEIGHT_RATIO * 1080.0,
            w: 1920.0,
            h: BAR_HEIGHT_RATIO * 1080.0,
        }
    );
    // It reaches the bottom edge exactly, at every output size.
    for (w, h) in [(1280.0, 720.0), (1920.0, 1080.0), (3840.0, 2160.0)] {
        let bar = bar_rect(w, h);
        assert_eq!(bar.y + bar.h, h);
        assert_eq!(bar.h / h, BAR_HEIGHT_RATIO);
    }
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
fn the_scoreboard_sits_inset_from_the_top_left() {
    let s = scoreboard_rects(1920.0, 1080.0);
    // 0.015 × 1080 in from both edges, 0.36 × 1920 by 0.08 × 1080.
    assert_eq!(s.bar.x, 16.2);
    assert_eq!(s.bar.y, 16.2);
    assert!((s.bar.w - 691.2).abs() < 1e-9);
    assert_eq!(s.bar.h, 86.4);
    // Square gap: the same pixels from the top as from the left, at any aspect.
    assert_eq!(s.bar.x, s.bar.y);
}

#[test]
fn the_scoreboard_s_cells_tile_its_bar_under_the_accent_strip() {
    let s = scoreboard_rects(1920.0, 1080.0);
    let cells = [s.home, s.score, s.away, s.clock];

    // The accent strip spans the bar's top; the cells fill the rest.
    assert_eq!(s.accent.x, s.bar.x);
    assert_eq!(s.accent.y, s.bar.y);
    assert_eq!(s.accent.w, s.bar.w);
    assert_eq!(s.accent.h, s.bar.h * 0.08);

    for cell in cells {
        assert_eq!(cell.y, s.accent.y + s.accent.h);
        assert_eq!(cell.h, s.bar.h - s.accent.h);
    }
    // Edge to edge with no seam and no overhang: the clock closes the bar.
    assert_eq!(s.home.x, s.bar.x);
    assert_eq!(s.score.x, s.home.x + s.home.w);
    assert_eq!(s.away.x, s.score.x + s.score.w);
    assert_eq!(s.clock.x, s.away.x + s.away.w);
    assert_eq!(s.clock.x + s.clock.w, s.bar.x + s.bar.w);
    // The score and clock columns are the narrow ones.
    assert!(s.score.w < s.home.w && s.clock.w < s.away.w);
}

#[test]
fn the_stoppage_tail_hangs_outside_the_bar() {
    let s = scoreboard_rects(1920.0, 1080.0);
    assert!(s.tail.x > s.bar.x + s.bar.w);
    // Its gap off the clock cell is a fraction of the cell height, so it holds
    // at every output size rather than being an absolute 2 pt.
    assert!((s.tail.x - (s.clock.x + s.clock.w) - s.clock.h * 0.025).abs() < 1e-9);
    assert_eq!(s.tail.y, s.clock.y);
    assert_eq!(s.tail.h, s.clock.h);
}

#[test]
fn the_scoreboard_is_the_same_fraction_of_the_frame_at_every_output_size() {
    let big = scoreboard_rects(1920.0, 1080.0);
    let small = scoreboard_rects(1280.0, 720.0);
    for (b, s) in [
        (big.bar, small.bar),
        (big.accent, small.accent),
        (big.home, small.home),
        (big.score, small.score),
        (big.away, small.away),
        (big.clock, small.clock),
        (big.tail, small.tail),
    ] {
        assert!((b.x / 1920.0 - s.x / 1280.0).abs() < 1e-12);
        assert!((b.y / 1080.0 - s.y / 720.0).abs() < 1e-12);
        assert!((b.w / 1920.0 - s.w / 1280.0).abs() < 1e-12);
        assert!((b.h / 1080.0 - s.h / 720.0).abs() < 1e-12);
    }
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
