//! The player highlights on the **live** picture (spec H5): the rings the
//! window draws while scanning or recording, as Slint path commands.
//!
//! The geometry is core's [`highlight_shapes`] — the one function the preview
//! and export overlay draws from too — with the **content rect** as the
//! picture, since that is what the live layer is sized to and what strokes are
//! normalized against. So a ring on screen is the ring the export burns in,
//! with no second mapping to drift.
//!
//! The label's size, its pill's height and where that pill sits are core's
//! too; all this module adds is the ring's path string and the anchor the
//! window clamps once it knows how wide the shaped text came out.
//!
//! Pure code: the window hands in the sizes and takes back strings and
//! numbers, so every rule here is tested without a display.

use uuid::Uuid;
use video_coach_core::highlight::{
    highlight_shapes, label_ink, HighlightShape, NormRect, LABEL_PAD_RATIO, LABEL_PILL_RATIO,
};
use video_coach_core::project::Project;
use video_coach_core::stroke::Rgba;
use video_coach_core::zoom::Zoom;

/// One highlight as the live layer draws it, in **content-rect logical
/// pixels**.
///
/// Every number here is core's [`HighlightShape`], which is also what the
/// media overlay draws from: the window lays the pill out, but it decides
/// nothing about it beyond how wide the shaped text came out.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveHighlight {
    /// The ring, as SVG path commands for a `Path` with `fit: preserve`,
    /// which takes them as raw pixels.
    pub commands: String,
    pub ink: Rgba,
    /// Empty draws no pill.
    pub label: String,
    /// Black or white, whichever can be read on the pill.
    pub label_ink: Rgba,
    /// Where the pill is centred horizontally: the box's centre. The window
    /// clamps it to the content rect once it knows how wide the pill came
    /// out.
    pub label_x: f64,
    /// The pill's top edge, already placed above the box or below it and
    /// clamped to the content rect.
    pub label_y: f64,
    /// The label's font size.
    pub font_size: f64,
    /// The padding the pill keeps around its text on each side — the window's
    /// half of the width, which only a text shaper can finish.
    pub label_pad: f64,
    /// The pill's height.
    pub pill_h: f64,
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
    .filter(|shape| shape.is_drawable())
    .map(live)
    .collect()
}

/// One drawable shape's ring and pill.
fn live(shape: &HighlightShape) -> LiveHighlight {
    let (cx, cy, rx, ry) = shape.ellipse;
    let rect = shape.rect;
    LiveHighlight {
        commands: ring_commands(cx, cy, rx, ry),
        ink: shape.color,
        label: shape.label.clone(),
        label_ink: label_ink(shape.color),
        label_x: rect.x + rect.w / 2.0,
        label_y: shape.label_y,
        font_size: shape.font_size,
        label_pad: LABEL_PAD_RATIO * shape.font_size,
        pill_h: LABEL_PILL_RATIO * shape.font_size,
    }
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

// --------------------------------------------------------------- the H tool

/// How far from a highlight's key range a *selected* highlight still takes a
/// new key (spec H3), seconds. Past it the drag starts a new highlight, so a
/// selection left over from earlier in the match can never stretch one across
/// it.
pub const SELECTION_REACH: f64 = 10.0;

/// The box a drag from `press` to `release` draws, in **source-normalized**
/// coordinates: the key's rect.
///
/// Both are content-rect logical pixels, as the tool's touch area reports
/// them. Each corner goes to a content fraction and then through
/// [`Zoom::source_point`], so a box drawn while zoomed is stored where the
/// player is in the footage; core's `highlight_shapes` is the exact inverse.
///
/// Clamped to the picture — the pointer is grabbed on press, so a drag runs
/// off it — and `None` for a drag with no area, which is a click, or before
/// the first layout.
pub fn drag_rect(
    press: (f64, f64),
    release: (f64, f64),
    zoom: Zoom,
    content_w: f64,
    content_h: f64,
) -> Option<NormRect> {
    if !(content_w > 0.0 && content_h > 0.0) {
        return None;
    }
    let corner = |(x, y): (f64, f64)| {
        let (sx, sy) = zoom.source_point(
            (x / content_w).clamp(0.0, 1.0),
            (y / content_h).clamp(0.0, 1.0),
        );
        (sx.clamp(0.0, 1.0), sy.clamp(0.0, 1.0))
    };
    let ((x0, y0), (x1, y1)) = (corner(press), corner(release));
    let (x, w) = (x0.min(x1), x0.max(x1) - x0.min(x1));
    let (y, h) = (y0.min(y1), y0.max(y1) - y0.min(y1));
    // A NaN corner falls out here, as a zero-area drag does.
    (w > 0.0 && h > 0.0).then_some(NormRect { x, y, w, h })
}

/// The highlight a press at `(x, y)` lands on, in content-rect pixels, or
/// `None` for the bare picture.
///
/// Topmost first, which is the last of `shapes`: they are drawn in order, so
/// the one on top is the one the coach sees under the pointer.
pub fn hit_test(shapes: &[HighlightShape], x: f64, y: f64) -> Option<Uuid> {
    shapes.iter().rev().find(|s| hits(s, x, y)).map(|s| s.id)
}

/// Whether `(x, y)` is on a shape: inside the box, or inside the ring it
/// stands in. The ring is grown by its own stroke width, so a press on the
/// line itself counts.
fn hits(shape: &HighlightShape, x: f64, y: f64) -> bool {
    let r = shape.rect;
    if x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h {
        return true;
    }
    let (cx, cy, rx, ry) = shape.ellipse;
    let (rx, ry) = (rx + shape.width, ry + shape.width);
    if !(rx > 0.0 && ry > 0.0) {
        return false;
    }
    let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
    dx * dx + dy * dy <= 1.0
}

/// Which highlight a drag from `press` puts its key on (spec H3), or `None`
/// for a new one, which the caller gives a fresh id.
///
/// The ring under the press first — the coach is aiming at a player they can
/// see — then the selected highlight, if it is on this source and the frame
/// is within [`SELECTION_REACH`] of its keys.
pub fn target_for_drag(
    project: &Project,
    shapes: &[HighlightShape],
    selected: Option<Uuid>,
    source_index: usize,
    source_secs: f64,
    press: (f64, f64),
) -> Option<Uuid> {
    if let Some(id) = hit_test(shapes, press.0, press.1) {
        return Some(id);
    }
    let h = project
        .player_highlights
        .iter()
        .find(|h| Some(h.id) == selected)?;
    let (first, last) = (h.keys.first()?, h.keys.last()?);
    (h.source_index == source_index
        && source_secs >= first.source_seconds - SELECTION_REACH
        && source_secs <= last.source_seconds + SELECTION_REACH)
        .then_some(h.id)
}

/// Whether highlight `id` has a key on source `source_index` at exactly
/// `source_secs` — which "Delete key here" removes, and which is the number
/// that placed it (spec H2: keys sit at the displayed frame's stream time, so
/// one frame is one number).
///
/// The source is matched as well as the time, as [`target_for_drag`] matches
/// it: with another video on screen, a highlight left selected in the panel
/// must not offer its key for deletion at a coincidence of seconds.
pub fn has_key_at(project: &Project, id: Uuid, source_index: usize, source_secs: f64) -> bool {
    project
        .player_highlights
        .iter()
        .find(|h| h.id == id)
        .is_some_and(|h| {
            h.source_index == source_index && h.keys.iter().any(|k| k.source_seconds == source_secs)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use video_coach_core::highlight::{HighlightKey, PlayerHighlight};
    use video_coach_core::project::SourceRef;

    /// A 800 × 400 content rect throughout, so a fraction of the height is a
    /// round number: the font is 12 px, the pill 18.6 and the gap 3.
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
        // The box's top is 100 px down; the pill is 18.6 tall with a 3 px gap.
        assert_eq!(live[0].label_y, 100.0 - 3.0 - 18.6);
        assert_eq!(live[0].pill_h, 18.6);
        assert_eq!(live[0].font_size, 12.0);

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

    // ------------------------------------------------------- the H tool

    /// The same drag core's round-trip test uses
    /// (`a_box_drawn_while_zoomed_comes_back_where_it_was_drawn`): the box
    /// goes to source space through `Zoom::source_point`, so
    /// `highlight_shapes` brings it back at the pixels it was drawn at.
    #[test]
    fn a_drag_is_the_source_box_it_encloses() {
        let (cw, ch) = (1600.0, 900.0);
        let zoom = Zoom::new(2.5, 0.12, -0.08).clamped();
        let (x0, y0, x1, y1) = (520.0, 300.0, 680.0, 660.0);
        let corner = |x: f64, y: f64| zoom.source_point(x / cw, y / ch);
        let (sx0, sy0) = corner(x0, y0);
        let (sx1, sy1) = corner(x1, y1);

        let got = drag_rect((x0, y0), (x1, y1), zoom, cw, ch).expect("a box");
        assert_eq!(
            got,
            NormRect {
                x: sx0,
                y: sy0,
                w: sx1 - sx0,
                h: sy1 - sy0,
            }
        );
        // Dragged from the far corner it is the same box: the coach may drag
        // in any of the four directions.
        assert_eq!(drag_rect((x1, y1), (x0, y0), zoom, cw, ch), Some(got));
        assert_eq!(drag_rect((x0, y1), (x1, y0), zoom, cw, ch), Some(got));
    }

    /// The pointer is grabbed on press, so a drag runs off the picture; the
    /// box stops at its edge rather than storing a box outside the source.
    /// A drag with no area at all -- a click -- is no box.
    #[test]
    fn a_drag_clamps_to_the_picture_and_needs_an_area() {
        let (cw, ch) = CONTENT;
        assert_eq!(
            drag_rect((-500.0, -500.0), (5000.0, 5000.0), Zoom::IDENTITY, cw, ch),
            Some(NormRect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            })
        );
        assert_eq!(
            drag_rect((10.0, 10.0), (10.0, 90.0), Zoom::IDENTITY, cw, ch),
            None
        );
        // Nothing to normalize against before the first layout.
        assert_eq!(
            drag_rect((10.0, 10.0), (90.0, 90.0), Zoom::IDENTITY, 0.0, 0.0),
            None
        );
    }

    /// The 10% × 20% box of `project`, centred: 80 × 80 px at (360, 160), so
    /// the ring is centred (400, 240) with rx 56 and ry 19.6.
    fn centred() -> Project {
        project(
            NormRect {
                x: 0.45,
                y: 0.4,
                w: 0.1,
                h: 0.2,
            },
            "",
        )
    }

    fn shapes_at(p: &Project, secs: f64) -> Vec<HighlightShape> {
        highlight_shapes(
            &p.player_highlights,
            0,
            secs,
            Zoom::IDENTITY,
            CONTENT.0,
            CONTENT.1,
        )
    }

    #[test]
    fn a_press_on_a_ring_or_its_box_finds_the_highlight() {
        let shapes = shapes_at(&centred(), 10.0);
        // On the ring's near edge, inside it, and inside the box.
        assert_eq!(hit_test(&shapes, 344.0, 240.0), Some(Uuid::nil()));
        assert_eq!(hit_test(&shapes, 400.0, 245.0), Some(Uuid::nil()));
        assert_eq!(hit_test(&shapes, 400.0, 200.0), Some(Uuid::nil()));
        // Beyond the ring, and elsewhere on the picture.
        assert_eq!(hit_test(&shapes, 400.0, 300.0), None);
        assert_eq!(hit_test(&shapes, 100.0, 100.0), None);
    }

    /// A second highlight, on `source_index`, with a key at each of `keys`
    /// and a box in the top-left corner (nowhere near `centred`'s).
    fn with_other(p: &mut Project, source_index: usize, keys: &[f64]) -> Uuid {
        let id = Uuid::from_u128(7);
        p.player_highlights.push(PlayerHighlight {
            id,
            source_index,
            color: Rgba::RED,
            label: String::new(),
            keys: keys
                .iter()
                .map(|&source_seconds| HighlightKey {
                    source_seconds,
                    rect: NormRect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.05,
                        h: 0.05,
                    },
                    tracked: false,
                })
                .collect(),
        });
        id
    }

    /// A press on a ring wins, whatever is selected: the coach is aiming at
    /// the player they can see.
    #[test]
    fn a_press_on_a_ring_beats_the_selection() {
        let mut p = centred();
        let other = with_other(&mut p, 0, &[10.0, 12.0]);
        let shapes = shapes_at(&p, 10.0);
        assert_eq!(
            target_for_drag(&p, &shapes, Some(other), 0, 10.0, (400.0, 240.0)),
            Some(Uuid::nil())
        );
    }

    /// With nothing under the press, the drag extends the selected highlight
    /// -- but only within 10 s of its range, so a selection left over from
    /// earlier in the match never stretches one across it.
    #[test]
    fn a_stale_selection_does_not_stretch_a_highlight() {
        let mut p = Project::new("p");
        p.source_videos.push(SourceRef {
            relative_path: "0.mp4".into(),
            display_name: "0".into(),
            duration_seconds: 600.0,
            display_aspect: 16.0 / 9.0,
        });
        let id = with_other(&mut p, 0, &[20.0, 25.0]);
        let target = |secs: f64, selected: Option<Uuid>, source_index: usize| {
            target_for_drag(
                &p,
                &shapes_at(&p, secs),
                selected,
                source_index,
                secs,
                (700.0, 380.0),
            )
        };
        // Inside the range, and at either end of the 10 s reach.
        assert_eq!(target(22.0, Some(id), 0), Some(id));
        assert_eq!(target(35.0, Some(id), 0), Some(id));
        assert_eq!(target(10.0, Some(id), 0), Some(id));
        // Past it, on another source, and with nothing selected: a new one.
        assert_eq!(target(35.1, Some(id), 0), None);
        assert_eq!(target(22.0, Some(id), 1), None);
        assert_eq!(target(22.0, None, 0), None);
        // ... as for a selection that is no longer there at all.
        assert_eq!(target(22.0, Some(Uuid::from_u128(99)), 0), None);
    }

    /// "Delete key here" is offered only on a frame that carries a key, and
    /// the number it matches is the one that placed it.
    #[test]
    fn a_key_is_found_only_at_its_own_time_on_its_own_source() {
        let mut p = Project::new("p");
        let id = with_other(&mut p, 0, &[20.0, 25.0]);
        assert!(has_key_at(&p, id, 0, 20.0));
        assert!(has_key_at(&p, id, 0, 25.0));
        assert!(!has_key_at(&p, id, 0, 22.0));
        assert!(!has_key_at(&p, Uuid::from_u128(99), 0, 20.0));
        // Another video is on screen: the same second is a different frame,
        // and this highlight has no key on it.
        assert!(!has_key_at(&p, id, 1, 20.0));
    }
}
