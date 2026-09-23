//! Where the self-view inset lands on the picture, and how big it is right
//! now (avatar spec G2).
//!
//! The corner over the player is where the export puts the inset, so it is
//! placed by core's [`pip_rect_over_picture`] and sized by core's
//! [`avatar_rect`] — the same two functions the render uses. Pure: the window
//! hands in the content rect and takes back a rect, so the rule is tested
//! without a display.

use video_coach_core::avatar::avatar_rect;
use video_coach_core::layout::{pip_rect_over_picture, Rect};

/// The self-view's rect over `picture`, for an inset of display aspect
/// `cam_aspect` at `level`.
///
/// **A camera take passes `level = 1.0`**, which [`avatar_rect`] maps to
/// exactly `pip_rect_over_picture`: one placement path, no branch, and the
/// camera's inset lands where it has always landed. An avatar take passes the
/// smoothed live level, and the picture breathes around that same centre.
///
/// `None` before the first layout, or with no picture to place on.
pub fn self_view_rect(picture: Rect, cam_aspect: f64, level: f64) -> Option<Rect> {
    (picture.w > 0.0 && picture.h > 0.0 && cam_aspect > 0.0)
        .then(|| avatar_rect(pip_rect_over_picture(picture, cam_aspect), level))
}

#[cfg(test)]
mod tests {
    use super::*;
    use video_coach_core::avatar::PULSE_GROWTH;

    /// A 16:9 picture, offset like a letterboxed player area.
    fn picture() -> Rect {
        Rect {
            x: 12.0,
            y: 30.0,
            w: 1600.0,
            h: 900.0,
        }
    }

    fn centre(r: Rect) -> (f64, f64) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn a_full_level_is_exactly_where_the_camera_goes() {
        let pip = pip_rect_over_picture(picture(), 16.0 / 9.0);
        let at = self_view_rect(picture(), 16.0 / 9.0, 1.0).expect("a picture to place on");
        assert_eq!((at.x, at.y, at.w, at.h), (pip.x, pip.y, pip.w, pip.h));
    }

    #[test]
    fn rest_is_concentric_and_smaller_by_the_growth() {
        let pip = pip_rect_over_picture(picture(), 1.0);
        let at = self_view_rect(picture(), 1.0, 0.0).expect("a picture to place on");
        assert!((at.w - pip.w / PULSE_GROWTH).abs() < 1e-9);
        assert!((at.h - pip.h / PULSE_GROWTH).abs() < 1e-9);
        let (cx, cy) = centre(pip);
        let (ax, ay) = centre(at);
        assert!((ax - cx).abs() < 1e-9 && (ay - cy).abs() < 1e-9);
    }

    #[test]
    fn it_grows_with_the_level_and_never_past_the_inset() {
        let pip = pip_rect_over_picture(picture(), 4.0 / 3.0);
        let mut last = 0.0;
        for step in 0..=10 {
            let at = self_view_rect(picture(), 4.0 / 3.0, f64::from(step) / 10.0)
                .expect("a picture to place on");
            assert!(at.w > last, "{} is not past {last}", at.w);
            assert!(at.w <= pip.w + 1e-9 && at.h <= pip.h + 1e-9);
            last = at.w;
        }
    }

    /// The live level comes off a filter fed by the microphone, so a
    /// non-number would otherwise reach the window as a `NaN` rect.
    #[test]
    fn a_wild_level_clamps() {
        let pip = pip_rect_over_picture(picture(), 1.0);
        for level in [f64::NAN, -3.0, 7.0, f64::INFINITY] {
            let at = self_view_rect(picture(), 1.0, level).expect("a picture to place on");
            assert!(at.x.is_finite() && at.y.is_finite() && at.w.is_finite() && at.h.is_finite());
            assert!(at.w >= pip.w / PULSE_GROWTH - 1e-9 && at.w <= pip.w + 1e-9);
        }
    }

    #[test]
    fn nothing_to_place_on_places_nothing() {
        let empty = Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        assert!(self_view_rect(empty, 16.0 / 9.0, 1.0).is_none());
        assert!(self_view_rect(picture(), 0.0, 1.0).is_none());
        assert!(self_view_rect(picture(), f64::NAN, 1.0).is_none());
    }
}
