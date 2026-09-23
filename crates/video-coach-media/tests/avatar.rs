//! [`decode_still`], the one avatar decoder (avatar spec A3): the pick
//! validates with it, and `composite::avatar`'s `drawn` and `open` scale its
//! output into the pixmaps the UI and the inset pad take.
//!
//! The pixels it produces are checked where they are used, in
//! `composite/avatar.rs`'s own tests; what is checked here is the seam the
//! app reaches for — what decodes, what is refused, and with what message.

use video_coach_media::decode_still;
use video_coach_media::fixtures::{self, StillFormat};

fn dir() -> tempfile::TempDir {
    gstreamer::init().unwrap();
    tempfile::tempdir().unwrap()
}

#[test]
fn decode_still_reads_a_png_and_a_jpeg() {
    let dir = dir();
    for (name, format) in [
        ("square.png", StillFormat::Png),
        ("square.jpg", StillFormat::Jpeg),
    ] {
        let path = fixtures::still_image(dir.path(), name, 96, 96, format);
        let still = decode_still(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((still.w, still.h), (96, 96), "{name}");
        assert_eq!(still.rgba.len(), 96 * 96 * 4, "{name}: tightly packed RGBA");
    }
}

#[test]
fn decode_still_keeps_a_non_square_shape() {
    let dir = dir();
    let path = fixtures::still_image(dir.path(), "tall.png", 60, 80, StillFormat::Png);
    let still = decode_still(&path).unwrap();
    assert_eq!((still.w, still.h), (60, 80));
}

#[test]
fn decode_still_keeps_transparency() {
    let dir = dir();
    let path = fixtures::solid_png(dir.path(), "half.png", 32, 32, 0x00ff_ffff, 128);
    let still = decode_still(&path).unwrap();
    // Straight alpha, as GStreamer's RGBA is: white at half alpha is still
    // full white. Premultiplying is `avatar::open`'s job, not the decoder's.
    let centre = (16 * 32 + 16) * 4;
    let px = &still.rgba[centre..centre + 4];
    assert!(px[0] > 250, "colour untouched: {px:?}");
    assert!((i32::from(px[3]) - 128).abs() <= 4, "half alpha: {px:?}");
}

#[test]
fn decode_still_takes_the_first_frame_of_a_multi_frame_file() {
    let dir = dir();
    // No animation and no error (spec A3): a file with more than one frame
    // yields its first.
    let path = fixtures::webm(dir.path(), "clip.webm", 1, 64, 48, 30, 30);
    let still = decode_still(&path).unwrap();
    assert_eq!((still.w, still.h), (64, 48));
}

#[test]
fn decode_still_refuses_a_non_image_with_a_message() {
    let dir = dir();
    let path = dir.path().join("notes.png");
    std::fs::write(&path, "this is not an image").unwrap();
    let e = decode_still(&path).expect_err("a text file is not an image");
    assert!(!e.is_empty(), "the refusal carries the decoder's message");
}

#[test]
fn decode_still_refuses_a_missing_file_with_a_message() {
    let dir = dir();
    let e = decode_still(&dir.path().join("gone.png")).expect_err("no such file");
    assert!(!e.is_empty(), "the refusal carries the decoder's message");
}

/// What drawing the avatar into the overlay layer would cost per frame: one
/// `draw_pixmap` of an inset-sized, pre-scaled pixmap into a 1080p frame, at
/// rest and at full size.
///
/// **This is the measurement that said no** (avatar spec E1, which holds the
/// numbers and the reading of them). It is kept because it is the evidence for
/// a closed decision: the blit costs more than the whole overlay layer does, so
/// the avatar rides the GL inset pad instead. The whole-frame clear below is
/// the calibration against the compositing spike's machine, so a number from
/// this one can be read beside that one.
///
/// `#[ignore]`d because it is a measurement, not an assertion: a threshold
/// here would fail on a loaded machine and say nothing about the design. Run
/// it when the question comes back:
///
/// ```text
/// cargo test --release -p video-coach-media --test avatar -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement, not an assertion -- see the doc comment"]
fn the_avatar_blit_costs() {
    use std::time::Instant;
    use tiny_skia::{Color, FilterQuality, Pixmap, PixmapPaint, Transform};
    use video_coach_core::avatar::PULSE_GROWTH;
    use video_coach_core::layout::pip_rect;

    /// Enough for the spread between runs to sit under a tenth of the number.
    const RUNS: u32 = 200;
    const WARMUP: u32 = 20;

    let (out_w, out_h) = (1920.0, 1080.0);
    // A 4:3 inset: the shape this was measured at while the question was open.
    // The square box the design settled on (spec A5) is larger still, so
    // keeping the original geometry only understates the answer.
    let rect = pip_rect(out_w, out_h, 4.0 / 3.0);
    let mut image = Pixmap::new(rect.w.ceil() as u32, rect.h.ceil() as u32).unwrap();
    image.fill(Color::from_rgba8(200, 120, 90, 255));
    let mut frame = Pixmap::new(out_w as u32, out_h as u32).unwrap();
    println!(
        "inset {}x{} into {}x{}",
        image.width(),
        image.height(),
        frame.width(),
        frame.height()
    );

    // The calibration, so this number can be read beside the compositing
    // spike's (0.61 ms on its machine for the same clear).
    for _ in 0..WARMUP {
        frame.fill(Color::TRANSPARENT);
    }
    let started = Instant::now();
    for _ in 0..RUNS {
        frame.fill(Color::TRANSPARENT);
    }
    println!(
        "clear {}x{}: {:.3} ms/frame",
        frame.width(),
        frame.height(),
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(RUNS)
    );

    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    // `avatar_rect`'s two ends, without core's clamp in the way: the pulse
    // runs between `1 / PULSE_GROWTH` and 1.
    for (name, scale) in [("rest", 1.0 / PULSE_GROWTH), ("full", 1.0)] {
        let (w, h) = (rect.w * scale, rect.h * scale);
        // `draw_pixmap`'s transform moves the source rect as well as the
        // pattern, so the placement rides in the transform and x/y stay 0.
        let placed = Transform::from_translate(
            (rect.x + (rect.w - w) / 2.0) as f32,
            (rect.y + (rect.h - h) / 2.0) as f32,
        )
        .pre_scale(
            (w / f64::from(image.width())) as f32,
            (h / f64::from(image.height())) as f32,
        );
        let mut blit = || {
            frame
                .as_mut()
                .draw_pixmap(0, 0, image.as_ref(), &paint, placed, None);
        };
        for _ in 0..WARMUP {
            blit();
        }
        let started = Instant::now();
        for _ in 0..RUNS {
            blit();
        }
        println!(
            "avatar blit at {name} (x{scale:.3}): {:.3} ms/frame",
            started.elapsed().as_secs_f64() * 1000.0 / f64::from(RUNS)
        );
    }
}
