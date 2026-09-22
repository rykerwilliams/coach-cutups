//! Player highlights: a ring at one player's feet, drawn on the footage
//! (spec H).
//!
//! **A highlight belongs to the footage, not to a clip** (H1). It is stored on
//! the project and positioned by `source_index` and source seconds, like a
//! match event, so it shows wherever that footage is on screen: scanning,
//! recording, a preview, every clip export that crosses it, and the reel. A
//! pen stroke is the opposite — it lives in a clip's event log, because it is
//! part of the commentary — and that is why a highlight may be placed outside
//! a recording while a drawing may not.
//!
//! **A key's `source_seconds` is the stream time of the frame it was placed
//! on** (H3), which is the key `Decoder::frame_at` answers with in export and
//! the key the match clock runs on ([`crate::export::FrameSpec::source_time`]).
//! So two keys on one frame are the same number — a replace needs no tolerance
//! — and a highlight freezes with the footage through a commentary pause
//! instead of drifting with the record clock (BACKLOG #27's failure, avoided
//! by construction).
//!
//! **[`highlight_shapes`] is the only drawing geometry**, shared by the media
//! overlay and the live Slint layer, and it maps through
//! [`Zoom::transform`](crate::zoom::Zoom::transform) — the same affine the
//! picture itself is drawn with, so a ring cannot drift from it. Strokes stay
//! zoom-agnostic (they live in the content rect and deliberately don't move
//! with the zoom); a highlight lives in source space and must move with it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::layout::Rect;
use crate::project::Project;
use crate::stroke::Rgba;
use crate::zoom::Zoom;

// ------------------------------------------------------------- stored types

/// A rectangle as fractions of the **source** frame, top-left origin.
///
/// Source-normalized, never output space: the box has to survive a zoom, a
/// letterbox and two output sizes, and the source frame is the one space all
/// three agree on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl NormRect {
    fn lerp(a: NormRect, b: NormRect, t: f64) -> NormRect {
        let at = |a: f64, b: f64| a + (b - a) * t;
        NormRect {
            x: at(a.x, b.x),
            y: at(a.y, b.y),
            w: at(a.w, b.w),
            h: at(a.h, b.h),
        }
    }
}

/// Where a highlight's box is at one instant of the source video.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightKey {
    /// The stream time of the frame this key was placed on — see the module
    /// comment.
    pub source_seconds: f64,
    pub rect: NormRect,
    /// `false` for the coach's own keys, which is every key in P2. It ships
    /// with the struct so that P6's tracker can re-track a stretch without
    /// touching a hand-placed key, and needs no format change to do it. Like
    /// every field of a new struct it is required, never defaulted (spec F2).
    pub tracked: bool,
}

/// One ringed player, on one source video.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerHighlight {
    pub id: Uuid,
    pub source_index: usize,
    /// Stored as a colour, not a pen, like a stroke: the swatch row is a UI
    /// convenience, and a project must draw the same ring on a build whose
    /// swatches differ.
    pub color: Rgba,
    /// `"#7"`; empty draws no label. Set through
    /// [`HighlightEdit::Label`], which normalizes it.
    pub label: String,
    /// Sorted by `source_seconds`, and **never empty**: deleting the last key
    /// deletes the highlight.
    ///
    /// **Not re-sorted on read.** The mutators here are the only way keys are
    /// added, and they keep the order; [`highlights_at`] relies on it. A
    /// hand-edited file that breaks the order draws an odd ring, which is
    /// cheaper than sorting a list on every load to guard against an edit
    /// nobody makes.
    pub keys: Vec<HighlightKey>,
}

/// One field of a highlight and its new value.
#[derive(Debug, Clone, PartialEq)]
pub enum HighlightEdit {
    Label(String),
    Color(Rgba),
}

/// [`Project::set_highlight_key`] refused; the project is unchanged.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightError {
    #[error("the highlight is on a different video")]
    OtherSource,
}

/// How long a highlight with a **single** key holds its box, centred on the
/// key (H2).
///
/// So it shows for the whole of a commentary pause on that frame, and for a
/// readable second when the footage plays through it. It is also why no
/// frame-matching tolerance is needed anywhere.
pub const SINGLE_KEY_SPAN: f64 = 1.0;

// --------------------------------------------------------------- mutations

/// A shirt number is shown as one: a label of digits only gets a leading `#`.
/// Everything else is just trimmed, so a name stays a name.
fn normalize_label(label: &str) -> String {
    let label = label.trim();
    if !label.is_empty() && label.chars().all(|c| c.is_ascii_digit()) {
        format!("#{label}")
    } else {
        label.to_string()
    }
}

impl Project {
    /// Place `key` on highlight `id`, creating the highlight if there is none.
    ///
    /// The caller generates the id, so the UI can select a new highlight the
    /// moment it sends the command. A new highlight takes `color` and an empty
    /// label; on an existing one `color` is ignored, because recolouring is
    /// [`HighlightEdit::Color`].
    ///
    /// A key at exactly the same `source_seconds` as an existing one
    /// **replaces** it: keys are placed at the displayed frame's stream time,
    /// so the same frame gives the same number. Keys stay sorted.
    ///
    /// Refuses a key on a source other than the highlight's own — a highlight
    /// is one player on one video.
    pub fn set_highlight_key(
        &mut self,
        id: Uuid,
        source_index: usize,
        color: Rgba,
        key: HighlightKey,
    ) -> Result<(), HighlightError> {
        let Some(h) = self.player_highlights.iter_mut().find(|h| h.id == id) else {
            self.player_highlights.push(PlayerHighlight {
                id,
                source_index,
                color,
                label: String::new(),
                keys: vec![key],
            });
            return Ok(());
        };
        if h.source_index != source_index {
            return Err(HighlightError::OtherSource);
        }
        match h
            .keys
            .binary_search_by(|k| k.source_seconds.total_cmp(&key.source_seconds))
        {
            Ok(i) => h.keys[i] = key,
            Err(i) => h.keys.insert(i, key),
        }
        Ok(())
    }

    /// Remove highlight `id`'s key at exactly `source_seconds` — the frame the
    /// coach is looking at, so the same number that placed it. Deleting the
    /// last key deletes the highlight.
    pub fn delete_highlight_key(&mut self, id: Uuid, source_seconds: f64) {
        let Some(h) = self.player_highlights.iter_mut().find(|h| h.id == id) else {
            return;
        };
        let Some(i) = h
            .keys
            .iter()
            .position(|k| k.source_seconds == source_seconds)
        else {
            return;
        };
        h.keys.remove(i);
        if h.keys.is_empty() {
            self.delete_highlight(id);
        }
    }

    pub fn delete_highlight(&mut self, id: Uuid) {
        self.player_highlights.retain(|h| h.id != id);
    }

    /// Set one field of highlight `id`; nothing happens if there is no such
    /// highlight. No inverse is returned: undo snapshots the whole list
    /// ([`UndoAction::EditHighlights`](crate::undo::UndoAction::EditHighlights)).
    pub fn edit_highlight(&mut self, id: Uuid, edit: HighlightEdit) {
        let Some(h) = self.player_highlights.iter_mut().find(|h| h.id == id) else {
            return;
        };
        match edit {
            HighlightEdit::Label(label) => h.label = normalize_label(&label),
            HighlightEdit::Color(color) => h.color = color,
        }
    }
}

// ------------------------------------------------------------ what's on screen

/// One highlight as it shows at an instant: its box in source-normalized
/// coordinates, ready to be mapped into a picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleHighlight<'a> {
    pub id: Uuid,
    pub color: Rgba,
    pub label: &'a str,
    pub rect: NormRect,
}

/// The box of highlight `keys` at `t`, or `None` if it doesn't show then.
///
/// Relies on `keys` being sorted, which the mutators guarantee.
fn rect_at(keys: &[HighlightKey], t: f64) -> Option<NormRect> {
    let (first, last) = (keys.first()?, keys.last()?);
    if keys.len() == 1 {
        let half = SINGLE_KEY_SPAN / 2.0;
        return ((t - first.source_seconds).abs() <= half).then_some(first.rect);
    }
    // The range is `[first key, last key]`; outside it nothing is drawn. The
    // negated form also rejects a NaN `t`.
    if !(t >= first.source_seconds && t <= last.source_seconds) {
        return None;
    }
    // `partition_point` counts the keys at or before `t`, so it is at least 1
    // here, and equals `len()` only at the last key.
    let i = keys.partition_point(|k| k.source_seconds <= t);
    let Some(b) = keys.get(i) else {
        return Some(last.rect);
    };
    let a = &keys[i - 1];
    let span = b.source_seconds - a.source_seconds;
    Some(if span > 0.0 {
        NormRect::lerp(a.rect, b.rect, (t - a.source_seconds) / span)
    } else {
        b.rect
    })
}

/// Every highlight showing at `t` seconds of source `source_index`, in stored
/// order.
///
/// With two or more keys a highlight shows over `[first key, last key]`, its
/// rect linearly interpolated between the keys either side. With one key it
/// shows over [`SINGLE_KEY_SPAN`] centred on that key, holding its box.
pub fn highlights_at(
    highlights: &[PlayerHighlight],
    source_index: usize,
    t: f64,
) -> Vec<VisibleHighlight<'_>> {
    highlights
        .iter()
        .filter(|h| h.source_index == source_index)
        .filter_map(|h| {
            Some(VisibleHighlight {
                id: h.id,
                color: h.color,
                label: &h.label,
                rect: rect_at(&h.keys, t)?,
            })
        })
        .collect()
}

// ---------------------------------------------------------------- geometry

/// The ring's width, as a multiple of the box's width: a little wider than the
/// player, so the ring reads as ground under them rather than a box around
/// their feet.
const RING_WIDTH_RATIO: f64 = 1.4;

/// The ring's height, as a multiple of **its own** width — a flat ellipse, the
/// foreshortening of a circle on the pitch seen from a raised camera.
const RING_ASPECT: f64 = 0.35;

/// The ring's stroke width, as a fraction of the picture's height. The pen's
/// own width (`drawing::LINE_WIDTH`), so a ring reads like a drawn ellipse at
/// every output size.
const RING_LINE_WIDTH: f64 = 0.005;

/// One highlight's drawing geometry, in **picture pixels** relative to the
/// picture's origin. The overlay and the live layer both draw from this, so
/// neither needs to know about the zoom.
#[derive(Debug, Clone, PartialEq)]
pub struct HighlightShape {
    pub id: Uuid,
    pub color: Rgba,
    /// Empty draws no label.
    pub label: String,
    /// The box the coach drew, mapped into the picture.
    pub rect: Rect,
    /// The ring at the player's feet: `(cx, cy, rx, ry)`.
    pub ellipse: (f64, f64, f64, f64),
    /// The stroke width for the ring.
    pub width: f64,
}

/// The ring for a box: an ellipse centred on the box's bottom edge.
fn highlight_ring(rect: Rect) -> (f64, f64, f64, f64) {
    let rx = RING_WIDTH_RATIO * rect.w / 2.0;
    (rect.x + rect.w / 2.0, rect.y + rect.h, rx, RING_ASPECT * rx)
}

/// The shapes to draw at `t` seconds of source `source_index`, on a picture
/// `picture_w` × `picture_h` pixels showing `zoom`.
///
/// The picture **is** the fitted source, so the source's own pixel size is
/// never needed: `zoom.transform(pw, ph, pw, ph)` is the same affine the
/// picture is drawn with, applied to a box denormalized against the picture.
/// That is deliberate — a second mapping function is a second thing to drift.
pub fn highlight_shapes(
    highlights: &[PlayerHighlight],
    source_index: usize,
    t: f64,
    zoom: Zoom,
    picture_w: f64,
    picture_h: f64,
) -> Vec<HighlightShape> {
    let transform = zoom.transform(picture_w, picture_h, picture_w, picture_h);
    highlights_at(highlights, source_index, t)
        .into_iter()
        .map(|h| {
            let (x, y) = transform.apply(h.rect.x * picture_w, h.rect.y * picture_h);
            let rect = Rect {
                x,
                y,
                w: h.rect.w * picture_w * transform.a,
                h: h.rect.h * picture_h * transform.d,
            };
            HighlightShape {
                id: h.id,
                color: h.color,
                label: h.label.to_string(),
                rect,
                ellipse: highlight_ring(rect),
                width: RING_LINE_WIDTH * picture_h,
            }
        })
        .collect()
}
