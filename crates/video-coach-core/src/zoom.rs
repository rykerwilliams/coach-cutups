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
