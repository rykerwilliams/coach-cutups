//! Where the composite's furniture sits, as ratios of the frame it lands on.
//!
//! Every value here is a ratio, never a pixel count, so preview (1280×720) and
//! export (1920×1080) lay out identically from one set of numbers — the parent
//! spec's "Layout constants the port must reproduce". Phase 7 needs the stroke
//! and PiP rows; the text bar (Phase 8) and the scoreboard (Phase 9) join this
//! module when something draws them.
//!
//! **Two spaces, and they differ only on a non-16:9 source** (BACKLOG #20,
//! settled here the way the parent spec recommended):
//!
//! - **Strokes live in the content rect** — the letterboxed picture, which is
//!   [`crate::zoom::Zoom::IDENTITY`]'s `transform`. They were drawn on the
//!   picture, so `line_width` denormalizes against *its* height and the
//!   overlay is rasterized at *its* size. Against the output rect they would
//!   stretch across the letterbox bars.
//! - **The PiP lives in output space**, overlapping those bars like broadcast
//!   furniture. It is chrome: the coach never drew it, so nothing ties it to
//!   the picture.

/// An axis-aligned rectangle in pixels, top-left origin.
///
/// Sub-pixel by design: a mixer pad rounds it once, and rounding earlier would
/// make the same layout land differently at two output sizes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The webcam inset's width, as a fraction of the **output** width.
pub const PIP_WIDTH_RATIO: f64 = 0.22;

/// The webcam inset's gap from the bottom and right edges, as a fraction of
/// the **output height** — the same fraction on both axes, so the gap is
/// square in pixels rather than matching the frame's aspect.
pub const PIP_MARGIN_RATIO: f64 = 0.022;

/// The webcam inset's rect in output space, flush to the bottom-right corner.
///
/// Width comes from the output; height comes from `cam_aspect`, so the camera
/// is never stretched or letterboxed inside the inset. `cam_aspect` is the
/// **display** aspect (width ÷ height with the pixel aspect ratio applied) and
/// must be positive: a camera reporting neither is not a camera.
pub fn pip_rect(out_w: f64, out_h: f64, cam_aspect: f64) -> Rect {
    let w = PIP_WIDTH_RATIO * out_w;
    let h = w / cam_aspect;
    let margin = PIP_MARGIN_RATIO * out_h;
    Rect {
        x: out_w - margin - w,
        y: out_h - margin - h,
        w,
        h,
    }
}

/// A stroke's line width in pixels, from [`crate::stroke::Stroke::line_width`].
///
/// `picture_h` is the **content rect's** height, not the output frame's (see
/// the module comment). Height on both, never width: a stroke keeps its weight
/// when the picture's aspect changes, and the two axes would otherwise give a
/// pen that is oval rather than round.
pub fn stroke_line_width(line_width: f64, picture_h: f64) -> f64 {
    line_width * picture_h
}
