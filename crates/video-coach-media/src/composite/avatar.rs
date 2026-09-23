//! The avatar image (avatar spec A3, A5): one decoder for it, the pre-scaled,
//! premultiplied, circular pixmap the mixer's inset pad carries, and the pulse
//! that sizes that pad frame by frame.
//!
//! **The inset pad, not the overlay** (spec E1, which carries the
//! measurement): drawing it into the overlay's pixmap costs more per frame than
//! the whole overlay does, because `tiny_skia` has no sprite fast path. So the
//! scaling and the blending happen on the GPU, where every other full-frame
//! pixel operation happens (CLAUDE.md).
//!
//! **One decoder, and one set of drawn pixels.** [`decode_still`] is the whole
//! of the decoding — the pick validates with it, and everything below scales
//! its output — and [`drawn`] is the whole of the drawing: cover-cropped into a
//! square, masked to a circle, premultiplied. The Devices popover's thumbnail
//! and the recording corner take it too, so what the coach is shown is what the
//! export draws, and a file that passes the pick cannot fail in an export.
//!
//! **Straight alpha in, premultiplied out.** GStreamer's `RGBA` is straight
//! alpha; tiny-skia stores premultiplied pixels (`overlay.rs` says exactly
//! this about the layer it draws into). A memcpy would leave a cut-out PNG's
//! soft edges at full colour — a bright halo around every soft pixel — so the
//! copy multiplies each of R, G and B by A.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use tiny_skia::{FillRule, FilterQuality, Mask, PathBuilder, Pixmap, PixmapPaint, Transform};
use video_coach_core::avatar::{avatar_box, pulse, PULSE_RATE};
use video_coach_core::export::OUTPUT_FPS;
use video_coach_core::layout::{self, Rect as LayoutRect};

use super::audio::Reader;
use super::CompositeError;

/// How long the still's pipeline may take before it is given up on. A still
/// is one frame off a local file; the bound is here so a file that stalls a
/// decoder costs a message rather than the app.
const STILL_TIMEOUT: Duration = Duration::from_secs(10);

/// One still frame's **straight-alpha** RGBA, at its own size, tightly
/// packed.
#[derive(Debug, Clone)]
pub struct Still {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Decodes the first frame of `path` as RGBA. The one avatar decoder (see
/// the module comment).
///
/// `videoflip video-direction=auto` applies the `image-orientation` tag,
/// which is where a phone's EXIF rotation ends up, so a portrait photo comes
/// out upright. ([`crate::probe`] *refuses* a rotated source instead: there a
/// timeline and a stored aspect are at stake, and here there is neither.)
///
/// A file that decodes to several frames — an animated PNG, a video — yields
/// its **first** frame and no error.
pub fn decode_still(path: &Path) -> Result<Still, String> {
    let pipeline = gst::parse::launch(
        "decodebin3 name=dec ! video/x-raw(ANY) ! videoflip video-direction=auto \
         ! videoconvert ! video/x-raw,format=RGBA,pixel-aspect-ratio=1/1 \
         ! appsink name=sink sync=false max-buffers=1",
    )
    .map_err(|e| format!("the image decoder could not be built: {e}"))?
    .downcast::<gst::Pipeline>()
    .expect("a multi-element launch string yields a pipeline");
    // Located before it is linked: linking queries it, which starts it, and a
    // source with no location posts an error then.
    let filesrc = gst::ElementFactory::make("filesrc")
        .property("location", path)
        .build()
        .map_err(|e| format!("the image decoder could not be built: {e}"))?;
    pipeline
        .add(&filesrc)
        .map_err(|e| format!("the image decoder could not be built: {e}"))?;
    filesrc
        .link(&pipeline.by_name("dec").expect("the pipeline has `dec`"))
        .map_err(|e| format!("the image decoder could not be built: {e}"))?;
    let sink = pipeline
        .by_name("sink")
        .and_downcast::<gst_app::AppSink>()
        .expect("the pipeline has an appsink named `sink`");

    let still = first_frame(&pipeline, &sink);
    let _ = pipeline.set_state(gst::State::Null);
    still
}

/// Runs `pipeline` until its `appsink` yields a frame, it fails, or it runs
/// out of file — whichever comes first, and never longer than
/// [`STILL_TIMEOUT`].
fn first_frame(pipeline: &gst::Pipeline, sink: &gst_app::AppSink) -> Result<Still, String> {
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("the image could not be read: {e}"))?;
    let bus = pipeline.bus().expect("a pipeline has a bus");
    let deadline = Instant::now() + STILL_TIMEOUT;
    loop {
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
            return still_from(&sample);
        }
        if let Some(msg) = bus.timed_pop_filtered(
            super::POLL,
            &[gst::MessageType::Error, gst::MessageType::Eos],
        ) {
            // The buffer reaches the appsink before the EOS that follows it
            // does the bus, but the two arrive on different threads: pull
            // once more before calling an end of file empty.
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                return still_from(&sample);
            }
            return Err(match msg.view() {
                gst::MessageView::Error(err) => {
                    format!("the image could not be read: {}", crate::error_text(err))
                }
                _ => "the file holds no image".to_owned(),
            });
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the image did not decode within {} seconds",
                STILL_TIMEOUT.as_secs()
            ));
        }
    }
}

/// A decoded sample as tightly packed RGBA.
fn still_from(sample: &gst::Sample) -> Result<Still, String> {
    let info = sample
        .caps()
        .and_then(|caps| gst_video::VideoInfo::from_caps(caps).ok())
        .ok_or_else(|| "the image decoded without usable caps".to_owned())?;
    let buffer = sample
        .buffer()
        .ok_or_else(|| "the image decoded without pixels".to_owned())?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|_| "the decoded image could not be read".to_owned())?;
    let (w, h) = (info.width(), info.height());
    let stride = frame.plane_stride()[0] as usize;
    let plane = frame.plane_data(0).expect("RGBA has one plane");
    let rgba = plane
        .chunks(stride)
        .take(h as usize)
        .flat_map(|row| row[..w as usize * 4].iter().copied())
        .collect();
    Ok(Still { w, h, rgba })
}

/// The avatar as it is drawn, at a size the caller asks for: `size`×`size`
/// **premultiplied** RGBA, tightly packed.
///
/// The one seam outside this crate for the drawn pixels — the Devices
/// popover's thumbnail and the recording corner — so neither can show
/// something the export won't draw.
#[derive(Debug, Clone)]
pub struct Drawn {
    /// Both sides: the avatar is drawn in a square box (see [`open`]).
    pub size: u32,
    pub rgba: Vec<u8>,
}

/// Decodes `path` and draws it as the inset draws it, `size`×`size`.
pub fn drawn(path: &Path, size: u32) -> Result<Drawn, String> {
    let image = circular(&decode_still(path)?, size)?;
    Ok(Drawn {
        size,
        rgba: image.data().to_vec(),
    })
}

/// The avatar as the mixer's inset pad takes it.
pub(super) struct Avatar {
    /// Cover-cropped into the square inset, premultiplied, and masked to the
    /// circle inscribed in it, so the pad has only to scale and blend it.
    pub(super) image: Pixmap,
    /// [`avatar_box`] of [`layout::pip_rect`] for a **square** inset: the
    /// avatar's footprint at its loudest.
    pub(super) rect: LayoutRect,
}

/// Decodes `path` and prepares it for an `out_w`×`out_h` run.
///
/// **The box is square and the image is cover-cropped into it** (spec A5).
/// What is drawn is always the circle inscribed in the box, so a box of the
/// image's own aspect would put a portrait's circle floating above the corner
/// the webcam inset sits in and a wide screenshot's off the top of the frame.
/// A square [`layout::pip_rect`] puts the circle on the webcam's own right and
/// bottom margins whatever was picked, and the image fills it: scaled until its
/// shorter side covers the box, then centred, so the middle of the picture —
/// where a face is — is what survives.
///
/// **And the box is [`avatar_box`] of that rect, not the rect**: a photograph
/// at the webcam's full size reads as too big (the coach's, on seeing it), so
/// the box is shrunk about its bottom-right corner — keeping the inset's own
/// right and bottom margins — and only then does the pulse breathe inside it.
///
/// The circle is masked in **once, here**: the mask is the same size for every
/// frame of the run, so building it per frame would buy nothing and cost a
/// rasterization.
pub(super) fn open(path: &Path, out_w: f64, out_h: f64) -> Result<Avatar, String> {
    let still = decode_still(path)?;
    let rect = avatar_box(layout::pip_rect(out_w, out_h, 1.0));
    // Rounded **up**, so the drawn box is never short of the rect it stands
    // for; everything after this reads the small pixmap.
    let image = circular(&still, rect.w.ceil() as u32)?;
    Ok(Avatar { image, rect })
}

/// [`open`], reporting a failure on stderr and giving the caller `None`.
///
/// **A missing or unreadable image costs the inset, never the run** (spec A4),
/// and it is reported once for the run rather than once an entry: the same
/// trade, and the same tone, as a picture-in-picture that will not open.
pub(super) fn open_reported(path: &Path, out_w: f64, out_h: f64) -> Option<Avatar> {
    match open(path, out_w, out_h) {
        Ok(avatar) => Some(avatar),
        Err(e) => {
            eprintln!(
                "no avatar from {}: {e}; the inset stays empty",
                path.display()
            );
            None
        }
    }
}

/// `still` premultiplied, cover-cropped into a `size`×`size` box and masked to
/// the circle inscribed in it — the avatar exactly as it is drawn (see
/// [`open`]).
fn circular(still: &Still, size: u32) -> Result<Pixmap, String> {
    let native = premultiplied(still)?;
    let mut image = Pixmap::new(size, size)
        .ok_or_else(|| format!("the inset is not a usable size ({size}x{size})"))?;
    // Cover, not fit: the **larger** ratio, so the shorter side reaches the box
    // and the longer one overhangs it equally at both ends.
    let box_side = f64::from(size);
    let scale = (box_side / f64::from(still.w)).max(box_side / f64::from(still.h));
    let (w, h) = (f64::from(still.w) * scale, f64::from(still.h) * scale);
    image.draw_pixmap(
        0,
        0,
        native.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        },
        // `draw_pixmap`'s transform moves the source rect as well as the
        // pattern, so the placement rides in the transform and x/y stay 0.
        Transform::from_translate(((box_side - w) / 2.0) as f32, ((box_side - h) / 2.0) as f32)
            .pre_scale(scale as f32, scale as f32),
        None,
    );
    mask_to_circle(&mut image);
    Ok(image)
}

/// `still`'s pixels as a premultiplied pixmap at its own size (see the module
/// comment for why the copy multiplies).
fn premultiplied(still: &Still) -> Result<Pixmap, String> {
    let mut pixmap = Pixmap::new(still.w, still.h)
        .ok_or_else(|| format!("the image is not a usable size ({}x{})", still.w, still.h))?;
    let scale = |channel: u8, alpha: u8| {
        // Rounded, not truncated: the difference is a pixel a shade dark at
        // every alpha, and it accumulates nowhere else to correct it.
        ((u32::from(channel) * u32::from(alpha) + 127) / 255) as u8
    };
    for (out, px) in pixmap
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(still.rgba.as_chunks::<4>().0)
    {
        let a = px[3];
        *out = [scale(px[0], a), scale(px[1], a), scale(px[2], a), a];
    }
    Ok(pixmap)
}

/// One pulse level per output frame of an entry, from `recording`'s own
/// commentary (spec D2): how loud the coach is at each frame, smoothed, in
/// `0..=1`.
///
/// **Called in job setup, never from the frame loop.** Decoding audio between
/// two pushed frames would stall the pump and the encoder behind it (spec D5).
/// The decode is bounded by what the entry shows — the `frames` output frames'
/// worth of samples, and not a sample of a recording that runs past them — so a
/// two-second entry of an hour-long take reads two seconds.
///
/// A recording with no audio track, one that will not read, one shorter than
/// its entry, and a cancel all read as silence: a flat avatar at rest. The
/// sound is the inset's own, so losing it costs the motion rather than the run,
/// exactly as a missing image costs the inset.
pub(super) fn pulse_table(recording: &Path, frames: usize, cancel: &AtomicBool) -> Vec<f64> {
    let wanted = (frames as u64 * u64::from(PULSE_RATE)).div_ceil(u64::from(OUTPUT_FPS)) as usize;
    let mut reader = match Reader::start(recording, PULSE_RATE, 1, cancel) {
        Ok(reader) => reader,
        // A cancel is the run ending, and its own `Watch` is about to see it.
        Err(CompositeError::Cancelled) => None,
        Err(CompositeError::Failed(e)) => {
            eprintln!(
                "no avatar pulse from {}: {e}; the inset holds still",
                recording.display()
            );
            None
        }
    };
    // Zero-padded past the end of the file, which is what an entry longer than
    // its recording wants.
    let samples = reader
        .as_mut()
        .map(|reader| reader.read(wanted, cancel))
        .unwrap_or(&[]);
    pulse(samples, PULSE_RATE, frames)
}

/// Cuts `image` down to the circle inscribed in it — the avatar is a
/// gravatar, which is round (spec A5).
///
/// A cut-out PNG is masked too, which costs it nothing: what a cut-out puts
/// near the corners of its box is already transparent.
fn mask_to_circle(image: &mut Pixmap) {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let mut builder = PathBuilder::new();
    builder.push_circle(w / 2.0, h / 2.0, w.min(h) / 2.0);
    let (Some(circle), Some(mut mask)) =
        (builder.finish(), Mask::new(image.width(), image.height()))
    else {
        return;
    };
    // Anti-aliased, unlike the overlay's picture mask: this edge is a curve,
    // and a hard one would crawl as the pulse resizes it.
    mask.fill_path(&circle, FillRule::Winding, true, Transform::identity());
    image.apply_mask(&mask);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{self, StillFormat};

    fn dir() -> tempfile::TempDir {
        gst::init().unwrap();
        tempfile::tempdir().unwrap()
    }

    /// The pixel at the middle of the fitted box, which every avatar covers.
    fn centre(avatar: &Avatar) -> tiny_skia::PremultipliedColorU8 {
        let image = &avatar.image;
        image
            .pixel(image.width() / 2, image.height() / 2)
            .expect("the centre is inside the pixmap")
    }

    #[test]
    fn the_avatar_pixmap_is_premultiplied() {
        let dir = dir();
        let path = fixtures::solid_png(dir.path(), "half.png", 64, 64, 0x00ff_ffff, 128);
        let avatar = open(&path, 1920.0, 1080.0).unwrap();
        let px = centre(&avatar);
        // Half-transparent white: premultiplied, every colour channel is the
        // alpha. A straight copy would leave them at 255 — the haloed
        // cut-out this test exists for.
        assert!(
            (i32::from(px.alpha()) - 128).abs() <= 4,
            "alpha kept: {px:?}"
        );
        for channel in [px.red(), px.green(), px.blue()] {
            assert!(
                (i32::from(channel) - i32::from(px.alpha())).abs() <= 4,
                "colour scaled by alpha: {px:?}"
            );
        }
    }

    #[test]
    fn an_avatar_is_a_circle_in_its_box() {
        let dir = dir();
        let path = fixtures::still_image(dir.path(), "square.png", 96, 96, StillFormat::Png);
        let avatar = open(&path, 1920.0, 1080.0).unwrap();
        let image = &avatar.image;
        let (w, h) = (image.width(), image.height());
        let alpha = |x: u32, y: u32| image.pixel(x, y).expect("inside the pixmap").alpha();

        assert!(centre(&avatar).alpha() > 0, "the middle is drawn");
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(alpha(x, y), 0, "the corner at ({x}, {y}) is cut away");
        }
        // The boundary is the inscribed circle's, not the box's: a point a
        // quarter of the way in on the diagonal is inside it, an eighth is
        // outside.
        assert!(alpha(w / 4, h / 4) > 0, "inside the circle");
        assert_eq!(alpha(w / 8, h / 8), 0, "outside the circle");
    }

    /// Whatever shape is picked, the inset is the **square** `pip_rect` cut
    /// down by `avatar_box`, and the pixmap is square with it (spec A5).
    ///
    /// A box of the image's own aspect would put the circle — which is what is
    /// actually drawn — somewhere other than where the webcam inset sits: a 3:4
    /// portrait's would float about 240 px above the corner at 1080p, and a
    /// phone screenshot's would run off the top of the frame.
    #[test]
    fn an_avatar_is_drawn_in_a_square_inset_whatever_its_shape() {
        let dir = dir();
        let square = avatar_box(layout::pip_rect(1920.0, 1080.0, 1.0));
        for (name, w, h) in [
            ("square.png", 96, 96),
            ("tall.png", 60, 80),
            ("wide.png", 160, 90),
        ] {
            let path = fixtures::still_image(dir.path(), name, w, h, StillFormat::Png);
            let avatar = open(&path, 1920.0, 1080.0).unwrap();
            assert_eq!(avatar.rect, square, "{name}: the inset is the square one");
            let image = &avatar.image;
            assert_eq!(image.width(), image.height(), "{name}: a square pixmap");
            assert!(
                f64::from(image.width()) >= square.w && f64::from(image.width()) < square.w + 1.0,
                "{name}: pixmap side {} against {}",
                image.width(),
                square.w
            );
            // And the circle is the one inscribed in that box, centred in it.
            let (c, last) = (image.width() / 2, image.width() - 1);
            let alpha = |x: u32, y: u32| image.pixel(x, y).expect("inside the pixmap").alpha();
            assert!(alpha(c, c) > 0, "{name}: the middle is drawn");
            for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
                assert_eq!(alpha(x, y), 0, "{name}: the corner ({x}, {y}) is cut away");
            }
            for (x, y) in [(c, 2), (c, last - 2), (2, c), (last - 2, c)] {
                assert!(alpha(x, y) > 0, "{name}: the circle reaches ({x}, {y})");
            }
        }
    }

    /// A still whose middle half — in both axes — is opaque red and whose
    /// edges are opaque green, so a test can say which part of an image
    /// survived the crop.
    fn banded(w: u32, h: u32) -> Still {
        let middle = |v: u32, of: u32| (of / 4..of - of / 4).contains(&v);
        let rgba = (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| match middle(x, w) && middle(y, h) {
                    true => [255, 0, 0, 255],
                    false => [0, 255, 0, 255],
                })
            })
            .collect();
        Still { w, h, rgba }
    }

    /// The image is **cover-cropped** into that square box: scaled until its
    /// shorter side fills it, then centred, so the middle of the picture is
    /// what survives (spec A5).
    ///
    /// The fixture's middle half is red. A 60×80 portrait covering a box keeps
    /// rows 10..70, so the red band spans two thirds of the box; squashed to
    /// fit it would span a half. A fifth of the way down is inside the band one
    /// way and outside it the other, and the same holds across a 160×90
    /// landscape — so one point either side of the middle tells the two apart.
    #[test]
    fn a_non_square_avatar_is_cropped_to_its_middle() {
        let side = 120;
        for (what, still) in [
            ("a portrait", banded(60, 80)),
            ("a landscape", banded(160, 90)),
        ] {
            let image = circular(&still, side).unwrap();
            let red = |x: u32, y: u32| {
                let px = image.pixel(x, y).expect("inside the pixmap");
                px.red() > 200 && px.green() < 60
            };
            let (c, near, far) = (side / 2, side / 5, side * 4 / 5);
            assert!(
                red(c, c),
                "{what}: the middle of the image is in the middle"
            );
            // Along the axis that was cropped, both.
            let (a, b) = match still.w < still.h {
                true => ((c, near), (c, far)),
                false => ((near, c), (far, c)),
            };
            for (x, y) in [a, b] {
                assert!(
                    red(x, y),
                    "{what}: ({x}, {y}) is outside the middle band, so the image was \
                     squashed to fit rather than cropped to cover"
                );
            }
        }
    }

    #[test]
    fn a_missing_image_is_a_message_not_a_panic() {
        let dir = dir();
        assert!(open(&dir.path().join("gone.png"), 1920.0, 1080.0).is_err());
    }
}
