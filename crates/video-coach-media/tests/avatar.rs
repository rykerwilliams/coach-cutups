//! [`decode_still`], the one avatar decoder (avatar spec A3): the pick
//! validates with it, the Devices popover and the recording corner draw with
//! it, and `composite::avatar::open` scales its output into the pixmap.
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
    let path = fixtures::translucent_png(dir.path(), "half.png", 32, 32);
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
