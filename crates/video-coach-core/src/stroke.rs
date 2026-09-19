//! Freehand telestration strokes.
//!
//! Stroke coordinates are normalized **top-left** origin: capture flips out of
//! the platform's view space, and the compositor does not flip again.
//! `line_width` is normalized to frame **height**, not width.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Straight RGBA, components in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const RED: Rgba = Rgba {
        r: 1.0,
        g: 0.2,
        b: 0.2,
        a: 1.0,
    };
}

/// One sampled point of a stroke.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    /// 0...1 of frame width.
    pub x: f64,
    /// 0...1 of frame height.
    pub y: f64,
    /// Seconds since this stroke started.
    pub t: f64,
}

/// A complete freehand stroke.
///
/// The `.stroke` event that carries one is appended when the stroke
/// **finishes**, so replay back-computes its start time. See
/// `stroke_replay::visible_strokes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub id: Uuid,
    pub color: Rgba,
    /// Normalized to frame height.
    pub line_width: f64,
    pub points: Vec<StrokePoint>,
    /// `None` = persist until a `ClearAll`.
    pub auto_clear_after_seconds: Option<f64>,
}
