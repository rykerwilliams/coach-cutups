//! Zoom and pan input for the player area (spec D9), and where the picture
//! sits at a given zoom. The math is core's [`Zoom`]; this module only maps
//! pointer and key input onto it, in the player area's logical pixels.
//!
//! **Sign convention.** Slint's scroll deltas are winit's line deltas × 60
//! (or its pixel deltas), and winit defines a positive delta as "the content
//! should move right / down". Slint's own `Flickable` moves its content by
//! `+delta`. So a scroll delta here is how far the **picture** moves, the way
//! a document scrolls (natural scrolling is the system's business: it flips
//! the delta before we see it). A drag delta is the same: the picture follows
//! the pointer. And Ctrl+scroll with a positive `dy` (wheel away from you,
//! "scroll up") zooms in, as in browsers.
//!
//! Every function takes finite input; the window drops non-finite values
//! before calling in (BACKLOG #28).

use video_coach_core::zoom::Zoom;

/// A drag pans only once the pointer has moved this far from the press,
/// logical pixels.
pub const DRAG_THRESHOLD: f64 = 4.0;

/// The displayed frame size and the player area it's fitted into.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    frame_w: f64,
    frame_h: f64,
    area_w: f64,
    area_h: f64,
}

/// A rectangle in the player area's coordinates, logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Viewport {
    /// `frame_*` is the frame's display size (pixel aspect ratio applied);
    /// only its shape matters. `None` unless every dimension is finite and
    /// positive, because the zoom math divides by all four (the area is empty
    /// before the first layout).
    pub fn new(frame_w: f64, frame_h: f64, area_w: f64, area_h: f64) -> Option<Viewport> {
        [frame_w, frame_h, area_w, area_h]
            .iter()
            .all(|v| v.is_finite() && *v > 0.0)
            .then_some(Viewport {
                frame_w,
                frame_h,
                area_w,
                area_h,
            })
    }

    /// Where the picture is drawn at `zoom`: `Zoom::transform`'s rect. At
    /// [`Zoom::IDENTITY`] this is the letterboxed content rect.
    pub fn picture(&self, zoom: Zoom) -> Rect {
        let t = zoom.transform(self.frame_w, self.frame_h, self.area_w, self.area_h);
        Rect {
            x: t.tx,
            y: t.ty,
            width: self.frame_w * t.a,
            height: self.frame_h * t.d,
        }
    }

    /// A point in the area as fractions of the content rect, clamped.
    fn fraction(&self, x: f64, y: f64) -> (f64, f64) {
        Zoom::content_fraction(x, y, self.frame_w, self.frame_h, self.area_w, self.area_h)
    }
}

/// Moves the picture by `(dx, dy)` area pixels: a fraction of the displayed
/// (zoomed) picture, since `pan` is one. A no-op at 1×, where there's
/// nothing to pan to.
pub fn panned(zoom: Zoom, vp: &Viewport, dx: f64, dy: f64) -> Zoom {
    if zoom.scale <= 1.0 {
        return zoom;
    }
    let shown = vp.picture(zoom);
    Zoom {
        pan_x: zoom.pan_x - dx / shown.width,
        pan_y: zoom.pan_y - dy / shown.height,
        ..zoom
    }
    .clamped()
}

/// A scroll over the player at `(x, y)`. Ctrl zooms by `1.1^(dy/60)` about
/// the pointer, so a wheel click (60 px) is 10% and fine touchpad deltas zoom
/// smoothly. Otherwise it pans; Shift turns vertical scrolling horizontal, as
/// Slint's `Flickable` does.
pub fn scrolled(
    zoom: Zoom,
    vp: &Viewport,
    (x, y): (f64, f64),
    dx: f64,
    dy: f64,
    ctrl: bool,
    shift: bool,
) -> Zoom {
    if ctrl {
        let (fx, fy) = vp.fraction(x, y);
        return zoom.zoomed_to_cursor(zoom.scale * 1.1f64.powf(dy / 60.0), fx, fy);
    }
    let (dx, dy) = if shift { (dy, dx) } else { (dx, dy) };
    panned(zoom, vp, dx, dy)
}

/// Keys `2` and `3`: scale by `delta` about the pointer, or about the
/// picture's centre when the pointer isn't over the player (it may be
/// anywhere, and the edge nearest it is no better a guess).
pub fn stepped(zoom: Zoom, vp: &Viewport, pointer: Option<(f64, f64)>, delta: f64) -> Zoom {
    let (fx, fy) = pointer.map_or((0.5, 0.5), |(x, y)| vp.fraction(x, y));
    zoom.zoomed_to_cursor(zoom.scale + delta, fx, fy)
}

/// A primary-button drag over the player, from its press.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragPan {
    press: (f64, f64),
    /// Where the pointer was when the picture last moved; `None` until the
    /// drag passes the threshold.
    last: Option<(f64, f64)>,
}

impl DragPan {
    pub fn new(x: f64, y: f64) -> DragPan {
        DragPan {
            press: (x, y),
            last: None,
        }
    }

    /// The pointer moved to `(x, y)`: how far to move the picture, if at all.
    /// Nothing until the pointer is [`DRAG_THRESHOLD`] from the press; then
    /// the whole distance from the press, so the grabbed point stays under
    /// the pointer, and after that each step.
    pub fn moved(&mut self, x: f64, y: f64) -> Option<(f64, f64)> {
        let from = match self.last {
            Some(last) => last,
            None if (x - self.press.0).hypot(y - self.press.1) >= DRAG_THRESHOLD => self.press,
            None => return None,
        };
        self.last = Some((x, y));
        Some((x - from.0, y - from.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16:9 in a 1000 × 1000 area: content rect (0, 218.75, 1000, 562.5).
    fn vp() -> Viewport {
        Viewport::new(1920.0, 1080.0, 1000.0, 1000.0).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn viewport_rejects_empty_and_non_finite() {
        assert!(Viewport::new(0.0, 1.0, 1.0, 1.0).is_none());
        assert!(Viewport::new(1.0, 1.0, 1.0, -1.0).is_none());
        assert!(Viewport::new(1.0, f64::NAN, 1.0, 1.0).is_none());
        assert!(Viewport::new(1.0, 1.0, f64::INFINITY, 1.0).is_none());
    }

    #[test]
    fn identity_picture_is_the_letterboxed_content_rect() {
        let r = vp().picture(Zoom::IDENTITY);
        assert!(close(r.x, 0.0) && close(r.y, 218.75));
        assert!(close(r.width, 1000.0) && close(r.height, 562.5));
    }

    #[test]
    fn zoomed_picture_is_centred_on_the_pan_point() {
        // 2×, centred: twice the size, same centre.
        let r = vp().picture(Zoom::new(2.0, 0.0, 0.0));
        assert!(close(r.width, 2000.0) && close(r.height, 1125.0));
        assert!(close(r.x + r.width / 2.0, 500.0) && close(r.y + r.height / 2.0, 500.0));
        // Panned right by a quarter of the picture: it sits a quarter further left.
        let s = vp().picture(Zoom::new(2.0, 0.25, 0.0));
        assert!(close(s.x, r.x - 500.0));
    }

    #[test]
    fn pan_moves_the_picture_by_the_delta() {
        let z = Zoom::new(2.0, 0.0, 0.0);
        let before = vp().picture(z);
        let after = vp().picture(panned(z, &vp(), 30.0, -20.0));
        assert!(close(after.x - before.x, 30.0));
        assert!(close(after.y - before.y, -20.0));
    }

    #[test]
    fn pan_is_a_no_op_at_1x_and_clamps_at_the_edge() {
        assert_eq!(panned(Zoom::IDENTITY, &vp(), 50.0, 50.0), Zoom::IDENTITY);
        let z = panned(Zoom::new(2.0, 0.0, 0.0), &vp(), -1e6, 1e6);
        assert_eq!((z.pan_x, z.pan_y), (0.25, -0.25));
    }

    #[test]
    fn ctrl_scroll_up_zooms_in_about_the_pointer() {
        // A wheel click up is dy = +60: × 1.1.
        let cursor = (750.0, 400.0);
        let z = scrolled(Zoom::IDENTITY, &vp(), cursor, 0.0, 60.0, true, false);
        assert!(close(z.scale, 1.1));
        // The source point under the pointer stays put.
        let (fx, fy) = vp().fraction(cursor.0, cursor.1);
        let (ax, ay) = Zoom::IDENTITY.source_point(fx, fy);
        let (bx, by) = z.source_point(fx, fy);
        assert!(close(ax, bx) && close(ay, by));
        // And back down.
        let back = scrolled(z, &vp(), cursor, 0.0, -60.0, true, false);
        assert!(close(back.scale, 1.0));
    }

    #[test]
    fn plain_scroll_pans_and_shift_swaps_axes() {
        let z = Zoom::new(2.0, 0.0, 0.0);
        assert_eq!(
            scrolled(z, &vp(), (0.0, 0.0), 10.0, 20.0, false, false),
            panned(z, &vp(), 10.0, 20.0)
        );
        assert_eq!(
            scrolled(z, &vp(), (0.0, 0.0), 10.0, 20.0, false, true),
            panned(z, &vp(), 20.0, 10.0)
        );
        assert_eq!(
            scrolled(Zoom::IDENTITY, &vp(), (0.0, 0.0), 10.0, 20.0, false, false),
            Zoom::IDENTITY
        );
    }

    #[test]
    fn key_steps_about_the_pointer_or_the_centre() {
        let z = stepped(Zoom::IDENTITY, &vp(), None, 0.25);
        assert_eq!(z, Zoom::new(1.25, 0.0, 0.0));
        // The top-left corner of the content stays put.
        let z = stepped(Zoom::IDENTITY, &vp(), Some((0.0, 218.75)), 0.25);
        let r = vp().picture(z);
        assert!(close(r.x, 0.0) && close(r.y, 218.75));
        // Stepping down from 1.25 lands on identity.
        assert_eq!(stepped(z, &vp(), None, -0.25), Zoom::IDENTITY);
    }

    #[test]
    fn drag_waits_for_the_threshold_then_follows_the_pointer() {
        let mut d = DragPan::new(100.0, 100.0);
        assert_eq!(d.moved(102.0, 102.0), None); // 2.8 px
        assert_eq!(d.moved(103.0, 103.0), Some((3.0, 3.0))); // 4.2 px: all of it
        assert_eq!(d.moved(101.0, 103.0), Some((-2.0, 0.0))); // then each step
        assert_eq!(d.moved(101.0, 103.0), Some((0.0, 0.0)));
    }
}
