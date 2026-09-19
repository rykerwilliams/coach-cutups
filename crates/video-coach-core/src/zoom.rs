//! Zoom and pan state.
//!
//! **Exactly one transform lives here, and it is not the one the macOS
//! compositors call.** The Swift original carries three:
//! `transform(sourceSize:destSize:)` (dead outside its own tests),
//! `deltaTransform(viewportSize:)` and `deltaTransformForCIImage(...)` (a sign
//! flip for Core Image's bottom-left origin). All three map the normalized
//! source point `0.5 + pan` to the viewport centre, but they disagree on base
//! fit (letterbox vs. pre-stretched) and on what `pan` is a fraction of
//! (displayed image vs. viewport). Since this port letterboxes, the surviving
//! formula is the letterbox-fit one. Both delta variants are deliberately
//! absent: reaching for `deltaTransform` because it is what the live
//! compositors call would get pan wrong on every source whose aspect ratio
//! differs from the output's.
//!
//! `PartialEq` here is **bit equality on purpose**. The recorder's zoom dedupe
//! is `if z == last_captured { return }`, and its intent is "the gesture fired
//! but `snapped().clamped()` collapsed to the same notch as last time". An
//! epsilon comparison would suppress genuinely distinct keyframes and break the
//! anchor-keyframe pattern that keeps replay from drifting across quiet gaps.

use serde::{Deserialize, Serialize};

/// Zoom scale plus pan, in normalized source coordinates.
///
/// `pan` is a fraction of the **displayed (letterboxed) source rect**, not of
/// the viewport. The visible centre is the normalized source point
/// `(0.5 + pan_x, 0.5 + pan_y)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Zoom {
    pub scale: f64,
    pub pan_x: f64,
    pub pan_y: f64,
}

/// Standard snap notches. Any UI tick marks must match these so the visible
/// track agrees with the snap behavior.
pub const SNAP_NOTCHES: [f64; 8] = [1.0, 1.25, 1.5, 2.0, 3.0, 5.0, 7.5, 10.0];

/// A 2D affine transform. Six fields; a geometry crate would violate the
/// no-unneeded-dependency rule for no benefit.
///
/// `b` and `c` are always zero here, so the transform is exactly the rect
/// `(tx, ty, src_w * a, src_h * d)` — the form both `gltransformation` and
/// tiny-skia want.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    /// Apply to a point.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        )
    }
}

impl Zoom {
    pub const IDENTITY: Zoom = Zoom {
        scale: 1.0,
        pan_x: 0.0,
        pan_y: 0.0,
    };

    pub fn new(scale: f64, pan_x: f64, pan_y: f64) -> Zoom {
        Zoom {
            scale,
            pan_x,
            pan_y,
        }
    }
}

impl Zoom {
    /// Hard floor 1.0x (never zoom out past the full frame), soft cap 10x.
    ///
    /// The pan limit narrows as scale approaches 1 and is forced to 0 at
    /// exactly 1. At `pan = ±(s−1)/(2s)` the visible window's edge lands
    /// exactly on the source edge, which is what lets the compositor treat
    /// zoom as a plain crop with no edge handling.
    #[allow(
        clippy::manual_clamp,
        reason = "clamp() panics when a bound is NaN; min/max degrade like Swift's"
    )]
    pub fn clamped(self) -> Zoom {
        // `f64::min`/`max` rather than `clamp`: `clamp` asserts `min <= max` and
        // so PANICS on a NaN scale (NaN.clamp gives NaN, the `<= 1.0` guard is
        // then false, and `pan.clamp(-NaN, NaN)` trips the assertion). These
        // return the non-NaN operand, which is bit-identical to the Swift
        // original for every input including NaN.
        let s = self.scale.min(10.0).max(1.0);
        if s <= 1.0 {
            return Zoom::IDENTITY;
        }
        let limit = (s - 1.0) / (2.0 * s);
        Zoom {
            scale: s,
            pan_x: self.pan_x.clamp(-limit, limit),
            pan_y: self.pan_y.clamp(-limit, limit),
        }
    }

    /// Snap `scale` to a notch within 3% relative tolerance; pan is preserved.
    ///
    /// Returns the **first** matching notch in table order. The table's spacing
    /// (≥20% between neighbours) makes the tolerance windows disjoint, so first
    /// and nearest coincide — but only by property of this table. Inserting a
    /// closely-spaced notch would silently change the semantics.
    ///
    /// Interactive commits only. Replay never snaps, or authored zoom values
    /// would not survive a round-trip.
    pub fn snapped(self) -> Zoom {
        for n in SNAP_NOTCHES {
            if (self.scale - n).abs() <= n * 0.03 {
                return Zoom { scale: n, ..self };
            }
        }
        self
    }

    /// Linear interpolation, with `alpha` clamped to `0..=1`.
    pub fn lerp(a: Zoom, b: Zoom, alpha: f64) -> Zoom {
        let t = alpha.clamp(0.0, 1.0);
        Zoom {
            scale: a.scale + (b.scale - a.scale) * t,
            pan_x: a.pan_x + (b.pan_x - a.pan_x) * t,
            pan_y: a.pan_y + (b.pan_y - a.pan_y) * t,
        }
    }

    /// The normalized source point currently visible at a position within the
    /// **displayed (letterboxed) source rect**.
    ///
    /// `content_x` / `content_y` are fractions of that rect, **not** of the
    /// viewport. A caller holding a window-space cursor must convert first —
    /// on a source whose aspect differs from the output's, the two differ.
    ///
    /// Inverse of the rendering transform: the visible window is `1/scale`
    /// wide, centred on `0.5 + pan`.
    pub fn source_point(self, content_x: f64, content_y: f64) -> (f64, f64) {
        (
            (0.5 + self.pan_x) + (content_x - 0.5) / self.scale,
            (0.5 + self.pan_y) + (content_y - 0.5) / self.scale,
        )
    }

    /// Change scale while keeping the source point under the cursor fixed.
    ///
    /// `content_x` / `content_y` carry the same meaning as in
    /// [`Zoom::source_point`].
    #[allow(
        clippy::manual_clamp,
        reason = "clamp() panics when a bound is NaN; min/max degrade like Swift's"
    )]
    pub fn zoomed_to_cursor(self, new_scale: f64, content_x: f64, content_y: f64) -> Zoom {
        let s2 = new_scale.min(10.0).max(1.0); // NaN-safe; see `clamped`
        if s2 <= 1.0 {
            return Zoom::IDENTITY;
        }
        let (sx, sy) = self.source_point(content_x, content_y);
        Zoom {
            scale: s2,
            pan_x: (sx - (content_x - 0.5) / s2) - 0.5,
            pan_y: (sy - (content_y - 0.5) / s2) - 0.5,
        }
        .clamped()
    }

    /// Map source-frame pixels onto output pixels, **letterbox-fitted**, in a
    /// top-left coordinate space.
    ///
    /// This is the one surviving transform (see the module comment). `pan` is a
    /// fraction of the *displayed source rect*, hence the `* src_w * s` term —
    /// dropping the `* s` gives a pan that drifts toward centre as scale rises.
    ///
    /// At [`Zoom::IDENTITY`] this is also the **content rect** that strokes
    /// denormalize against, so one function covers both needs.
    pub fn transform(self, src_w: f64, src_h: f64, out_w: f64, out_h: f64) -> Affine {
        let base = (out_w / src_w).min(out_h / src_h);
        let s = self.scale * base;
        Affine {
            a: s,
            b: 0.0,
            c: 0.0,
            d: s,
            tx: (out_w - src_w * s) / 2.0 - self.pan_x * src_w * s,
            ty: (out_h - src_h * s) / 2.0 - self.pan_y * src_h * s,
        }
    }
}

/// The zoom in effect at `record_time`, **linearly interpolated** between
/// adjacent keyframes.
///
/// Before the first keyframe it holds the first value; after the last it holds
/// the last; an empty track is identity.
///
/// The interpolation is why three recorder-side rules exist, and porting the
/// lookup without them produces visibly wrong replay:
///
/// 1. **Anchor keyframe.** When more than 100 ms has passed since the last
///    distinct capture, the recorder emits an extra keyframe at `t − 1 ms`
///    holding the *previous* value. Without it the lerp ramps smoothly across
///    a quiet gap instead of holding and then snapping.
/// 2. **Dedupe.** Captures equal to the last captured value are skipped.
/// 3. **No throttling.** Capturing at a reduced rate makes replay keyframe-
///    stepped while the coach saw a smooth picture, so a drawing made while
///    panning lands offset from what it was drawn on.
///
/// The scan stops at the first keyframe past `record_time`, so the event log
/// must be sorted.
pub fn zoom_at(events: &[crate::event::CommentaryEvent], record_time: f64) -> Zoom {
    use crate::event::EventKind;

    crate::event::debug_assert_sorted(events);

    let mut prev: Option<(f64, Zoom)> = None;
    let mut next: Option<(f64, Zoom)> = None;

    for e in events {
        let EventKind::Zoom(z) = e.kind else { continue };
        if e.record_time <= record_time {
            prev = Some((e.record_time, z));
        } else {
            next = Some((e.record_time, z));
            break;
        }
    }

    match (prev, next) {
        (None, None) => Zoom::IDENTITY,
        (Some((_, p)), None) => p,
        (None, Some((_, n))) => n,
        (Some((pt, p)), Some((nt, n))) => {
            let span = nt - pt;
            if span <= 0.0 {
                n
            } else {
                Zoom::lerp(p, n, (record_time - pt) / span)
            }
        }
    }
}
