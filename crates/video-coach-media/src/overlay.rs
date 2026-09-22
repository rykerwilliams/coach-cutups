//! The vector overlay layer, rasterized once per output frame (spec P4, E2).
//!
//! Phase 8 draws the strokes and the text bar; Phase 9's scoreboard joins them,
//! **last, over everything** (spec S3), and the player highlights go under all
//! of it (spec H5). Geometry stays in core ([`video_coach_core::layout`] and
//! [`video_coach_core::highlight`]), pixels stay here.
//!
//! **The rect is the output frame; the strokes are mapped into the picture.**
//! One layer carries both, because they belong to different spaces: the coach
//! drew on the picture, so a stroke is normalized to the letterboxed content
//! rect and its pen denormalizes against *that* height, while the bar is chrome
//! and belongs to the frame. Splitting them across two mixer pads would buy
//! nothing — the extra pad is free either way (measured) — and would put the
//! bar's background and its glyphs on different layers, which is exactly the
//! macOS arrangement the raised PiP exists to remove.
//!
//! **The fonts are vendored** (`fonts/DejaVuSans*.ttf`, with their licence
//! beside them) and they are the only fonts loaded, because [`font_system`]
//! builds the database itself rather than letting `cosmic-text` scan the
//! machine. So an export looks the same here, on another coach's laptop and on
//! a CI runner with no fonts installed at all. Both faces load under the one
//! family, so **every [`Attrs`] states its weight**: the default would draw the
//! scoreboard's labels in whichever face the query happened to reach.
//!
//! **Premultiplied, which the mixer pad has to be told.** tiny-skia stores
//! premultiplied pixels; GStreamer's `RGBA` means straight alpha. Nothing here
//! demultiplies — the mixer pad carrying this buffer sets
//! `blend-function-src-rgb=one` instead (the destination function already
//! defaults to `one-minus-src-alpha`), which is premultiplied-over for free on
//! the GPU.
//!
//! **A fresh buffer per frame, no pool.** Drawing is sub-millisecond, while
//! recycling a pixmap would need destroy-notify bookkeeping to avoid
//! overwriting a frame still queued in the mixer.

use std::sync::Arc;

use cosmic_text::{
    fontdb, Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping, SwashCache,
    Weight, Wrap,
};
use gstreamer as gst;
use gstreamer_video as gst_video;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, PixmapMut, Rect, Transform,
};
use video_coach_core::highlight::HighlightShape;
use video_coach_core::layout::{
    bar_rect, scoreboard_rects, stroke_line_width, Rect as LayoutRect, BAR_FONT_RATIO,
    BAR_INSET_RATIO, SCOREBOARD_FONT_RATIO, SCOREBOARD_MIN_FONT_RATIO, SCOREBOARD_NAME_PAD_RATIO,
    SCOREBOARD_TAIL_FONT_RATIO,
};
use video_coach_core::project::Clip;
use video_coach_core::scoreboard::{format_clock, ScoreboardConfig, ScoreboardState};
use video_coach_core::stroke::Rgba;
use video_coach_core::stroke_replay::visible_strokes;

/// The two faces everything here is drawn in. Vendored so the picture doesn't
/// depend on what the machine happens to have installed.
const FONT_REGULAR: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../fonts/DejaVuSans-Bold.ttf");

/// The family both faces load under, and so the family every [`Attrs`] asks
/// for: [`Family::SansSerif`] resolves to it (see [`font_system`]).
const FONT_FAMILY: &str = "DejaVu Sans";

/// What a line too long for its rect ends in.
const ELLIPSIS: &str = "…";

/// The bar's background, over the picture: macOS's black at 60%.
const BAR_ALPHA: f32 = 0.6;

/// The dark edge under every opaque stroke, on **each side**, as a fraction of
/// the stroke's width -- so it scales with the pen as the pen scales with the
/// picture. A quarter reads as an outline: 1.35 px a side on a 1080p export,
/// under a pen of 5.4. Much more and it reads as a drop shadow.
///
/// The app's live layer draws the same edge (`StrokePath` in `app.slint`).
const STROKE_EDGE_RATIO: f64 = 0.25;
/// The edge's opacity, over black: dark enough to hold a white or yellow line
/// against a sunlit pitch, and short of opaque so it doesn't read as a black
/// pen stroke of its own.
const STROKE_EDGE_ALPHA: f32 = 0.8;

/// A highlight label's size, as a fraction of the picture's height — the
/// picture's and not the output's, for the reason the pen has: a pillarboxed
/// entry's ring and label keep the size they had beside the footage.
const LABEL_FONT_RATIO: f64 = 0.03;
/// The gap the label's pill keeps around its text, as a fraction of the font
/// size: once on each side of the line, and once more split above and below
/// it, since [`OverlayRenderer::draw_label`] centres the line down its rect.
const LABEL_PAD_RATIO: f32 = 0.35;
/// The gap between the pill and the box, as a fraction of the font size.
const LABEL_GAP_RATIO: f32 = 0.25;
/// How far a label may be shrunk to fit its pill before it is ellipsized
/// instead. A pill is only ever narrowed by the picture's own edge, so this
/// costs nothing until a name is drawn beside a very narrow picture.
const LABEL_MIN_FONT_RATIO: f32 = 0.5;

/// The score cell's fill, and the clock cell's — the two that aren't a team's
/// colour (spec S3). macOS's 0.1 and 0.05 grey, the clock's slightly darker
/// and slightly translucent.
const SCORE_FILL: [u8; 4] = [0x1a, 0x1a, 0x1a, 255];
const CLOCK_FILL: [u8; 4] = [0x0d, 0x0d, 0x0d, 242];

/// Line height as a multiple of the font size — cosmic-text has no default,
/// and this is the usual one.
const LINE_HEIGHT: f32 = 1.2;

/// How many times [`OverlayRenderer::fit`] may shrink a line before it gives
/// up and cuts it. Measured: every string tried converged in three, and the
/// bound is only here so a font that rounded the wrong way couldn't spin.
const FIT_PASSES: usize = 6;

/// One frame's overlay: what to draw, and the spaces to draw it in.
pub(crate) struct OverlayFrame<'a> {
    /// The drawings' clip, or `None` for an entry with no clip, which draws
    /// no strokes.
    pub clip: Option<&'a Clip>,
    /// Where in the recording the frame sits, which is the clock stroke replay
    /// runs on.
    pub record_time: f64,
    /// The letterboxed picture rect inside the output frame, `(x, y, w, h)`:
    /// the base pad's rect, which is the space the strokes were drawn in.
    pub picture: (i32, i32, i32, i32),
    /// The player highlights showing at this frame, already in **picture
    /// pixels** relative to the picture rect's origin
    /// (`video_coach_core::highlight::highlight_shapes`). The driver maps them
    /// through the frame's own zoom, so this layer stays zoom-agnostic exactly
    /// as it is for strokes (spec H4).
    pub highlights: &'a [HighlightShape],
    /// The bar's line. **Empty draws no bar at all** — neither its background
    /// nor its glyphs: that is how a caller suppresses the bar. Neither
    /// shipping caller does; both draw the entry's own line (spec E7).
    pub text: &'a str,
    /// The teams to draw and what the board reads at this frame, or `None`
    /// when the project has no scoreboard configured or nothing has been
    /// tagged yet (`core::scoreboard`'s two "draw nothing" cases). The state
    /// is the driver's per-frame
    /// [`ScoreboardContext::state_at`](video_coach_core::scoreboard::ScoreboardContext::state_at).
    pub scoreboard: Option<(&'a ScoreboardConfig, ScoreboardState)>,
}

/// Rasterizes overlay frames, holding the fonts between them.
///
/// One per pump thread: `FontSystem` and `SwashCache` are `&mut` to draw with,
/// and building a `FontSystem` per frame would re-parse both TTFs.
pub(crate) struct OverlayRenderer {
    fonts: FontSystem,
    cache: SwashCache,
    /// The last line fitted in each [`TextSlot`], so [`Self::fit`]'s shrinking
    /// and its search run once an entry rather than once a frame: none of
    /// those lines changes inside one.
    fitted: [Option<Fitted>; TextSlot::COUNT],
    /// The highlights' clip, with the picture rect and output size it was
    /// built for. Remembered for the same reason a line's fit is: the picture
    /// rect changes once an entry, while building the mask means zeroing and
    /// then filling a whole output frame's worth of bytes — measured at 0.45 ms
    /// of a 3.2 ms overlay at 1080p.
    mask: Option<(MaskKey, Mask)>,
}

/// What a remembered [`Mask`] was built for: the picture rect, and the output
/// size that is the mask's own size.
type MaskKey = ((i32, i32, i32, i32), u32, u32);

/// A line that holds still for a whole entry, and so gets a memo slot of its
/// own. The score, the clock and the stoppage tail change every frame and are
/// fitted afresh each time — a slot for them would only thrash.
///
/// The two team names share a size and very nearly a width, so a single slot
/// would thrash the same way: each frame would evict the other name's fit and
/// re-run the search.
#[derive(Debug, Clone, Copy)]
enum TextSlot {
    Bar,
    HomeName,
    AwayName,
}

impl TextSlot {
    const COUNT: usize = 3;
}

/// A remembered result of [`OverlayRenderer::fit`], with the four inputs it
/// came from: a hit needs all of them to match.
struct Fitted {
    line: String,
    font_size: f32,
    min_font_size: f32,
    max_width: f32,
    /// What to draw, and the size to draw it at.
    result: (String, f32),
}

/// How a run of text is shaped. **The weight is always stated**: both vendored
/// faces load under [`FONT_FAMILY`], so it is what picks between them.
#[derive(Debug, Clone, Copy)]
struct Style {
    metrics: Metrics,
    weight: Weight,
}

impl Style {
    /// A style at `font_size` pixels, with the usual line height.
    fn new(font_size: f32, weight: Weight) -> Style {
        Style {
            metrics: Metrics::new(font_size, font_size * LINE_HEIGHT),
            weight,
        }
    }

    fn attrs(&self) -> Attrs<'static> {
        Attrs::new().family(Family::SansSerif).weight(self.weight)
    }
}

/// Where a label sits across its rect. It is always centred down it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
}

/// One line of text to draw, and everything about how it lands.
///
/// **Every label is fitted to its rect**, because nothing here clips and a
/// centred line that overflows spills out of both ends of its cell. `BREAK` in
/// the clock cell used to reach into the away team's colour on one side and
/// past the bar on the other.
struct Label<'a> {
    text: &'a str,
    /// The rect the line is placed in, in output pixels.
    rect: LayoutRect,
    /// The size the line is drawn at when it fits, and the largest it is ever
    /// drawn at.
    style: Style,
    /// The smallest size the line may be shrunk to before it is ellipsized
    /// instead. Passing `style`'s own size forbids shrinking, which is what
    /// the text bar wants: it is one long sentence, and a sentence that shrank
    /// with its length would leave the bar's size dancing entry to entry.
    min_font_size: f32,
    color: TextColor,
    align: Align,
    /// The gap kept off `rect`'s left and right edges, in pixels.
    pad: f32,
    /// Where the fit is remembered, or `None` to re-run it every frame. Only
    /// a line that holds still for a whole entry is worth a slot (see
    /// [`TextSlot`]).
    slot: Option<TextSlot>,
}

/// The font database the overlay draws from: the two vendored faces and
/// nothing else.
///
/// **Built by hand rather than through `FontSystem::new_with_fonts`**, which
/// calls `fontdb::Database::load_system_fonts` — 432 faces on the reference
/// laptop, none on CI. That made the picture depend on the machine, and with a
/// bold face to pick it would have decided which one the labels got. The
/// sans-serif alias points at [`FONT_FAMILY`] so [`Family::SansSerif`] resolves
/// to the vendored family instead of falling back.
///
/// The locale is fixed for the same reason: it steers `cosmic-text`'s
/// script fallback, and there is nothing here to fall back to.
fn font_system() -> FontSystem {
    let mut db = fontdb::Database::new();
    for face in [FONT_REGULAR, FONT_BOLD] {
        db.load_font_source(fontdb::Source::Binary(Arc::new(face)));
    }
    db.set_sans_serif_family(FONT_FAMILY);
    FontSystem::new_with_locale_and_db("en-US".to_owned(), db)
}

impl OverlayRenderer {
    pub(crate) fn new() -> OverlayRenderer {
        OverlayRenderer {
            fonts: font_system(),
            cache: SwashCache::new(),
            fitted: [const { None }; TextSlot::COUNT],
            mask: None,
        }
    }

    /// `frame`'s overlay as an `out_w`×`out_h` premultiplied-RGBA buffer with
    /// a `VideoMeta`.
    ///
    /// The buffer carries no timestamp: the pump stamps it with the same PTS
    /// as the base frame it belongs to, or the mixer starves.
    pub(crate) fn render(&mut self, frame: &OverlayFrame, out_w: u32, out_h: u32) -> gst::Buffer {
        // tiny-skia assumes a tightly packed `w * 4` stride, which is what the
        // default allocator gives; the `VideoMeta` states it rather than
        // leaving `glupload` to infer it from the caps.
        let size = out_w as usize * out_h as usize * 4;
        let mut buffer = gst::Buffer::with_size(size).expect("one overlay frame fits in memory");
        let writable = buffer
            .get_mut()
            .expect("a freshly allocated buffer is writable");
        gst_video::VideoMeta::add(
            writable,
            gst_video::VideoFrameFlags::empty(),
            gst_video::VideoFormat::Rgba,
            out_w,
            out_h,
        )
        .expect("RGBA meta for a buffer allocated at exactly that size");
        let mut map = writable
            .map_writable()
            .expect("a freshly allocated buffer maps writable");
        let mut pixmap = PixmapMut::from_bytes(map.as_mut_slice(), out_w, out_h)
            .expect("the output frame is never empty");
        self.draw(&mut pixmap, frame);
        // The map holds the buffer borrowed until it goes, and it would
        // otherwise go at the end of the function -- after the return.
        drop(map);
        buffer
    }

    /// Draws `frame` over `pixmap`, in output pixels.
    ///
    /// **The order is macOS's:** the bar's background, then the strokes, then
    /// the glyphs, and the scoreboard over all of it. A drawing near the bottom
    /// of the picture stays visible over the bar's tint, the words stay legible
    /// over the drawing, and the board is never drawn through. The highlights
    /// go under all of it (spec H5): they mark the footage, and the coach's own
    /// pen is what they must never hide.
    fn draw(&mut self, pixmap: &mut PixmapMut, frame: &OverlayFrame) {
        // The allocator hands back whatever was in that memory, and nothing
        // else clears it: `from_bytes` adopts the bytes as they are.
        pixmap.fill(Color::TRANSPARENT);

        self.draw_highlights(pixmap, frame);

        let (out_w, out_h) = (f64::from(pixmap.width()), f64::from(pixmap.height()));
        let bar = bar_rect(out_w, out_h);
        if !frame.text.is_empty() {
            fill(
                pixmap,
                &bar,
                Color::from_rgba(0.0, 0.0, 0.0, BAR_ALPHA).expect("a valid colour"),
            );
        }

        draw_strokes(pixmap, frame);

        if !frame.text.is_empty() {
            self.draw_label(
                pixmap,
                &Label {
                    text: frame.text,
                    rect: bar,
                    style: Style::new((bar.h * BAR_FONT_RATIO) as f32, Weight::NORMAL),
                    // Its own size, so the bar never shrinks: it is a whole
                    // sentence, and one that resized with its length would
                    // leave the bar dancing from entry to entry.
                    min_font_size: (bar.h * BAR_FONT_RATIO) as f32,
                    color: TextColor::rgb(255, 255, 255),
                    align: Align::Left,
                    pad: (bar.h * BAR_INSET_RATIO) as f32,
                    slot: Some(TextSlot::Bar),
                },
            );
        }

        if let Some((config, state)) = frame.scoreboard {
            self.draw_scoreboard(pixmap, config, state, out_w, out_h);
        }
    }

    /// Draws the player highlights: a ring at each one's feet, and its label
    /// in a pill above it (spec H4).
    ///
    /// **The shapes arrive in picture pixels**, already mapped through the
    /// frame's zoom by `core::highlight::highlight_shapes`, so all that is
    /// left here is the picture's origin. The box itself is never drawn — it
    /// is the geometry the ring and the pill hang off.
    fn draw_highlights(&mut self, pixmap: &mut PixmapMut, frame: &OverlayFrame) {
        if frame.highlights.is_empty() {
            return;
        }
        let (x0, y0, ..) = frame.picture;
        let (x0, y0) = (f64::from(x0), f64::from(y0));
        // A ring at the edge of the footage is cut there rather than drawn
        // across the letterbox bars, which belong to the frame.
        let key = (frame.picture, pixmap.width(), pixmap.height());
        if self.mask.as_ref().is_none_or(|(k, _)| *k != key) {
            let Some(mask) = picture_mask(frame.picture, pixmap.width(), pixmap.height()) else {
                return;
            };
            self.mask = Some((key, mask));
        }
        let mask = &self.mask.as_ref().expect("built just above").1;
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        // Round, like the pen: nothing here has a corner anyway, and a ring
        // cut by the mask should end as softly as a stroke does.
        let mut pen = tiny_skia::Stroke {
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..tiny_skia::Stroke::default()
        };
        let edge = Color::from_rgba(0.0, 0.0, 0.0, STROKE_EDGE_ALPHA).expect("a valid colour");

        for shape in frame.highlights {
            let (cx, cy, rx, ry) = shape.ellipse;
            // `None` for a non-finite or empty ellipse — a corrupt project,
            // not something to paint a guess over (BACKLOG #28).
            let Some(oval) = Rect::from_xywh(
                (x0 + cx - rx) as f32,
                (y0 + cy - ry) as f32,
                (2.0 * rx) as f32,
                (2.0 * ry) as f32,
            ) else {
                continue;
            };
            let Some(path) = PathBuilder::from_oval(oval) else {
                continue;
            };
            let c = shape.color;
            let Some(color) = Color::from_rgba(c.r as f32, c.g as f32, c.b as f32, c.a as f32)
            else {
                continue;
            };
            // The dark edge first, a wider stroke the ring then covers, as
            // under a pen line — and for the same reason, only under an opaque
            // ring, which would otherwise read darker than its stored colour.
            if c.a >= 1.0 {
                paint.set_color(edge);
                pen.width = (shape.width * (1.0 + 2.0 * STROKE_EDGE_RATIO)) as f32;
                pixmap.stroke_path(&path, &paint, &pen, Transform::identity(), Some(mask));
            }
            paint.set_color(color);
            pen.width = shape.width as f32;
            pixmap.stroke_path(&path, &paint, &pen, Transform::identity(), Some(mask));
        }

        // Every pill over every ring, so one player's label is never cut in
        // half by the next player's ring.
        for shape in frame.highlights {
            self.draw_highlight_label(pixmap, frame, shape);
        }
    }

    /// Draws `shape`'s label in a pill of its own colour, above the box.
    ///
    /// **[`Self::draw_label`] bypasses the mask**, so the pill is *placed*
    /// inside the picture rect rather than clipped to it: below the box when
    /// there is no room above, and shifted in at the left and right edges. A
    /// clipped label would be worse than a moved one anyway — half a shirt
    /// number is a different shirt number.
    fn draw_highlight_label(
        &mut self,
        pixmap: &mut PixmapMut,
        frame: &OverlayFrame,
        shape: &HighlightShape,
    ) {
        if shape.label.is_empty() {
            return;
        }
        let (x0, y0, w, h) = frame.picture;
        let (x0, y0, w, h) = (f64::from(x0), f64::from(y0), f64::from(w), f64::from(h));
        let font_size = (LABEL_FONT_RATIO * h) as f32;
        if font_size <= 0.0 || w <= 0.0 {
            return;
        }
        let style = Style::new(font_size, Weight::BOLD);
        let pad = font_size * LABEL_PAD_RATIO;
        // The pill is measured around the line rather than the line fitted to
        // a pill, so it is exactly as wide as it needs to be. The picture's
        // own width is the only thing that ever narrows it, and `draw_label`
        // then fits the line to what is left.
        let pill_w = f64::from(self.width(&shape.label, style) + 2.0 * pad).min(w);
        let pill_h = f64::from(style.metrics.line_height + pad);
        let gap = f64::from(font_size * LABEL_GAP_RATIO);
        let above = shape.rect.y - gap - pill_h;
        let y = if above >= 0.0 {
            above
        } else {
            shape.rect.y + shape.rect.h + gap
        };
        let rect = LayoutRect {
            x: x0
                + (shape.rect.x + (shape.rect.w - pill_w) / 2.0).clamp(0.0, (w - pill_w).max(0.0)),
            y: y0 + y.clamp(0.0, (h - pill_h).max(0.0)),
            w: pill_w,
            h: pill_h,
        };
        fill(pixmap, &rect, fill_color(shape.color));
        self.draw_label(
            pixmap,
            &Label {
                text: &shape.label,
                rect,
                style,
                min_font_size: font_size * LABEL_MIN_FONT_RATIO,
                color: label_ink(shape.color),
                align: Align::Center,
                pad,
                // No slot: a ring moves every frame, and so does its pill.
                slot: None,
            },
        );
    }

    /// Draws the scoreboard top-left, over everything else (spec S3).
    ///
    /// The cells come from [`scoreboard_rects`]; the only geometry decided here
    /// is the accent strip, which that function returns as one row across the
    /// whole bar because the score cell sits between the two columns it
    /// actually covers.
    fn draw_scoreboard(
        &mut self,
        pixmap: &mut PixmapMut,
        config: &ScoreboardConfig,
        state: ScoreboardState,
        out_w: f64,
        out_h: f64,
    ) {
        let rects = scoreboard_rects(out_w, out_h);
        fill(pixmap, &rects.home, fill_color(config.home.primary_color));
        fill(pixmap, &rects.score, rgba8(SCORE_FILL));
        fill(pixmap, &rects.away, fill_color(config.away.primary_color));
        fill(pixmap, &rects.clock, rgba8(CLOCK_FILL));
        // The strip runs over the team columns only, in each team's own
        // secondary colour, so it is drawn as the two of them.
        for (cell, color) in [
            (&rects.home, config.home.secondary_color),
            (&rects.away, config.away.secondary_color),
        ] {
            let strip = LayoutRect {
                x: cell.x,
                w: cell.w,
                ..rects.accent
            };
            fill(pixmap, &strip, fill_color(color));
        }

        // Every label's size is a fraction of the CELL height, not the bar's
        // (the parent spec's table says the bar's and is ~9% too large).
        let cell_h = rects.home.h;
        let bold = Style::new((cell_h * SCOREBOARD_FONT_RATIO) as f32, Weight::BOLD);
        let white = TextColor::rgb(255, 255, 255);
        let pad = (cell_h * SCOREBOARD_NAME_PAD_RATIO) as f32;
        // Every cell here is narrow and everything in it is centred, so a
        // label that overflowed would spill out of both its ends. They are all
        // allowed to shrink down to the same floor instead; the columns are
        // sized so that nothing realistic has to (`core::layout`).
        let min_font_size = (cell_h * SCOREBOARD_MIN_FONT_RATIO) as f32;
        let clock = format_clock(state.clock);
        let score = format!("{} - {}", state.home_score, state.away_score);

        let labels = [
            Label {
                text: &config.home.name,
                rect: rects.home,
                style: bold,
                min_font_size,
                color: text_color(config.home.font_color),
                align: Align::Center,
                pad,
                slot: Some(TextSlot::HomeName),
            },
            Label {
                text: &score,
                rect: rects.score,
                style: bold,
                min_font_size,
                color: white,
                align: Align::Center,
                pad: 0.0,
                slot: None,
            },
            Label {
                text: &config.away.name,
                rect: rects.away,
                style: bold,
                min_font_size,
                color: text_color(config.away.font_color),
                align: Align::Center,
                pad,
                slot: Some(TextSlot::AwayName),
            },
            Label {
                text: &clock.main,
                rect: rects.clock,
                style: bold,
                min_font_size,
                color: white,
                align: Align::Center,
                pad: 0.0,
                slot: None,
            },
        ];
        for label in &labels {
            self.draw_label(pixmap, label);
        }
        // The `+M:SS` tail hangs outside the bar, and only in stoppage:
        // `trailing` is empty otherwise.
        if !clock.trailing.is_empty() {
            self.draw_label(
                pixmap,
                &Label {
                    text: &clock.trailing,
                    rect: rects.tail,
                    style: Style::new((cell_h * SCOREBOARD_TAIL_FONT_RATIO) as f32, Weight::NORMAL),
                    min_font_size,
                    color: white,
                    align: Align::Center,
                    pad: 0.0,
                    slot: None,
                },
            );
        }
    }

    /// Draws `label` on one row of its rect, fitted to it, centred down it and
    /// placed across it by its [`Align`].
    fn draw_label(&mut self, pixmap: &mut PixmapMut, label: &Label) {
        let max_width = label.rect.w as f32 - 2.0 * label.pad;
        if max_width <= 0.0 || label.style.metrics.font_size <= 0.0 || label.text.is_empty() {
            return;
        }
        let (line, font_size) = match label.slot {
            Some(slot) => self.remembered_fit(slot, label, max_width),
            None => self.fit(label.text, label.style, label.min_font_size, max_width),
        };
        // The fit may have shrunk the line, and it is drawn at the size it was
        // fitted at or it no longer fits.
        let style = Style::new(font_size, label.style.weight);

        let mut buffer = self.shaped(&line, style, Some(max_width));
        // The shaped width comes off this same buffer rather than a second
        // measuring pass: centring a label must not cost an extra shaping a
        // frame.
        let line_w = line_width(&buffer);
        let left = match label.align {
            Align::Left => label.rect.x as f32 + label.pad,
            Align::Center => label.rect.x as f32 + (label.rect.w as f32 - line_w) / 2.0,
        }
        .round() as i32;
        // Vertically centred on the rect: the glyphs' own box is one line
        // high, so centring it centres the ascender and descender together.
        let line_height = style.metrics.line_height;
        let top = (label.rect.y as f32 + (label.rect.h as f32 - line_height) / 2.0).round() as i32;

        let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
        let data = pixmap.data_mut();
        let (fonts, cache) = (&mut self.fonts, &mut self.cache);
        buffer.draw(fonts, cache, label.color, |x, y, w, h, color| {
            let a = u32::from(color.a());
            if a == 0 {
                return;
            }
            // `color` is straight alpha; the pixmap is premultiplied.
            let src = [color.r(), color.g(), color.b(), color.a()].map(|c| u32::from(c) * a / 255);
            for dy in 0..h as i32 {
                for dx in 0..w as i32 {
                    let (px, py) = (left + x + dx, top + y + dy);
                    if px < 0 || py < 0 || px >= width || py >= height {
                        continue;
                    }
                    let i = (py as usize * width as usize + px as usize) * 4;
                    for c in 0..4 {
                        let under = u32::from(data[i + c]) * (255 - a) / 255;
                        data[i + c] = (src[c] + under).min(255) as u8;
                    }
                }
            }
        });
    }

    /// [`Self::fit`] for `label`, remembered in `slot`.
    ///
    /// A slot keeps one fit, and its four inputs with it: a line, a size, a
    /// floor and a width. A slot always holds the same weight, so the memo
    /// need not key on that.
    fn remembered_fit(&mut self, slot: TextSlot, label: &Label, max_width: f32) -> (String, f32) {
        let font_size = label.style.metrics.font_size;
        if let Some(f) = &self.fitted[slot as usize] {
            if f.line == label.text
                && f.font_size == font_size
                && f.min_font_size == label.min_font_size
                && f.max_width == max_width
            {
                return f.result.clone();
            }
        }
        let result = self.fit(label.text, label.style, label.min_font_size, max_width);
        self.fitted[slot as usize] = Some(Fitted {
            line: label.text.to_owned(),
            font_size,
            min_font_size: label.min_font_size,
            max_width,
            result: result.clone(),
        });
        result
    }

    /// `line` made to fit `max_width`, as the line to draw and the size to
    /// draw it at.
    ///
    /// **It is shrunk first, down to `min_font_size`, and only cut once it is
    /// there.** A smaller whole name says more than a full-size stub:
    /// `Manchester United` in the home cell ellipsizes to `Manche…` but fits
    /// whole at 0.38 of the full size, which is 17 px at 1080p. Spec S3
    /// originally refused shrink-to-fit on the grounds that a shrunk long name
    /// is illegible anyway; measured, it isn't.
    ///
    /// Something has to fit, because nothing here clips: [`Wrap::None`] puts
    /// the whole line on one row whatever its width, so a realistic clip line
    /// otherwise runs off the right of the frame (measured: 143 characters
    /// overflows at every resolution) and a centred clock label spills out of
    /// both ends of its cell. macOS wrapped the bar onto a second row, which
    /// landed over the picture.
    ///
    /// The text bar passes its own size as the floor, which forbids shrinking
    /// and leaves it ellipsizing exactly as it did.
    fn fit(
        &mut self,
        line: &str,
        style: Style,
        min_font_size: f32,
        max_width: f32,
    ) -> (String, f32) {
        // A floor above the size asked for would make the loop below climb
        // instead of descend. No caller does it; the clamp is what makes that
        // true of every future one too.
        let min_font_size = min_font_size.min(style.metrics.font_size);
        let mut style = style;
        // A shaped width is very nearly proportional to the size, so scaling
        // by the overflow lands within a fraction of a pixel and the next pass
        // confirms it — measured: two passes for every string tried. It is
        // measured rather than computed because hinting rounds each advance,
        // and bounded because a font that rounded the wrong way could
        // otherwise creep down a pixel at a time.
        for _ in 0..FIT_PASSES {
            let width = self.width(line, style);
            if width <= max_width {
                return (line.to_owned(), style.metrics.font_size);
            }
            let size = (style.metrics.font_size * max_width / width).max(min_font_size);
            if size >= style.metrics.font_size {
                break;
            }
            style = Style::new(size, style.weight);
        }

        // At the floor and still too wide: the longest prefix that fits with
        // an ellipsis after it. Shaping is the cost here, so this bisects the
        // character boundaries rather than shaping once per character.
        let cuts: Vec<usize> = line
            .char_indices()
            .map(|(i, _)| i)
            .chain([line.len()])
            .collect();
        let with_ellipsis = |cut: usize| format!("{}{ELLIPSIS}", &line[..cut]);
        let (mut low, mut high) = (0, cuts.len() - 1);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if self.width(&with_ellipsis(cuts[mid]), style) <= max_width {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        // `low` is 0 when even one character and an ellipsis are too wide, and
        // a bare ellipsis is then the honest answer.
        (with_ellipsis(cuts[low]), style.metrics.font_size)
    }

    /// How wide `line` is, shaped on one unbounded row.
    fn width(&mut self, line: &str, style: Style) -> f32 {
        line_width(&self.shaped(line, style, None))
    }

    /// `line` shaped on one row, at most `max_width` wide if given.
    fn shaped(&mut self, line: &str, style: Style, max_width: Option<f32>) -> Buffer {
        let mut buffer = Buffer::new(&mut self.fonts, style.metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(max_width, Some(style.metrics.line_height));
        buffer.set_text(line, &style.attrs(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }
}

/// How wide a shaped [`Buffer`]'s widest row is.
fn line_width(buffer: &Buffer) -> f32 {
    buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0, f32::max)
}

/// Fills `rect` with `color`, doing nothing for a rect that is empty or
/// non-finite (a corrupt layout, not something to paint a guess over).
///
/// Anti-aliasing off: these are axis-aligned blocks of chrome, and a soft
/// edge on one would only leak the picture through it.
fn fill(pixmap: &mut PixmapMut, rect: &LayoutRect, color: Color) {
    let Some(rect) = Rect::from_xywh(rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32)
    else {
        return;
    };
    let paint = Paint {
        shader: tiny_skia::Shader::SolidColor(color),
        anti_alias: false,
        ..Paint::default()
    };
    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
}

/// An 8-bit straight-alpha colour as tiny-skia's.
fn rgba8([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_rgba8(r, g, b, a)
}

/// A stored colour as tiny-skia's. Out of range is transparent: a corrupt
/// project, not something to paint a guess over (BACKLOG #28).
fn fill_color(c: Rgba) -> Color {
    Color::from_rgba(c.r as f32, c.g as f32, c.b as f32, c.a as f32).unwrap_or(Color::TRANSPARENT)
}

/// Black or white over `c`, whichever can be read on it.
///
/// A label's pill takes the highlight's own colour, and the swatch row runs
/// from white through yellow to black, so a fixed ink would be invisible at
/// one end of it. The weights are the usual relative-luminance ones.
fn label_ink(c: Rgba) -> TextColor {
    let luma = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    if luma > 0.5 {
        TextColor::rgb(0, 0, 0)
    } else {
        TextColor::rgb(255, 255, 255)
    }
}

/// A mask covering the picture rect on an `out_w`×`out_h` frame, which is what
/// the highlights are clipped to. `None` for an empty or non-finite rect,
/// which draws nothing.
fn picture_mask(picture: (i32, i32, i32, i32), out_w: u32, out_h: u32) -> Option<Mask> {
    let (x, y, w, h) = picture;
    let rect = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32)?;
    let mut mask = Mask::new(out_w, out_h)?;
    // Anti-aliasing off: the picture rect is an axis-aligned block, and a soft
    // edge on the clip would only leak a ring into the letterbox bars.
    mask.fill_path(
        &PathBuilder::from_rect(rect),
        FillRule::Winding,
        false,
        Transform::identity(),
    );
    Some(mask)
}

/// A stored colour as cosmic-text's, which is 8-bit and straight-alpha.
fn text_color(c: Rgba) -> TextColor {
    let channel = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    TextColor::rgba(channel(c.r), channel(c.g), channel(c.b), channel(c.a))
}

/// Draws the visible strokes over `pixmap`, mapped into the picture rect, each
/// on its thin dark edge ([`STROKE_EDGE_RATIO`]).
///
/// The pen denormalizes against the picture's **height**, not the output's:
/// a stroke keeps the weight it was drawn with when the picture is
/// pillarboxed (`core::layout::stroke_line_width`).
fn draw_strokes(pixmap: &mut PixmapMut, frame: &OverlayFrame) {
    let Some(clip) = frame.clip else {
        return;
    };
    let (x0, y0, w, h) = frame.picture;
    let (x0, y0, w, h) = (f64::from(x0), f64::from(y0), f64::from(w), f64::from(h));
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    // Round both, always: the coach draws with a pen, and a mitre join spikes
    // on the sharp reversals a freehand stroke is full of.
    let mut pen = tiny_skia::Stroke {
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..tiny_skia::Stroke::default()
    };
    let edge = Color::from_rgba(0.0, 0.0, 0.0, STROKE_EDGE_ALPHA).expect("a valid colour");

    for visible in visible_strokes(clip, frame.record_time) {
        let stroke = visible.stroke;
        let Some((first, rest)) = stroke.points[..visible.drawn_point_count].split_first() else {
            continue;
        };
        let point = |p: &video_coach_core::stroke::StrokePoint| {
            ((x0 + p.x * w) as f32, (y0 + p.y * h) as f32)
        };

        let mut path = PathBuilder::new();
        let (x, y) = point(first);
        path.move_to(x, y);
        for p in rest {
            let (x, y) = point(p);
            path.line_to(x, y);
        }
        if rest.is_empty() {
            // A click is one point. The degenerate segment `M x y L x y` draws
            // as a dot under a round cap, where a bare move-to draws nothing.
            path.line_to(x, y);
        }
        // `None` when a coordinate is non-finite — a corrupt project, not
        // something to paint a guess over (BACKLOG #28).
        let Some(path) = path.finish() else { continue };

        let c = stroke.color;
        let Some(color) = Color::from_rgba(c.r as f32, c.g as f32, c.b as f32, c.a as f32) else {
            continue;
        };
        let width = stroke_line_width(stroke.line_width, h);
        // The edge first, a wider stroke the line then covers, so only its
        // rim shows. Each stroke's pair in turn: a later line crossing an
        // earlier one is outlined over it, as a pen on a pen would be.
        //
        // **Opaque strokes only.** The edge runs under the whole line, so a
        // see-through one would show it through its middle and read darker
        // than its stored colour. Every pen the app offers is opaque.
        if c.a >= 1.0 {
            paint.set_color(edge);
            pen.width = (width * (1.0 + 2.0 * STROKE_EDGE_RATIO)) as f32;
            pixmap.stroke_path(&path, &paint, &pen, Transform::identity(), None);
        }
        paint.set_color(color);
        pen.width = width as f32;
        pixmap.stroke_path(&path, &paint, &pen, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;
    use video_coach_core::event::{CommentaryEvent, EventKind};
    use video_coach_core::highlight::{highlight_shapes, HighlightKey, NormRect, PlayerHighlight};
    use video_coach_core::layout::{BAR_HEIGHT_RATIO, PIP_WIDTH_RATIO};
    use video_coach_core::scoreboard::{ClockDisplay, MatchFormat, TeamConfig};
    use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
    use video_coach_core::zoom::Zoom;

    use super::*;

    /// The export's own output size, which the scoreboard tests use so the
    /// rects they reason about are the shipping ones.
    const OUT_W: u32 = 1920;
    const OUT_H: u32 = 1080;

    /// A clip whose only content is `events`.
    fn clip(events: Vec<CommentaryEvent>) -> Clip {
        Clip {
            id: Uuid::nil(),
            name: "c".into(),
            notes: String::new(),
            tags: Vec::new(),
            source_index: 0,
            start_source_seconds: 0.0,
            recording_duration: 60.0,
            recording_filename: "c.mkv".into(),
            events,
            show_pip: true,
            sort_index: 0,
            created_at: "2026-09-19T00:00:00Z".into(),
            transcript: String::new(),
        }
    }

    /// A horizontal stroke across the middle of the picture, from `x = 0.2` to
    /// `x = 0.8`, logged (as the recorder does) at pen-up.
    fn bar(finished_at: f64, line_width: f64, auto_clear: Option<f64>) -> CommentaryEvent {
        let points = [0.2, 0.5, 0.8]
            .into_iter()
            .enumerate()
            .map(|(i, x)| StrokePoint {
                x,
                y: 0.5,
                t: i as f64 * 0.1,
            })
            .collect();
        CommentaryEvent::new(
            finished_at,
            EventKind::Stroke(Stroke {
                id: Uuid::nil(),
                color: Rgba::RED,
                line_width,
                points,
                auto_clear_after_seconds: auto_clear,
            }),
        )
    }

    /// The rendered pixels of `frame`, as `[r, g, b, a]` per pixel in row
    /// order, on a `w`×`h` output.
    fn render_frame(frame: &OverlayFrame, w: u32, h: u32) -> Vec<[u8; 4]> {
        gst::init().unwrap();
        let buffer = OverlayRenderer::new().render(frame, w, h);
        let meta = buffer.meta::<gst_video::VideoMeta>().expect("a VideoMeta");
        assert_eq!(meta.format(), gst_video::VideoFormat::Rgba);
        assert_eq!((meta.width(), meta.height()), (w, h));
        assert_eq!(meta.stride(), [w as i32 * 4]);

        let map = buffer.map_readable().unwrap();
        assert_eq!(map.len(), (w * h * 4) as usize);
        map.as_chunks::<4>().0.to_vec()
    }

    /// An overlay over `clip` with no scoreboard and no highlights, which is
    /// Phase 8's.
    fn render_at(
        clip: &Clip,
        record_time: f64,
        text: &str,
        picture: (i32, i32, i32, i32),
        w: u32,
        h: u32,
    ) -> Vec<[u8; 4]> {
        render_frame(
            &OverlayFrame {
                clip: Some(clip),
                record_time,
                picture,
                highlights: &[],
                text,
                scoreboard: None,
            },
            w,
            h,
        )
    }

    /// [`render_at`] over a picture filling the whole output and no bar.
    fn render(clip: &Clip, record_time: f64, w: u32, h: u32) -> Vec<[u8; 4]> {
        render_at(clip, record_time, "", (0, 0, w as i32, h as i32), w, h)
    }

    fn at(px: &[[u8; 4]], w: u32, x: u32, y: u32) -> [u8; 4] {
        px[(y * w + x) as usize]
    }

    #[test]
    fn a_stroke_covers_the_point_it_was_drawn_through_and_nothing_far_from_it() {
        let px = render(&clip(vec![bar(1.0, 0.05, None)]), 1.0, 200, 100);
        // Dead centre of the bar: fully covered, and premultiplied RED
        // (1.0, 0.2, 0.2) at full alpha.
        assert_eq!(at(&px, 200, 100, 50), [255, 51, 51, 255]);
        // A corner is untouched -- including its colour channels, which a
        // buffer the allocator handed back dirty would carry.
        assert_eq!(at(&px, 200, 5, 5), [0, 0, 0, 0]);
        // So is a point on the same row, past where the stroke ended.
        assert_eq!(at(&px, 200, 195, 50), [0, 0, 0, 0]);
    }

    /// Every stroke sits on a thin dark edge, so a bright pen stays crisp over
    /// grass, kits and compression noise: just past the coloured line the
    /// picture is darkened, and further out it is left alone.
    #[test]
    fn a_stroke_has_a_thin_dark_edge() {
        // A 10 px pen across the middle row of a 200×200 picture.
        let px = render(&clip(vec![bar(1.0, 0.05, None)]), 1.0, 200, 200);
        let line = 10.0;
        let edge = line * (1.0 + 2.0 * STROKE_EDGE_RATIO);
        // The row halfway between the coloured line's side and the edge's.
        let just_outside = (100.0 + (line + edge) / 4.0) as u32;
        let [r, g, b, a] = at(&px, 200, 100, just_outside);
        assert!(
            a >= 150 && r <= 20 && g <= 20 && b <= 20,
            "row {just_outside}: {:?}",
            [r, g, b, a]
        );
        // The line itself is still its own colour, fully covering the edge.
        assert_eq!(at(&px, 200, 100, 100), [255, 51, 51, 255]);
        // Well clear of both, nothing: an outline, not a shadow.
        assert_eq!(at(&px, 200, 100, 100 + (edge as u32)), [0, 0, 0, 0]);

        // A see-through stroke has none: it would show through the middle.
        let mut ev = bar(1.0, 0.05, None);
        let EventKind::Stroke(s) = &mut ev.kind else {
            unreachable!()
        };
        s.color.a = 0.5;
        let px = render(&clip(vec![ev]), 1.0, 200, 200);
        assert_eq!(at(&px, 200, 100, just_outside), [0, 0, 0, 0]);
    }

    #[test]
    fn every_pixel_is_premultiplied() {
        // A translucent stroke: half-covered edge pixels are where straight
        // alpha would show up as `r > a`.
        let mut ev = bar(1.0, 0.05, None);
        let EventKind::Stroke(s) = &mut ev.kind else {
            unreachable!()
        };
        s.color = Rgba {
            a: 0.5,
            ..Rgba::RED
        };
        for [r, g, b, a] in render(&clip(vec![ev]), 1.0, 200, 100) {
            assert!(r <= a && g <= a && b <= a, "{r},{g},{b} over alpha {a}");
        }
    }

    #[test]
    fn a_partly_drawn_stroke_stops_where_the_pen_had_reached() {
        // Pen-up at t = 1.0 after 0.2 s, so the stroke began at 0.8 and at
        // t = 0.95 the pen has reached its middle point, x = 0.5. Everything
        // up to there is painted; the rest of the bar is not.
        let px = render(&clip(vec![bar(1.0, 0.05, None)]), 0.95, 200, 100);
        assert_eq!(at(&px, 200, 60, 50), [255, 51, 51, 255]);
        assert_eq!(at(&px, 200, 140, 50), [0, 0, 0, 0]);
    }

    #[test]
    fn a_cleared_or_expired_stroke_draws_nothing() {
        let cleared = clip(vec![
            bar(1.0, 0.05, None),
            CommentaryEvent::new(2.0, EventKind::ClearAll),
        ]);
        assert!(render(&cleared, 3.0, 200, 100).iter().all(|p| p[3] == 0));

        // Auto-clear counts from pen-up: gone at t = 1.0 + 1.5.
        let expired = clip(vec![bar(1.0, 0.05, Some(1.5))]);
        assert!(render(&expired, 2.6, 200, 100).iter().all(|p| p[3] == 0));
        // ... and still there just before.
        assert!(render(&expired, 2.4, 200, 100).iter().any(|p| p[3] > 0));
    }

    #[test]
    fn a_single_point_stroke_draws_a_dot() {
        let mut ev = bar(1.0, 0.05, None);
        let EventKind::Stroke(s) = &mut ev.kind else {
            unreachable!()
        };
        s.points.truncate(1);
        let px = render(&clip(vec![ev]), 1.0, 200, 100);
        // The point is (0.2, 0.5), and the round cap makes it a disc.
        assert_eq!(at(&px, 200, 40, 50), [255, 51, 51, 255]);
        assert_eq!(at(&px, 200, 100, 50), [0, 0, 0, 0]);
    }

    /// The pen is normalized to the picture's HEIGHT, so it thickens with the
    /// rect's height and ignores its width.
    #[test]
    fn the_line_width_scales_with_the_picture_height_only() {
        let clip = clip(vec![bar(1.0, 0.05, None)]);
        // The bar's thickness in pixels, down the centre column it crosses:
        // summed coverage rather than a count of opaque rows, so the two
        // anti-aliased edge pixels are measured instead of being a threshold
        // to pick (tiny-skia's anti-aliasing is not a stable contract). The
        // red channel, not alpha: the dark edge has alpha but no red, so this
        // is the coloured line alone.
        let thickness = |w: u32, h: u32| {
            let px = render(&clip, 1.0, w, h);
            let red: u32 = (0..h).map(|y| u32::from(at(&px, w, w / 2, y)[0])).sum();
            f64::from(red) / 255.0
        };
        // 0.05 x 100.
        assert!((thickness(200, 100) - 5.0).abs() < 0.5);
        // Twice the height, twice the pen.
        assert!((thickness(200, 200) - 10.0).abs() < 0.5);
        // Twice the width, same pen.
        assert_eq!(thickness(400, 100), thickness(200, 100));
    }

    /// A pillarboxed entry: the overlay is the output frame, but the stroke
    /// lands in the picture rect and takes its pen from the picture's height.
    /// Rasterizing at the output size without the mapping would put the middle
    /// of the stroke at x = 640 rather than x = 800.
    #[test]
    fn a_stroke_is_mapped_into_the_picture_rect() {
        // 4:3 in 1280x720: the picture is (160, 0, 960, 720).
        let picture = (160, 0, 960, 720);
        let clip = clip(vec![bar(1.0, 0.05, None)]);
        // After pen-up, so the whole stroke is drawn.
        let px = render_at(&clip, 1.05, "", picture, 1280, 720);
        // The stroke spans x = 0.2..0.8 of the picture: 352 to 928.
        assert_eq!(at(&px, 1280, 640, 360), [255, 51, 51, 255]);
        assert_eq!(at(&px, 1280, 360, 360), [255, 51, 51, 255]);
        assert_eq!(at(&px, 1280, 920, 360), [255, 51, 51, 255]);
        // Nothing in the pillarbox bars, nor past the stroke's ends.
        assert_eq!(at(&px, 1280, 80, 360), [0, 0, 0, 0]);
        assert_eq!(at(&px, 1280, 1200, 360), [0, 0, 0, 0]);
        // (The round cap and its edge reach 18 + 9 px before x = 352.)
        assert_eq!(at(&px, 1280, 320, 360), [0, 0, 0, 0]);
        // The pen is 0.05 x 720 = 36 px, from the picture's height and not the
        // output's (identical here) nor its width. Red, as in the test above.
        let red: u32 = (0..720).map(|y| u32::from(at(&px, 1280, 640, y)[0])).sum();
        assert!((f64::from(red) / 255.0 - 36.0).abs() < 1.0);
    }

    /// The bar's background covers the bottom strip of the **output**, at the
    /// layout's ratio and alpha, and nothing above it.
    #[test]
    fn the_bar_covers_the_bottom_strip_of_the_output() {
        let px = render_at(
            &clip(Vec::new()),
            0.0,
            "1 / 3",
            (0, 0, 1280, 720),
            1280,
            720,
        );
        let bar_top = (720.0 - BAR_HEIGHT_RATIO * 720.0) as u32;
        // Premultiplied black at 60%: (0, 0, 0, 153).
        assert_eq!(at(&px, 1280, 20, bar_top + 4), [0, 0, 0, 153]);
        assert_eq!(at(&px, 1280, 1260, 719), [0, 0, 0, 153]);
        // One row above the bar is untouched.
        assert_eq!(at(&px, 1280, 20, bar_top - 2), [0, 0, 0, 0]);
    }

    /// An empty line draws nothing at all — not even the bar's background:
    /// the one way a caller can suppress the bar.
    #[test]
    fn an_empty_line_draws_no_bar() {
        let px = render_at(&clip(Vec::new()), 0.0, "", (0, 0, 1280, 720), 1280, 720);
        assert!(px.iter().all(|p| p[3] == 0));
    }

    /// The glyphs land inside the bar, left of the PiP, and never above it.
    #[test]
    fn the_glyphs_land_inside_the_bar() {
        let px = render_at(
            &clip(Vec::new()),
            0.0,
            "1 / 3 | Second-half restart | press",
            (0, 0, 1280, 720),
            1280,
            720,
        );
        let bar_top = (720.0 - BAR_HEIGHT_RATIO * 720.0) as u32;
        // White glyphs are the only thing here brighter than the tint.
        let lit = |rows: std::ops::Range<u32>, cols: std::ops::Range<u32>| {
            rows.flat_map(|y| cols.clone().map(move |x| (x, y)))
                .filter(|&(x, y)| at(&px, 1280, x, y)[0] > 128)
                .count()
        };
        assert!(lit(bar_top..720, 0..640) > 100, "no glyphs in the bar");
        assert_eq!(lit(0..bar_top, 0..1280), 0, "glyphs above the bar");
        // And they start after the inset rather than at the very edge.
        assert_eq!(lit(bar_top..720, 0..4), 0, "glyphs in the inset");
        // The PiP's column is clear at this length, so the inset never has to
        // fight the webcam for the right-hand end of the bar.
        let pip_left = (1280.0 * (1.0 - PIP_WIDTH_RATIO)) as u32;
        assert_eq!(lit(bar_top..720, pip_left..1280), 0);
    }

    /// A line too long for the bar is cut with an ellipsis rather than
    /// wrapped onto a second row or run off the frame.
    #[test]
    fn a_long_line_is_cut_with_an_ellipsis() {
        let long = "12 / 24 | Second-half restart down the left channel, \
                    the one we talked about on Tuesday | press, transition, wide, \
                    set-piece";
        let mut renderer = OverlayRenderer::new();
        let bar = bar_rect(1920.0, 1080.0);
        let style = Style::new((bar.h * BAR_FONT_RATIO) as f32, Weight::NORMAL);
        let max_width = bar.w as f32 - 2.0 * (bar.h * BAR_INSET_RATIO) as f32;

        let size = style.metrics.font_size;
        // The bar forbids shrinking by passing its own size as the floor.
        let (fitted, fitted_size) = renderer.fit(long, style, size, max_width);
        assert_eq!(fitted_size, size, "the bar shrank");
        assert!(fitted.ends_with(ELLIPSIS), "{fitted:?} has no ellipsis");
        assert!(long.starts_with(fitted.trim_end_matches(ELLIPSIS)));
        assert!(renderer.width(&fitted, style) <= max_width);
        // And it is the longest such cut: one more character overflows.
        let kept = fitted.trim_end_matches(ELLIPSIS).chars().count();
        let longer = format!(
            "{}{ELLIPSIS}",
            long.chars().take(kept + 1).collect::<String>()
        );
        assert!(renderer.width(&longer, style) > max_width);

        // A line that fits is left exactly as it is.
        let short = "3 / 7 | Turnover";
        assert_eq!(renderer.fit(short, style, size, max_width).0, short);
    }

    /// The cut line is drawn on one row: the overflow never reaches the
    /// picture above the bar, which is where macOS's wrap put it.
    #[test]
    fn a_long_line_stays_on_one_row() {
        let long = "9 / 30 | ".to_owned() + &"a long clip name ".repeat(20);
        let px = render_at(&clip(Vec::new()), 0.0, &long, (0, 0, 1280, 720), 1280, 720);
        let bar_top = (720.0 - BAR_HEIGHT_RATIO * 720.0) as u32;
        let above = (0..bar_top)
            .flat_map(|y| (0..1280).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&px, 1280, x, y)[3] > 0)
            .count();
        assert_eq!(above, 0, "{above} pixels of text above the bar");
        // The last column of the bar is inset, so a line that overflowed the
        // frame would have painted into it.
        let right_edge = (bar_top..720)
            .filter(|&y| at(&px, 1280, 1279, y)[0] > 128)
            .count();
        assert_eq!(right_edge, 0);
    }

    // -------------------------------------------------- the player highlights

    fn norm(x: f64, y: f64, w: f64, h: f64) -> NormRect {
        NormRect { x, y, w, h }
    }

    /// The shapes core makes for one highlight box, on a `w`×`h` picture with
    /// no zoom: the drivers' own call, so the ring geometry under test is the
    /// shipping one rather than a copy of it.
    fn shapes(color: Rgba, label: &str, rect: NormRect, w: f64, h: f64) -> Vec<HighlightShape> {
        let highlight = PlayerHighlight {
            id: Uuid::nil(),
            source_index: 0,
            color,
            label: label.to_owned(),
            keys: vec![HighlightKey {
                source_seconds: 0.0,
                rect,
                tracked: false,
            }],
        };
        highlight_shapes(&[highlight], 0, 0.0, Zoom::IDENTITY, w, h)
    }

    /// `highlights` alone: no drawings, no bar and no board.
    fn render_rings(
        highlights: &[HighlightShape],
        picture: (i32, i32, i32, i32),
        w: u32,
        h: u32,
    ) -> Vec<[u8; 4]> {
        render_frame(
            &OverlayFrame {
                clip: Some(&clip(Vec::new())),
                record_time: 0.0,
                picture,
                highlights,
                text: "",
                scoreboard: None,
            },
            w,
            h,
        )
    }

    /// Whether any pixel within `r` of `(x, y)` satisfies `matches`: an
    /// anti-aliased curve's ink lands within a pixel or two of its geometry.
    fn near(
        px: &[[u8; 4]],
        w: u32,
        x: u32,
        y: u32,
        r: u32,
        matches: impl Fn([u8; 4]) -> bool,
    ) -> bool {
        (y.saturating_sub(r)..=y + r)
            .flat_map(|y| (x.saturating_sub(r)..=x + r).map(move |x| (x, y)))
            .any(|(x, y)| matches(at(px, w, x, y)))
    }

    /// The ring's own colour, premultiplied and opaque.
    fn blue(p: [u8; 4]) -> bool {
        p == [0, 0, 255, 255]
    }

    /// A label's ink over a dark pill: the only thing in these frames that is
    /// white in every channel.
    fn white(p: [u8; 4]) -> bool {
        p[0] > 200 && p[1] > 200 && p[2] > 200
    }

    /// The rows carrying `matches` ink, in order.
    fn ink_rows(px: &[[u8; 4]], w: u32, h: u32, matches: impl Fn([u8; 4]) -> bool) -> Vec<u32> {
        (0..h)
            .filter(|&y| (0..w).any(|x| matches(at(px, w, x, y))))
            .collect()
    }

    /// The first few painted pixels outside `picture`, which is where nothing
    /// belongs: the ring is masked to the footage, and the pill is placed
    /// inside it.
    fn painted_outside(
        px: &[[u8; 4]],
        picture: (i32, i32, i32, i32),
        w: u32,
        h: u32,
    ) -> Vec<(u32, u32)> {
        let (x0, y0, pw, ph) = picture;
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| at(px, w, x, y)[3] > 0)
            .filter(|&(x, y)| {
                (x as i32) < x0 || (x as i32) >= x0 + pw || (y as i32) < y0 || (y as i32) >= y0 + ph
            })
            .take(8)
            .collect()
    }

    /// The ring is an ellipse at the box's feet, in the highlight's colour and
    /// on the strokes' dark edge. The box itself is geometry, not ink.
    #[test]
    fn a_highlight_draws_a_ring_at_the_boxs_feet() {
        let w = 800;
        let shapes = shapes(
            rgba(0.0, 0.0, 1.0),
            "",
            norm(0.4, 0.3, 0.2, 0.4),
            800.0,
            800.0,
        );
        let px = render_rings(&shapes, (0, 0, 800, 800), w, 800);
        // The box is (320, 240, 160, 320), so the ring is centred on
        // (400, 560) with rx = 1.4 x 160 / 2 = 112 and ry = 0.35 x 112 = 39.2.
        assert!(near(&px, w, 400, 599, 3, blue), "no ring below the feet");
        assert!(near(&px, w, 400, 521, 3, blue), "no ring above the feet");
        assert!(near(&px, w, 288, 560, 3, blue), "no ring to the left");
        assert!(near(&px, w, 512, 560, 3, blue), "no ring to the right");
        // The dark edge, as under a stroke: just past the ring's bottom the
        // picture is darkened, and the blue channel says it is not the ring.
        assert!(
            (560..620).any(|y| {
                let [r, g, b, a] = at(&px, w, 400, y);
                a > 150 && r < 60 && g < 60 && b < 60
            }),
            "the ring has no dark edge"
        );
        // The box is not drawn, and nothing is painted well clear of the ring.
        assert_eq!(at(&px, w, 400, 300), [0, 0, 0, 0], "the box was drawn");
        assert_eq!(at(&px, w, 400, 470), [0, 0, 0, 0]);
        assert_eq!(at(&px, w, 400, 660), [0, 0, 0, 0]);
    }

    /// A ring reaching past the picture is cut at its edge: the letterbox bars
    /// are the frame's, and a highlight belongs to the footage (spec H4).
    #[test]
    fn a_ring_is_clipped_to_the_picture_rect() {
        let picture = (160, 40, 480, 640);
        let (pw, ph) = (480.0, 640.0);
        let color = rgba(0.0, 0.0, 1.0);
        // A box at the left edge, one whose feet sit on the bottom edge, one
        // at the right edge, and one whose ring is entirely above the picture.
        let mut all = shapes(color, "", norm(0.0, 0.4, 0.08, 0.2), pw, ph);
        for rect in [
            norm(0.4, 0.85, 0.2, 0.15),
            norm(0.92, 0.4, 0.08, 0.2),
            norm(0.4, -0.2, 0.2, 0.15),
        ] {
            all.extend(shapes(color, "", rect, pw, ph));
        }
        let (w, h) = (800, 720);
        let px = render_rings(&all, picture, w, h);

        let outside = painted_outside(&px, picture, w, h);
        assert!(
            outside.is_empty(),
            "painted outside the picture: {outside:?}"
        );
        assert!(px.iter().copied().any(blue), "no ring was drawn at all");
    }

    /// Highlights go under everything, so the coach's pen stays on top of a
    /// ring it crosses (spec H5).
    #[test]
    fn a_stroke_crossing_a_ring_shows_the_strokes_colour() {
        let w = 800;
        let shapes = shapes(
            rgba(0.0, 0.0, 1.0),
            "",
            norm(0.4, 0.25, 0.2, 0.25),
            800.0,
            800.0,
        );
        let px = render_frame(
            &OverlayFrame {
                clip: Some(&clip(vec![bar(1.0, 0.05, None)])),
                record_time: 1.0,
                picture: (0, 0, 800, 800),
                highlights: &shapes,
                text: "",
                scoreboard: None,
            },
            w,
            800,
        );
        // The stroke runs along y = 400, which is the ring's own centre line,
        // and the ring's left extreme (288, 400) is under it.
        assert_eq!(at(&px, w, 288, 400), [255, 51, 51, 255]);
        // The ring is still drawn where the stroke doesn't reach.
        assert!(near(&px, w, 400, 439, 3, blue), "no ring below the stroke");
    }

    /// The label sits in a pill of the highlight's own colour, above the box.
    #[test]
    fn the_label_pill_sits_above_the_box() {
        let (w, h) = (800, 800);
        let shapes = shapes(
            rgba(0.0, 0.0, 1.0),
            "#7",
            norm(0.4, 0.4, 0.2, 0.2),
            800.0,
            800.0,
        );
        let px = render_rings(&shapes, (0, 0, 800, 800), w, h);
        // The pill is a line high plus its padding, a small gap above the
        // box's top edge at y = 320.
        let rows = ink_rows(&px, w, h, white);
        let (&top, &bottom) = (
            rows.first().expect("no label was drawn"),
            rows.last().expect("no label was drawn"),
        );
        assert!(
            bottom < 320,
            "the label is not above the box: {top}..{bottom}"
        );
        assert!(top > 260, "the label is far above the box: {top}..{bottom}");
        // It is a pill, not bare glyphs: the highlight's colour is painted
        // behind the ink.
        assert!(
            (top..=bottom).any(|y| (0..w).any(|x| blue(at(&px, w, x, y)))),
            "the label has no pill behind it"
        );
    }

    /// [`OverlayRenderer::draw_label`] bypasses the mask, so a pill is moved
    /// rather than clipped: below the box when there is no room above it, and
    /// shifted sideways at the picture's left and right edges.
    #[test]
    fn a_pill_at_an_edge_stays_inside_the_picture() {
        let picture = (160, 40, 480, 640);
        let (pw, ph) = (480.0, 640.0);
        let color = rgba(0.0, 0.0, 1.0);
        let mut all = shapes(color, "#7", norm(0.4, 0.0, 0.2, 0.2), pw, ph);
        for rect in [norm(0.0, 0.5, 0.03, 0.2), norm(0.97, 0.5, 0.03, 0.2)] {
            all.extend(shapes(color, "#77", rect, pw, ph));
        }
        let (w, h) = (800, 720);
        let px = render_rings(&all, picture, w, h);

        let outside = painted_outside(&px, picture, w, h);
        assert!(
            outside.is_empty(),
            "painted outside the picture: {outside:?}"
        );
        // The first box's top edge is the picture's, so its pill goes below
        // the box instead: under the box's bottom edge at output y = 168.
        let rows = ink_rows(&px, w, h, white);
        let &top = rows.first().expect("no label was drawn");
        assert!(top > 168, "a pill was drawn above its box: row {top}");
        // The two edge boxes' pills are shifted in rather than dropped.
        let lit = |cols: std::ops::Range<u32>| {
            (0..h)
                .flat_map(|y| cols.clone().map(move |x| (x, y)))
                .any(|(x, y)| white(at(&px, w, x, y)))
        };
        assert!(lit(160..220), "no label at the left edge");
        assert!(lit(580..640), "no label at the right edge");
    }

    /// No highlights, nothing drawn — the layer as it was before them.
    #[test]
    fn no_highlights_draws_nothing() {
        let px = render_rings(&[], (0, 0, 800, 800), 800, 800);
        assert!(px.iter().all(|p| p[3] == 0));
    }

    // ------------------------------------------------------- the scoreboard

    fn rgba(r: f64, g: f64, b: f64) -> Rgba {
        Rgba { r, g, b, a: 1.0 }
    }

    /// Two teams whose six colours are all different and all primary, so a
    /// pixel says which of them painted it.
    fn scoreboard_config() -> ScoreboardConfig {
        ScoreboardConfig {
            home: TeamConfig {
                name: "HOME".into(),
                primary_color: rgba(0.0, 0.0, 1.0),
                secondary_color: rgba(1.0, 1.0, 0.0),
                font_color: rgba(1.0, 0.0, 1.0),
            },
            away: TeamConfig {
                name: "AWAY".into(),
                primary_color: rgba(1.0, 0.0, 0.0),
                secondary_color: rgba(0.0, 1.0, 1.0),
                font_color: rgba(0.0, 1.0, 0.0),
            },
            format: MatchFormat::default(),
            auto_back_anchor_p1: false,
        }
    }

    fn state(clock: ClockDisplay) -> ScoreboardState {
        ScoreboardState {
            home_score: 2,
            away_score: 1,
            clock,
        }
    }

    /// The scoreboard alone, over an empty clip and no text bar, at the
    /// export's output size.
    fn render_scoreboard(config: &ScoreboardConfig, state: ScoreboardState) -> Vec<[u8; 4]> {
        render_frame(
            &OverlayFrame {
                clip: Some(&clip(Vec::new())),
                record_time: 0.0,
                picture: (0, 0, OUT_W as i32, OUT_H as i32),
                highlights: &[],
                text: "",
                scoreboard: Some((config, state)),
            },
            OUT_W,
            OUT_H,
        )
    }

    /// How many pixels of `rect` satisfy `matches`. The rect is taken a pixel
    /// inside on every edge, so an anti-aliased boundary is never counted.
    fn count_in(px: &[[u8; 4]], rect: &LayoutRect, matches: impl Fn([u8; 4]) -> bool) -> usize {
        let rows = (rect.y.ceil() as u32 + 1)..(rect.y + rect.h) as u32;
        let cols = (rect.x.ceil() as u32 + 1)..(rect.x + rect.w) as u32;
        rows.flat_map(|y| cols.clone().map(move |x| (x, y)))
            .filter(|&(x, y)| matches(at(px, OUT_W, x, y)))
            .count()
    }

    /// The four cells take their fills.
    #[test]
    fn the_scoreboard_fills_its_cells() {
        let config = scoreboard_config();
        let px = render_scoreboard(&config, state(ClockDisplay::Running { seconds: 135.0 }));
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));

        // The bottom-left corner of each cell: inside the fill, clear of the
        // centred glyphs.
        let corner =
            |cell: &LayoutRect| at(&px, OUT_W, cell.x as u32 + 4, (cell.y + cell.h) as u32 - 4);
        assert_eq!(corner(&r.home), [0, 0, 255, 255], "the home cell");
        assert_eq!(corner(&r.away), [255, 0, 0, 255], "the away cell");
        assert_eq!(corner(&r.score), [26, 26, 26, 255], "the score cell");
        // The clock's fill is the one that isn't opaque (macOS's 0.95), and
        // the pixmap is premultiplied, so its channels sit under its alpha.
        let clock = corner(&r.clock);
        assert_eq!(clock[3], 242, "the clock cell's alpha");
        assert!(clock[0] < 16, "the clock cell is dark: {clock:?}");
        // Named for what they are: the inset's own margins.
        assert_eq!(at(&px, OUT_W, r.bar.x as u32 - 4, 60), [0, 0, 0, 0]);
        assert_eq!(at(&px, OUT_W, 60, r.bar.y as u32 - 4), [0, 0, 0, 0]);
    }

    /// Nothing at all is painted outside the bar — and outside the stoppage
    /// tail **only when the tail is drawn**, which is the hole this test used
    /// to have: it allowed the tail rect unconditionally, so a clock label
    /// spilling into it passed.
    ///
    /// The clock cell holds the longest strings on the board and every label
    /// is centred, so an overflow spills *both* ways: into the away team's
    /// colour on one side and past the bar's right edge on the other. The two
    /// that used to do it are `BREAK` — every break of every format but
    /// soccer's first reads it, and the setup sheet offers ten periods — and
    /// `104:59`, the default soccer format in overtime.
    #[test]
    fn the_scoreboard_paints_nowhere_outside_its_bar() {
        let config = scoreboard_config();
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        let cases = [
            (ClockDisplay::Running { seconds: 135.0 }, false),
            (ClockDisplay::OnBreak("BREAK"), false),
            (ClockDisplay::Running { seconds: 6299.0 }, false),
            (
                ClockDisplay::Stoppage {
                    base: 2700.0,
                    plus: 125.0,
                },
                true,
            ),
        ];
        for (clock, tail_drawn) in cases {
            let px = render_scoreboard(&config, state(clock));
            // To within the rounding of a sub-pixel rect.
            let touched: Vec<(u32, u32)> = (0..OUT_H)
                .flat_map(|y| (0..OUT_W).map(move |x| (x, y)))
                .filter(|&(x, y)| at(&px, OUT_W, x, y)[3] > 0)
                .filter(|&(x, y)| {
                    let outside = |rect: &LayoutRect| {
                        f64::from(x) < rect.x - 1.0
                            || f64::from(x) > rect.x + rect.w + 1.0
                            || f64::from(y) < rect.y - 1.0
                            || f64::from(y) > rect.y + rect.h + 1.0
                    };
                    outside(&r.bar) && (!tail_drawn || outside(&r.tail))
                })
                // A handful names the mistake; the whole frame would be two
                // million pairs in the panic message.
                .take(8)
                .collect();
            assert!(touched.is_empty(), "{clock:?} painted outside: {touched:?}");
        }
    }

    /// The accent strip is each team's secondary colour over that team's
    /// column only. `scoreboard_rects` returns it as one row across the whole
    /// bar because the score cell sits between the two columns it covers, so
    /// this is the one piece of geometry the drawer decides.
    #[test]
    fn the_accent_strip_covers_the_team_columns_only() {
        let config = scoreboard_config();
        let px = render_scoreboard(&config, state(ClockDisplay::Running { seconds: 60.0 }));
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        let row = (r.accent.y + r.accent.h / 2.0) as u32;
        let strip = |cell: &LayoutRect| at(&px, OUT_W, cell.x as u32 + 4, row);

        assert_eq!(strip(&r.home), [255, 255, 0, 255], "the home accent");
        assert_eq!(strip(&r.away), [0, 255, 255, 255], "the away accent");
        // The score and clock columns get no strip: the board's top row is
        // open above them.
        assert_eq!(strip(&r.score), [0, 0, 0, 0], "above the score");
        assert_eq!(strip(&r.clock), [0, 0, 0, 0], "above the clock");
    }

    /// The `+M:SS` tail is drawn only in stoppage, and outside the bar.
    #[test]
    fn the_stoppage_tail_is_drawn_only_in_stoppage() {
        let config = scoreboard_config();
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        let lit = |px: &[[u8; 4]], rect: &LayoutRect| count_in(px, rect, |p| p[3] > 0);

        let running = render_scoreboard(&config, state(ClockDisplay::Running { seconds: 135.0 }));
        assert_eq!(lit(&running, &r.tail), 0, "a tail with the clock running");
        assert!(lit(&running, &r.clock) > 0, "no clock at all");

        let stoppage = render_scoreboard(
            &config,
            state(ClockDisplay::Stoppage {
                base: 2700.0,
                plus: 125.0,
            }),
        );
        assert!(lit(&stoppage, &r.tail) > 0, "no tail in stoppage");
        // And it hangs past the bar rather than inside it.
        assert!(r.tail.x > r.bar.x + r.bar.w);
    }

    /// Each team's name is drawn in that team's `font_color` — the field the
    /// setup sheet sets separately from the two cell colours.
    #[test]
    fn each_team_name_is_drawn_in_its_own_font_color() {
        let config = scoreboard_config();
        let px = render_scoreboard(&config, state(ClockDisplay::Running { seconds: 1.0 }));
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        // Magenta and green over blue and red cells: a glyph pixel is the only
        // place either can come from, and neither fill is close to either.
        let magenta = |p: [u8; 4]| p[0] > 200 && p[1] < 64 && p[2] > 200;
        let green = |p: [u8; 4]| p[0] < 64 && p[1] > 200 && p[2] < 64;

        assert!(count_in(&px, &r.home, magenta) > 50, "no home name");
        assert!(count_in(&px, &r.away, green) > 50, "no away name");
        assert_eq!(count_in(&px, &r.home, green), 0, "the away colour at home");
        assert_eq!(count_in(&px, &r.away, magenta), 0, "the home colour away");
    }

    /// The board is drawn last, so a drawing under it never shows through.
    /// macOS drew it on top of everything and so does this.
    #[test]
    fn the_scoreboard_covers_a_stroke_under_it() {
        let across = CommentaryEvent::new(
            1.0,
            EventKind::Stroke(Stroke {
                id: Uuid::nil(),
                color: Rgba::RED,
                line_width: 0.05,
                // Across the board's cells and out past them, a twentieth of
                // the way down the picture.
                points: [0.01, 0.5]
                    .into_iter()
                    .enumerate()
                    .map(|(i, x)| StrokePoint {
                        x,
                        y: 0.055,
                        t: i as f64 * 0.1,
                    })
                    .collect(),
                auto_clear_after_seconds: None,
            }),
        );
        let config = scoreboard_config();
        let px = render_frame(
            &OverlayFrame {
                clip: Some(&clip(vec![across])),
                record_time: 1.5,
                picture: (0, 0, OUT_W as i32, OUT_H as i32),
                highlights: &[],
                text: "",
                scoreboard: Some((&config, state(ClockDisplay::Running { seconds: 1.0 }))),
            },
            OUT_W,
            OUT_H,
        );
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        // The stroke crosses this row of the home cell; the cell's fill wins.
        let y = (0.055 * f64::from(OUT_H)) as u32;
        assert_eq!(at(&px, OUT_W, r.home.x as u32 + 4, y), [0, 0, 255, 255]);
        // And it is still there past the board's right edge.
        assert_eq!(at(&px, OUT_W, 900, y), [255, 51, 51, 255]);
    }

    /// A frame with no scoreboard leaves the board's rects alone: the two
    /// "draw nothing" cases (not configured, nothing tagged yet) reach here as
    /// one `None`.
    #[test]
    fn a_frame_without_a_scoreboard_draws_no_board() {
        let px = render_at(
            &clip(Vec::new()),
            0.0,
            "1 / 3 | Kick-off",
            (0, 0, OUT_W as i32, OUT_H as i32),
            OUT_W,
            OUT_H,
        );
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        assert_eq!(count_in(&px, &r.bar, |p| p[3] > 0), 0);
        assert_eq!(count_in(&px, &r.tail, |p| p[3] > 0), 0);
    }

    /// The clock column is wide enough for everything the clock can read, so
    /// the clock is drawn at the board's own size rather than shrinking to
    /// fit. This is what the column widths are for, and the reason they are
    /// not the parent spec's: at the 0.20 the clock once had, `BREAK` needed
    /// 3.76 em in a 3.16 em cell, `104:59` needed 3.88 — and even an ordinary
    /// `00:00` needed 3.18.
    #[test]
    fn every_clock_label_fits_its_cell_at_full_size() {
        let mut renderer = OverlayRenderer::new();
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        let cell_h = r.home.h;
        let full = (cell_h * SCOREBOARD_FONT_RATIO) as f32;
        let floor = (cell_h * SCOREBOARD_MIN_FONT_RATIO) as f32;
        let style = Style::new(full, Weight::BOLD);
        // `format_clock`'s whole range: a plain time, a stoppage base, both
        // break labels, full time, and the longest a clock reaches in
        // practice — the default soccer format in overtime.
        for clock in [
            ClockDisplay::Running { seconds: 0.0 },
            ClockDisplay::Running { seconds: 6299.0 },
            ClockDisplay::Stoppage {
                base: 2700.0,
                plus: 125.0,
            },
            ClockDisplay::OnBreak("HT"),
            ClockDisplay::OnBreak("BREAK"),
            ClockDisplay::Fulltime,
        ] {
            let labels = format_clock(clock);
            let (line, size) = renderer.fit(&labels.main, style, floor, r.clock.w as f32);
            assert_eq!(
                (line.as_str(), size),
                (labels.main.as_str(), full),
                "{clock:?} does not fit the clock cell at full size"
            );
            if labels.trailing.is_empty() {
                continue;
            }
            let tail = Style::new((cell_h * SCOREBOARD_TAIL_FONT_RATIO) as f32, Weight::NORMAL);
            let (line, size) = renderer.fit(&labels.trailing, tail, floor, r.tail.w as f32);
            assert_eq!(
                (line.as_str(), size),
                (labels.trailing.as_str(), tail.metrics.font_size),
                "{clock:?}'s tail does not fit at full size"
            );
        }
    }

    /// A team name too wide for its cell is **shrunk whole** rather than cut
    /// to a stub, and only cut once shrinking would take it under the floor.
    /// Spec S3 originally refused shrink-to-fit because "a shrunk long name is
    /// illegible anyway"; measured, `Manchester United` fits at 17 px on a
    /// 1080p frame, which is not.
    #[test]
    fn a_long_team_name_is_shrunk_and_only_cut_under_the_floor() {
        let mut renderer = OverlayRenderer::new();
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        let cell_h = r.home.h;
        let full = (cell_h * SCOREBOARD_FONT_RATIO) as f32;
        let floor = (cell_h * SCOREBOARD_MIN_FONT_RATIO) as f32;
        let max_width = r.home.w as f32 - 2.0 * (cell_h * SCOREBOARD_NAME_PAD_RATIO) as f32;
        let style = Style::new(full, Weight::BOLD);
        let fits = |renderer: &mut OverlayRenderer, line: &str, size: f32| {
            renderer.width(line, Style::new(size, Weight::BOLD)) <= max_width
        };

        // A name that fits is left exactly as it is, at full size.
        let (line, size) = renderer.fit("HOME", style, floor, max_width);
        assert_eq!((line.as_str(), size), ("HOME", full));

        // One that doesn't keeps every character and loses size instead.
        let (line, size) = renderer.fit("Manchester United", style, floor, max_width);
        assert_eq!(line, "Manchester United");
        assert!(
            size < full && size > floor,
            "{size} outside ({floor}, {full})"
        );
        assert!(fits(&mut renderer, &line, size));

        // The longest real club name measured still clears the floor, and it
        // is what the floor was chosen against: it needs 0.265 of the full
        // size where the floor is 0.25. A failure here means the floor wants
        // revisiting, not that this name is special.
        let (line, size) = renderer.fit("Wolverhampton Wanderers", style, floor, max_width);
        assert_eq!(line, "Wolverhampton Wanderers");
        assert!(fits(&mut renderer, &line, size));

        // Below it a line is cut rather than smeared to nothing: without the
        // floor this one shapes at about a pixel and a half.
        let absurd = "Association Football Club of the Northern Riverside Parishes";
        let (line, size) = renderer.fit(absurd, style, floor, max_width);
        assert!(line.ends_with(ELLIPSIS), "{line:?} has no ellipsis");
        assert!(absurd.starts_with(line.trim_end_matches(ELLIPSIS)));
        assert_eq!(size, floor);
        assert!(fits(&mut renderer, &line, size));
    }

    /// The fitted size reaches the **drawing**, not just the fit: a shrunk
    /// name's ink is a fraction of a full-size one's, and it stays in its own
    /// cell rather than reaching the score.
    #[test]
    fn a_shrunk_team_name_is_drawn_at_its_fitted_size() {
        let magenta = |p: [u8; 4]| p[0] > 200 && p[1] < 64 && p[2] > 200;
        let r = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
        // How many rows of the home cell carry the home name's ink.
        let ink_rows = |name: &str| {
            let mut config = scoreboard_config();
            config.home.name = name.into();
            let px = render_scoreboard(&config, state(ClockDisplay::Running { seconds: 60.0 }));
            assert_eq!(
                count_in(&px, &r.score, magenta),
                0,
                "{name:?} reached the score cell"
            );
            (r.home.y as u32..(r.home.y + r.home.h) as u32)
                .filter(|&y| {
                    (r.home.x as u32..(r.home.x + r.home.w) as u32)
                        .any(|x| magenta(at(&px, OUT_W, x, y)))
                })
                .count()
        };
        let full = ink_rows("HOME");
        let shrunk = ink_rows("Wolverhampton Wanderers");
        assert!(full > 0 && shrunk > 0, "no name drawn: {full}, {shrunk}");
        assert!(shrunk * 2 < full, "{shrunk} rows is not shrunk from {full}");
    }

    /// Both faces are loaded under one family, so the weight is what picks
    /// between them — and picking is silent when it goes wrong. Bold DejaVu is
    /// wider than regular at the same size, which is the cheapest proof that
    /// two different faces were actually reached.
    #[test]
    fn the_weight_picks_between_the_two_vendored_faces() {
        let mut renderer = OverlayRenderer::new();
        let line = "Hamburgefonstiv 12:34";
        let regular = renderer.width(line, Style::new(40.0, Weight::NORMAL));
        let bold = renderer.width(line, Style::new(40.0, Weight::BOLD));
        assert!(bold > regular, "bold {bold} is not wider than {regular}");
    }

    /// The two vendored faces are the only fonts in the database. Without
    /// this, `cosmic-text` scans the machine (432 faces on the reference
    /// laptop, none on CI) and the picture stops being the same everywhere.
    #[test]
    fn only_the_vendored_faces_are_loaded() {
        let fonts = font_system();
        let faces: Vec<_> = fonts.db().faces().collect();
        assert_eq!(faces.len(), 2, "{} faces loaded", faces.len());
        for face in &faces {
            assert!(
                face.families.iter().any(|(name, _)| name == FONT_FAMILY),
                "{:?} is not {FONT_FAMILY}",
                face.families
            );
        }
        let mut weights: Vec<_> = faces.iter().map(|f| f.weight).collect();
        weights.sort_by_key(|w| w.0);
        assert_eq!(weights, [Weight::NORMAL, Weight::BOLD]);
    }
}
