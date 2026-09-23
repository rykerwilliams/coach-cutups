//! The avatar image (avatar spec A3, A5): one decoder for it, and the
//! pre-scaled, premultiplied, circular pixmap the overlay blits per frame.
//!
//! **One decoder.** [`decode_still`] is the whole of it — the pick validates
//! with it, the Devices popover's thumbnail and the recording corner draw
//! with it, and [`open`] scales its output into the pixmap. So a file that
//! passes the pick cannot fail in an export, and nothing has to keep a list
//! of extensions in step: what decoded is what is accepted.
//!
//! **Straight alpha in, premultiplied out.** GStreamer's `RGBA` is straight
//! alpha; tiny-skia stores premultiplied pixels (`overlay.rs` says exactly
//! this about the layer it draws into). A memcpy would leave a cut-out PNG's
//! soft edges at full colour — a bright halo around every soft pixel — so the
//! copy multiplies each of R, G and B by A.

use std::path::Path;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use tiny_skia::{FillRule, FilterQuality, Mask, PathBuilder, Pixmap, PixmapPaint, Transform};
use video_coach_core::layout::{self, Rect as LayoutRect};

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

/// The avatar as the overlay draws it.
// Until the overlay's draw step lands (the render task), the tests below are
// this type's only caller; the allow goes with that step.
#[allow(dead_code)]
pub(super) struct Avatar {
    /// Pre-scaled to the inset's size, premultiplied, and masked to the
    /// circle inscribed in it, so the per-frame draw is a plain blit.
    pub(super) image: Pixmap,
    /// [`layout::pip_rect`] for the image's own aspect: the inset's footprint
    /// at its loudest. The aspect is not stored separately — this rect
    /// already carries it.
    pub(super) rect: LayoutRect,
}

/// Decodes `path` and prepares it for an `out_w`×`out_h` run.
///
/// The image keeps its own shape — a square gravatar stays square, and
/// nothing is stretched or cropped to the inset — because [`layout::pip_rect`]
/// is given the image's aspect, exactly as it is given a camera's. It is then
/// masked to the circle inscribed in that box (spec A5), **once, here**: the
/// mask is the same size for every frame of the run, so building it per frame
/// would buy nothing and cost a rasterization.
#[allow(dead_code)]
pub(super) fn open(path: &Path, out_w: f64, out_h: f64) -> Result<Avatar, String> {
    let still = decode_still(path)?;
    let native = premultiplied(&still)?;
    let rect = layout::pip_rect(out_w, out_h, f64::from(still.w) / f64::from(still.h));
    // Rounded **up**, so the drawn box is never short of the rect it stands
    // for; everything after this reads the small pixmap.
    let mut image = Pixmap::new(rect.w.ceil() as u32, rect.h.ceil() as u32)
        .ok_or_else(|| format!("the inset is not a usable size ({}x{})", rect.w, rect.h))?;
    image.draw_pixmap(
        0,
        0,
        native.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        },
        Transform::from_scale(
            image.width() as f32 / still.w as f32,
            image.height() as f32 / still.h as f32,
        ),
        None,
    );
    mask_to_circle(&mut image);
    Ok(Avatar { image, rect })
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
        let path = fixtures::translucent_png(dir.path(), "half.png", 64, 64);
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

    #[test]
    fn an_avatar_is_prescaled_to_the_inset() {
        let dir = dir();
        for (name, w, h) in [("square.png", 96, 96), ("tall.png", 60, 80)] {
            let path = fixtures::still_image(dir.path(), name, w, h, StillFormat::Png);
            let avatar = open(&path, 1920.0, 1080.0).unwrap();
            let expected = layout::pip_rect(1920.0, 1080.0, f64::from(w) / f64::from(h));
            assert_eq!(avatar.rect, expected, "{name}");
            let image = &avatar.image;
            assert!(
                f64::from(image.width()) >= expected.w
                    && f64::from(image.width()) < expected.w + 1.0,
                "{name}: pixmap width {} against {}",
                image.width(),
                expected.w
            );
            assert!(
                f64::from(image.height()) >= expected.h
                    && f64::from(image.height()) < expected.h + 1.0,
                "{name}: pixmap height {} against {}",
                image.height(),
                expected.h
            );
        }
    }

    #[test]
    fn a_missing_image_is_a_message_not_a_panic() {
        let dir = dir();
        assert!(open(&dir.path().join("gone.png"), 1920.0, 1080.0).is_err());
    }
}
