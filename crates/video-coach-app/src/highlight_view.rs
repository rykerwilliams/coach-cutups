//! The player highlights on the **live** picture (spec H5): the rings the
//! window draws while scanning or recording, as Slint path commands.
//!
//! The geometry is core's [`highlight_shapes`] — the one function the preview
//! and export overlay draws from too — with the **content rect** as the
//! picture, since that is what the live layer is sized to and what strokes are
//! normalized against. So a ring on screen is the ring the export burns in,
//! with no second mapping to drift.
//!
//! Pure code: the window hands in the sizes and takes back strings and
//! numbers, so every rule here is tested without a display.

use video_coach_core::highlight::{highlight_shapes, HighlightShape};
use video_coach_core::project::Project;
use video_coach_core::stroke::Rgba;
use video_coach_core::zoom::Zoom;

/// A label's size, as a fraction of the content rect's height. The overlay's
/// `LABEL_FONT_RATIO`, so the live pill reads at the size export burns in.
const LABEL_FONT_RATIO: f64 = 0.03;

/// The pill's height, as a multiple of the font size. The overlay measures its
/// own from the font it rasterizes with; here the window lays the pill out
/// while this module decides whether it goes above the box or below, so the
/// two have to agree on one number — `app.slint`'s `HighlightLabel` uses this
/// same ratio.
const LABEL_PILL_RATIO: f64 = 1.5;

/// The gap between the pill and the box, as a fraction of the font size (the
/// overlay's `LABEL_GAP_RATIO`).
const LABEL_GAP_RATIO: f64 = 0.25;

/// One highlight as the live layer draws it, in **content-rect logical
/// pixels**.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveHighlight {
    /// The ring, as SVG path commands for a `Path` with `fit: preserve`,
    /// which takes them as raw pixels.
    pub commands: String,
    pub ink: Rgba,
    /// Empty draws no pill.
    pub label: String,
    /// Where the pill is centred horizontally: the box's centre. The window
    /// clamps it to the content rect once it knows how wide the pill came
    /// out.
    pub label_x: f64,
    /// The pill's top edge, already placed above the box or below it and
    /// clamped to the content rect.
    pub label_y: f64,
}

/// The highlights showing at `source_secs` of source `source_index`, drawn on
/// a content rect of `content_w` × `content_h` logical pixels showing `zoom`.
///
/// Empty before the first layout, when nothing shows at that instant, or when
/// a stored box is corrupt (BACKLOG #28).
pub fn live_highlights(
    project: &Project,
    source_index: usize,
    source_secs: f64,
    zoom: Zoom,
    content_w: f64,
    content_h: f64,
) -> Vec<LiveHighlight> {
    if !(content_w > 0.0 && content_h > 0.0) {
        return Vec::new();
    }
    highlight_shapes(
        &project.player_highlights,
        source_index,
        source_secs,
        zoom,
        content_w,
        content_h,
    )
    .iter()
    .filter_map(|shape| live(shape, content_h))
    .collect()
}

/// One shape's ring and pill, or `None` if its geometry isn't drawable.
fn live(shape: &HighlightShape, content_h: f64) -> Option<LiveHighlight> {
    let (cx, cy, rx, ry) = shape.ellipse;
    let rect = shape.rect;
    let finite = [cx, cy, rx, ry, rect.x, rect.y, rect.w, rect.h]
        .iter()
        .all(|v| v.is_finite());
    if !finite || rx <= 0.0 || ry <= 0.0 {
        return None;
    }
    // Above the box, or below it when there is no room — the overlay's rule
    // (`draw_highlight_label`), for the same reason: half a shirt number is a
    // different shirt number, so the pill is moved rather than clipped.
    let font = LABEL_FONT_RATIO * content_h;
    let pill_h = LABEL_PILL_RATIO * font;
    let gap = LABEL_GAP_RATIO * font;
    let above = rect.y - gap - pill_h;
    let y = if above >= 0.0 {
        above
    } else {
        rect.y + rect.h + gap
    };
    Some(LiveHighlight {
        commands: ring_commands(cx, cy, rx, ry),
        ink: shape.color,
        label: shape.label.clone(),
        label_x: rect.x + rect.w / 2.0,
        label_y: y.clamp(0.0, (content_h - pill_h).max(0.0)),
    })
}

/// The ring as **two** SVG arcs, left point to right point and back: one arc
/// can't close an ellipse, since a 360° sweep has no distinct end point and
/// draws nothing.
///
/// Coordinates are rounded to hundredths of a logical pixel, as a stroke's
/// are ([`crate::drawing`]): nothing finer is renderable, Slint re-parses the
/// string on every set, and the tick only sets the model when it changed —
/// which sub-pixel jitter would defeat.
fn ring_commands(cx: f64, cy: f64, rx: f64, ry: f64) -> String {
    let (left, right) = (cx - rx, cx + rx);
    format!(
        "M {left:.2} {cy:.2} A {rx:.2} {ry:.2} 0 0 1 {right:.2} {cy:.2} \
         A {rx:.2} {ry:.2} 0 0 1 {left:.2} {cy:.2}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use video_coach_core::highlight::{HighlightKey, NormRect, PlayerHighlight};
    use video_coach_core::project::SourceRef;

    /// A 800 × 400 content rect throughout, so a fraction of the height is a
    /// round number: the font is 12 px, the pill 18 and the gap 3.
    const CONTENT: (f64, f64) = (800.0, 400.0);

    /// One project with one source and one highlight, a single key at 10 s
    /// holding `rect`.
    fn project(rect: NormRect, label: &str) -> Project {
        let mut p = Project::new("p");
        p.source_videos.push(SourceRef {
            relative_path: "0.mp4".into(),
            display_name: "0".into(),
            duration_seconds: 600.0,
            display_aspect: 16.0 / 9.0,
        });
        p.player_highlights.push(PlayerHighlight {
            id: Uuid::nil(),
            source_index: 0,
            color: Rgba::RED,
            label: label.to_string(),
            keys: vec![HighlightKey {
                source_seconds: 10.0,
                rect,
                tracked: false,
            }],
        });
        p
    }

    fn live_at(p: &Project, secs: f64, zoom: Zoom) -> Vec<LiveHighlight> {
        live_highlights(p, 0, secs, zoom, CONTENT.0, CONTENT.1)
    }

    /// A box in the middle of the frame rings the middle of the content rect:
    /// the ellipse is centred on the box's bottom edge, 1.4 × its width
    /// across.
    #[test]
    fn a_centred_box_rings_the_contents_centre() {
        // 10% wide, 20% tall, centred: 80 × 80 px at (360, 160).
        let p = project(
            NormRect {
                x: 0.45,
                y: 0.4,
                w: 0.1,
                h: 0.2,
            },
            "",
        );
        let live = live_at(&p, 10.0, Zoom::IDENTITY);
        assert_eq!(live.len(), 1);
        // cx = 400, cy = 240 (the box's feet), rx = 1.4 × 80 / 2 = 56,
        // ry = 0.35 × 56 = 19.6.
        assert_eq!(
            live[0].commands,
            "M 344.00 240.00 A 56.00 19.60 0 0 1 456.00 240.00 \
             A 56.00 19.60 0 0 1 344.00 240.00"
        );
        assert_eq!(live[0].ink, Rgba::RED);
    }

    /// The change check in `tick` compares the model it built last time, so
    /// one instant must always give one string.
    #[test]
    fn the_same_inputs_give_byte_equal_commands() {
        let p = project(
            NormRect {
                x: 0.3137,
                y: 0.2718,
                w: 0.0841,
                h: 0.1421,
            },
            "#7",
        );
        let zoom = Zoom::new(2.5, 0.1, -0.05);
        assert_eq!(live_at(&p, 10.0, zoom), live_at(&p, 10.0, zoom));
    }

    /// The pill goes above the box, and below it when the box is at the top
    /// of the picture — the overlay's rule, so neither is clipped away.
    #[test]
    fn the_label_sits_above_the_box_unless_theres_no_room() {
        let p = project(
            NormRect {
                x: 0.45,
                y: 0.25,
                w: 0.1,
                h: 0.25,
            },
            "#7",
        );
        let live = live_at(&p, 10.0, Zoom::IDENTITY);
        assert_eq!(live[0].label, "#7");
        assert_eq!(live[0].label_x, 400.0);
        // The box's top is 100 px down; the pill is 18 tall with a 3 px gap.
        assert_eq!(live[0].label_y, 100.0 - 3.0 - 18.0);

        let p = project(
            NormRect {
                x: 0.45,
                y: 0.0,
                w: 0.1,
                h: 0.1,
            },
            "#7",
        );
        let live = live_at(&p, 10.0, Zoom::IDENTITY);
        // No room above, so it goes under the box's bottom edge (40 px).
        assert_eq!(live[0].label_y, 40.0 + 3.0);
    }

    /// Nothing shows outside a lone key's span, and nothing at all before the
    /// first layout.
    #[test]
    fn nothing_shows_outside_the_span_or_before_a_layout() {
        let p = project(
            NormRect {
                x: 0.45,
                y: 0.4,
                w: 0.1,
                h: 0.2,
            },
            "",
        );
        assert!(live_at(&p, 12.0, Zoom::IDENTITY).is_empty());
        assert!(live_highlights(&p, 0, 10.0, Zoom::IDENTITY, 0.0, 0.0).is_empty());
        // ... nor on another source.
        assert!(live_highlights(&p, 1, 10.0, Zoom::IDENTITY, CONTENT.0, CONTENT.1).is_empty());
    }

    /// A corrupt box is skipped rather than drawn as a guess (BACKLOG #28).
    #[test]
    fn a_degenerate_box_draws_nothing() {
        let p = project(
            NormRect {
                x: 0.5,
                y: 0.5,
                w: 0.0,
                h: 0.1,
            },
            "",
        );
        assert!(live_at(&p, 10.0, Zoom::IDENTITY).is_empty());
    }
}
