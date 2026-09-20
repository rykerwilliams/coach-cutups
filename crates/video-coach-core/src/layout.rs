//! Where the composite's furniture sits, as ratios of the frame it lands on.
//!
//! Every value here is a ratio, never a pixel count, so preview (1280×720) and
//! export (1920×1080) lay out identically from one set of numbers — the parent
//! spec's "Layout constants the port must reproduce". Phase 7 needs the stroke
//! and PiP rows; the text bar (Phase 8) and the scoreboard (Phase 9) joined
//! them when something drew them.
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
//!   the picture. So does the text bar (Phase 8).

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

/// The text bar's height, as a fraction of the **output height** (macOS's
/// `size.height * 0.08`).
pub const BAR_HEIGHT_RATIO: f64 = 0.08;

/// The glyphs' inset inside the bar, as a fraction of the **bar's height** —
/// the same inset on both axes, so the text doesn't kiss its edges.
pub const BAR_INSET_RATIO: f64 = 0.15;

/// The bar's font size, as a fraction of the **bar's height**. Small enough
/// that a line plus its ascender and descender fits the inset rect.
pub const BAR_FONT_RATIO: f64 = 0.5;

/// The text bar's rect in output space: a full-width strip along the bottom.
pub fn bar_rect(out_w: f64, out_h: f64) -> Rect {
    let h = BAR_HEIGHT_RATIO * out_h;
    Rect {
        x: 0.0,
        y: out_h - h,
        w: out_w,
        h,
    }
}

/// The webcam inset's rect in output space, flush to the right edge and
/// sitting **on** the text bar rather than over it.
///
/// Width comes from the output; height comes from `cam_aspect`, so the camera
/// is never stretched or letterboxed inside the inset. `cam_aspect` is the
/// **display** aspect (width ÷ height with the pixel aspect ratio applied) and
/// must be positive: a camera reporting neither is not a camera.
///
/// **The bottom margin is the bar's height plus the margin** (spec E2). macOS
/// split the bar's background and its glyphs across two layers precisely
/// because its PiP overlapped the bar; raising the PiP removes the need, and
/// the bar is drawn in one piece.
pub fn pip_rect(out_w: f64, out_h: f64, cam_aspect: f64) -> Rect {
    let w = PIP_WIDTH_RATIO * out_w;
    let h = w / cam_aspect;
    let margin = PIP_MARGIN_RATIO * out_h;
    Rect {
        x: out_w - margin - w,
        y: bar_rect(out_w, out_h).y - margin - h,
        w,
        h,
    }
}

// --------------------------------------------------------------- scoreboard
//
// The scoreboard's own ratios, deliberately **not** shared with the text bar's:
// `SCOREBOARD_HEIGHT_RATIO` happens to equal `BAR_HEIGHT_RATIO` today, and
// tying the two together would make one of them impossible to change.
//
// Everything below the bar itself is a fraction of the **cell height**
// (`bar.h` less the accent strip), not of the bar — the parent spec's table
// says `bar.h` and is ~9% too large.

/// The bar's width, as a fraction of the **output width**.
const SCOREBOARD_WIDTH_RATIO: f64 = 0.36;

/// The bar's height, as a fraction of the **output height**.
const SCOREBOARD_HEIGHT_RATIO: f64 = 0.08;

/// The bar's gap from the top and left edges, as a fraction of the **output
/// height** — the same fraction on both axes, so the gap is square in pixels.
const SCOREBOARD_INSET_RATIO: f64 = 0.015;

/// The accent strip's height, as a fraction of the **bar's height**.
const SCOREBOARD_ACCENT_RATIO: f64 = 0.08;

/// The cell widths, as fractions of the **bar's width**. The clock takes
/// whatever is left, so the four tile the bar exactly.
const SCOREBOARD_HOME_RATIO: f64 = 0.30;
const SCOREBOARD_SCORE_RATIO: f64 = 0.20;
const SCOREBOARD_AWAY_RATIO: f64 = 0.30;

/// The stoppage tail's gap from the clock cell, as a fraction of the **cell
/// height** (macOS used an absolute 2 pt, which changes meaning with
/// resolution).
const SCOREBOARD_TAIL_GAP_RATIO: f64 = 0.025;

/// The four labels' font size, as a fraction of the **cell height**
/// ([`ScoreboardRects::home`]`.h`).
pub const SCOREBOARD_FONT_RATIO: f64 = 0.55;

/// The stoppage tail's font size, as a fraction of the **cell height**. It is
/// the one label that is not bold.
pub const SCOREBOARD_TAIL_FONT_RATIO: f64 = 0.45;

/// A team name's padding inside its cell, as a fraction of the **cell height**
/// (macOS used an absolute 4 pt).
pub const SCOREBOARD_NAME_PAD_RATIO: f64 = 0.05;

/// Where every piece of the scoreboard sits, in output space.
///
/// The five labels are centred in `home`, `score`, `away`, `clock` and `tail`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreboardRects {
    /// The whole bar: the accent strip plus the row of cells.
    pub bar: Rect,
    /// The accent strip, **above** the cells and spanning the bar. It is drawn
    /// over the **home and away columns only**, in each team's secondary
    /// color, so the drawer intersects it with those two cells' `x` and `w`.
    pub accent: Rect,
    pub home: Rect,
    pub score: Rect,
    pub away: Rect,
    pub clock: Rect,
    /// The `+M:SS` stoppage tail, which hangs **outside** the bar past the
    /// clock cell and is drawn only while the clock is in stoppage.
    pub tail: Rect,
}

/// The scoreboard's rects in output space, anchored to the top-left.
pub fn scoreboard_rects(out_w: f64, out_h: f64) -> ScoreboardRects {
    let inset = SCOREBOARD_INSET_RATIO * out_h;
    let bar = Rect {
        x: inset,
        y: inset,
        w: SCOREBOARD_WIDTH_RATIO * out_w,
        h: SCOREBOARD_HEIGHT_RATIO * out_h,
    };
    let accent = Rect {
        h: SCOREBOARD_ACCENT_RATIO * bar.h,
        ..bar
    };
    let cell = |x: f64, w: f64| Rect {
        x,
        y: bar.y + accent.h,
        w,
        h: bar.h - accent.h,
    };

    let home = cell(bar.x, SCOREBOARD_HOME_RATIO * bar.w);
    let score = cell(home.x + home.w, SCOREBOARD_SCORE_RATIO * bar.w);
    let away = cell(score.x + score.w, SCOREBOARD_AWAY_RATIO * bar.w);
    // The clock closes the bar rather than taking a fourth ratio, so the cells
    // tile it exactly instead of to within a rounding error.
    let clock = cell(away.x + away.w, bar.x + bar.w - (away.x + away.w));
    let tail = cell(
        clock.x + clock.w + SCOREBOARD_TAIL_GAP_RATIO * clock.h,
        clock.w,
    );

    ScoreboardRects {
        bar,
        accent,
        home,
        score,
        away,
        clock,
        tail,
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
