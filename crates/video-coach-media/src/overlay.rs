//! The vector overlay layer, rasterized once per output frame (spec P4, E2).
//!
//! Phase 8 draws the strokes and the text bar; Phase 9's scoreboard joins this
//! file. Geometry stays in core ([`video_coach_core::layout`]), pixels stay
//! here.
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
//! **The font is vendored** (`fonts/DejaVuSans.ttf`, with its licence beside
//! it) and it is the only font loaded: `cosmic-text`'s system-font scanning is
//! off, so an export's bar looks the same on this machine, on another coach's,
//! and on a CI runner with no fonts installed at all.
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
    Wrap,
};
use gstreamer as gst;
use gstreamer_video as gst_video;
use tiny_skia::{Color, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Rect, Transform};
use video_coach_core::layout::{
    bar_rect, stroke_line_width, Rect as LayoutRect, BAR_FONT_RATIO, BAR_INSET_RATIO,
};
use video_coach_core::project::Clip;
use video_coach_core::stroke_replay::visible_strokes;

/// The one font the bar is drawn in. Vendored so the picture doesn't depend on
/// what the machine happens to have installed.
const FONT: &[u8] = include_bytes!("../fonts/DejaVuSans.ttf");

/// What a line too long for the bar ends in.
const ELLIPSIS: &str = "…";

/// The bar's background, over the picture: macOS's black at 60%.
const BAR_ALPHA: f32 = 0.6;

/// Line height as a multiple of the font size — cosmic-text has no default,
/// and this is the usual one.
const LINE_HEIGHT: f32 = 1.2;

/// One frame's overlay: what to draw, and the spaces to draw it in.
pub(crate) struct OverlayFrame<'a> {
    /// The drawings' clip.
    pub clip: &'a Clip,
    /// Where in the recording the frame sits, which is the clock stroke replay
    /// runs on.
    pub record_time: f64,
    /// The letterboxed picture rect inside the output frame, `(x, y, w, h)`:
    /// the base pad's rect, which is the space the strokes were drawn in.
    pub picture: (i32, i32, i32, i32),
    /// The bar's line. **Empty draws no bar at all** — neither its background
    /// nor its glyphs: that is how a caller suppresses the bar. Neither
    /// shipping caller does; both draw the entry's own line (spec E7).
    pub text: &'a str,
}

/// Rasterizes overlay frames, holding the font between them.
///
/// One per pump thread: `FontSystem` and `SwashCache` are `&mut` to draw with,
/// and building a `FontSystem` per frame would re-parse the TTF.
pub(crate) struct OverlayRenderer {
    fonts: FontSystem,
    cache: SwashCache,
    /// The last line fitted to the bar, so the search in [`Self::ellipsized`]
    /// runs once an entry rather than once a frame: the line changes only at
    /// an entry boundary.
    fitted: Option<Fitted>,
}

/// A remembered result of [`OverlayRenderer::ellipsized`], with what it was
/// fitted to.
struct Fitted {
    line: String,
    font_size: f32,
    max_width: f32,
    result: String,
}

impl OverlayRenderer {
    pub(crate) fn new() -> OverlayRenderer {
        OverlayRenderer {
            fonts: FontSystem::new_with_fonts([fontdb::Source::Binary(Arc::new(FONT))]),
            cache: SwashCache::new(),
            fitted: None,
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
    /// the glyphs. A drawing near the bottom of the picture stays visible over
    /// the bar's tint, and the words stay legible over the drawing.
    fn draw(&mut self, pixmap: &mut PixmapMut, frame: &OverlayFrame) {
        // The allocator hands back whatever was in that memory, and nothing
        // else clears it: `from_bytes` adopts the bytes as they are.
        pixmap.fill(Color::TRANSPARENT);

        let bar = bar_rect(f64::from(pixmap.width()), f64::from(pixmap.height()));
        if !frame.text.is_empty() {
            if let Some(rect) =
                Rect::from_xywh(bar.x as f32, bar.y as f32, bar.w as f32, bar.h as f32)
            {
                let paint = Paint {
                    shader: tiny_skia::Shader::SolidColor(
                        Color::from_rgba(0.0, 0.0, 0.0, BAR_ALPHA).expect("a valid colour"),
                    ),
                    anti_alias: false,
                    ..Paint::default()
                };
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }

        draw_strokes(pixmap, frame);

        if !frame.text.is_empty() {
            self.draw_text(pixmap, frame.text, &bar);
        }
    }

    /// Draws `line` inside `bar`, on one line, clipped with a tail ellipsis.
    fn draw_text(&mut self, pixmap: &mut PixmapMut, line: &str, bar: &LayoutRect) {
        let font_size = (bar.h * BAR_FONT_RATIO) as f32;
        let inset = (bar.h * BAR_INSET_RATIO) as f32;
        let metrics = Metrics::new(font_size, font_size * LINE_HEIGHT);
        let max_width = bar.w as f32 - 2.0 * inset;
        if max_width <= 0.0 || font_size <= 0.0 {
            return;
        }
        let line = self.ellipsized(line, metrics, max_width);

        let mut buffer = self.shaped(&line, metrics, Some(max_width));
        // Left at the inset, and vertically centred on the bar: the glyphs'
        // own box is one line high, so centring it centres the ascender and
        // descender together.
        let left = (bar.x as f32 + inset).round() as i32;
        let top = (bar.y as f32 + (bar.h as f32 - metrics.line_height) / 2.0).round() as i32;

        let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
        let data = pixmap.data_mut();
        let (fonts, cache) = (&mut self.fonts, &mut self.cache);
        buffer.draw(
            fonts,
            cache,
            TextColor::rgb(255, 255, 255),
            |x, y, w, h, color| {
                let a = u32::from(color.a());
                if a == 0 {
                    return;
                }
                // `color` is straight alpha; the pixmap is premultiplied.
                let src =
                    [color.r(), color.g(), color.b(), color.a()].map(|c| u32::from(c) * a / 255);
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
            },
        );
    }

    /// `line` cut to at most `max_width`, with a tail ellipsis if it didn't
    /// fit.
    ///
    /// [`Wrap::None`] puts the whole line on one row whatever its width, so
    /// without this a realistic clip line runs off the right of the frame
    /// (measured: 143 characters overflows at every resolution). macOS wrapped
    /// it onto a second row, which landed over the picture.
    fn ellipsized(&mut self, line: &str, metrics: Metrics, max_width: f32) -> String {
        let size = metrics.font_size;
        if let Some(f) = &self.fitted {
            if f.line == line && f.font_size == size && f.max_width == max_width {
                return f.result.clone();
            }
        }
        let result = self.fit(line, metrics, max_width);
        self.fitted = Some(Fitted {
            line: line.to_owned(),
            font_size: size,
            max_width,
            result: result.clone(),
        });
        result
    }

    /// [`Self::ellipsized`] without the memo.
    fn fit(&mut self, line: &str, metrics: Metrics, max_width: f32) -> String {
        if self.width(line, metrics) <= max_width {
            return line.to_owned();
        }
        // The longest prefix that still fits with an ellipsis after it.
        // Shaping is the cost here, so this bisects the character boundaries
        // rather than shaping once per character.
        let cuts: Vec<usize> = line
            .char_indices()
            .map(|(i, _)| i)
            .chain([line.len()])
            .collect();
        let with_ellipsis = |cut: usize| format!("{}{ELLIPSIS}", &line[..cut]);
        let (mut low, mut high) = (0, cuts.len() - 1);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if self.width(&with_ellipsis(cuts[mid]), metrics) <= max_width {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        // `low` is 0 when even one character and an ellipsis are too wide, and
        // a bare ellipsis is then the honest answer.
        with_ellipsis(cuts[low])
    }

    /// How wide `line` is, shaped on one unbounded row.
    fn width(&mut self, line: &str, metrics: Metrics) -> f32 {
        self.shaped(line, metrics, None)
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max)
    }

    /// `line` shaped on one row, at most `max_width` wide if given.
    fn shaped(&mut self, line: &str, metrics: Metrics, max_width: Option<f32>) -> Buffer {
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(max_width, Some(metrics.line_height));
        buffer.set_text(
            line,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }
}

/// Draws the visible strokes over `pixmap`, mapped into the picture rect.
///
/// The pen denormalizes against the picture's **height**, not the output's:
/// a stroke keeps the weight it was drawn with when the picture is
/// pillarboxed (`core::layout::stroke_line_width`).
fn draw_strokes(pixmap: &mut PixmapMut, frame: &OverlayFrame) {
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

    for visible in visible_strokes(frame.clip, frame.record_time) {
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
        paint.set_color(color);
        pen.width = stroke_line_width(stroke.line_width, h) as f32;
        pixmap.stroke_path(&path, &paint, &pen, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;
    use video_coach_core::event::{CommentaryEvent, EventKind};
    use video_coach_core::layout::{BAR_HEIGHT_RATIO, PIP_WIDTH_RATIO};
    use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};

    use super::*;

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

    /// The rendered pixels, as `[r, g, b, a]` per pixel in row order, for a
    /// `w`×`h` output whose picture is `picture` and whose bar reads `text`.
    fn render_at(
        clip: &Clip,
        record_time: f64,
        text: &str,
        picture: (i32, i32, i32, i32),
        w: u32,
        h: u32,
    ) -> Vec<[u8; 4]> {
        gst::init().unwrap();
        let buffer = OverlayRenderer::new().render(
            &OverlayFrame {
                clip,
                record_time,
                picture,
                text,
            },
            w,
            h,
        );
        let meta = buffer.meta::<gst_video::VideoMeta>().expect("a VideoMeta");
        assert_eq!(meta.format(), gst_video::VideoFormat::Rgba);
        assert_eq!((meta.width(), meta.height()), (w, h));
        assert_eq!(meta.stride(), [w as i32 * 4]);

        let map = buffer.map_readable().unwrap();
        assert_eq!(map.len(), (w * h * 4) as usize);
        map.as_chunks::<4>().0.to_vec()
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
        // to pick (tiny-skia's anti-aliasing is not a stable contract).
        let thickness = |w: u32, h: u32| {
            let px = render(&clip, 1.0, w, h);
            let alpha: u32 = (0..h).map(|y| u32::from(at(&px, w, w / 2, y)[3])).sum();
            f64::from(alpha) / 255.0
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
        assert_eq!(at(&px, 1280, 330, 360), [0, 0, 0, 0]);
        // The pen is 0.05 x 720 = 36 px, from the picture's height and not the
        // output's (identical here) nor its width.
        let alpha: u32 = (0..720).map(|y| u32::from(at(&px, 1280, 640, y)[3])).sum();
        assert!((f64::from(alpha) / 255.0 - 36.0).abs() < 1.0);
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
        let font_size = (bar.h * BAR_FONT_RATIO) as f32;
        let metrics = Metrics::new(font_size, font_size * LINE_HEIGHT);
        let max_width = bar.w as f32 - 2.0 * (bar.h * BAR_INSET_RATIO) as f32;

        let fitted = renderer.ellipsized(long, metrics, max_width);
        assert!(fitted.ends_with(ELLIPSIS), "{fitted:?} has no ellipsis");
        assert!(long.starts_with(fitted.trim_end_matches(ELLIPSIS)));
        assert!(renderer.width(&fitted, metrics) <= max_width);
        // And it is the longest such cut: one more character overflows.
        let kept = fitted.trim_end_matches(ELLIPSIS).chars().count();
        let longer = format!(
            "{}{ELLIPSIS}",
            long.chars().take(kept + 1).collect::<String>()
        );
        assert!(renderer.width(&longer, metrics) > max_width);

        // A line that fits is left exactly as it is.
        let short = "3 / 7 | Turnover";
        assert_eq!(renderer.ellipsized(short, metrics, max_width), short);
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
}
