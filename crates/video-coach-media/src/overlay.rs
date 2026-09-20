//! The vector overlay layer, rasterized once per output frame (spec P4).
//!
//! Phase 7 draws strokes only. Phase 8's text bar and Phase 9's scoreboard join
//! this file, and so does the font when one is needed: geometry stays in core,
//! pixels stay here.
//!
//! **The rect is the picture, not the frame.** `w`/`h` are the letterboxed
//! content rect — the mixer pad this buffer goes on takes the same rect as the
//! base pad, so the caller passes `fit_rect`'s size. Strokes are normalized to
//! that rect (they were drawn on the picture), so rasterizing at the output
//! size would stretch them across the letterbox bars on any source whose aspect
//! isn't the output's. It is also the cheaper of the two: the rect is at most
//! the output size, and a 1080p overlay into a 720p preview measured 2.5× the
//! UI's p95 frame time.
//!
//! **Premultiplied, which the mixer pad has to be told.** tiny-skia stores
//! premultiplied pixels; GStreamer's `RGBA` means straight alpha. Nothing here
//! demultiplies — the mixer pad carrying this buffer sets
//! `blend-function-src-rgb=one` instead (the destination function already
//! defaults to `one-minus-src-alpha`), which is premultiplied-over for free on
//! the GPU.
//!
//! **A fresh buffer per frame, no pool.** Drawing 720p is sub-millisecond,
//! while recycling a pixmap would need destroy-notify bookkeeping to avoid
//! overwriting a frame still queued in the mixer.

use gstreamer as gst;
use gstreamer_video as gst_video;
use tiny_skia::{Color, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Transform};
use video_coach_core::layout::stroke_line_width;
use video_coach_core::project::Clip;
use video_coach_core::stroke_replay::visible_strokes;

/// `clip`'s overlay at `record_time`, as a `w`×`h` premultiplied-RGBA buffer
/// with a `VideoMeta`.
///
/// The buffer carries no timestamp: the pump stamps it with the same PTS as
/// the base frame it belongs to, or the mixer starves.
pub(crate) fn render_overlay(clip: &Clip, record_time: f64, w: u32, h: u32) -> gst::Buffer {
    // tiny-skia assumes a tightly packed `w * 4` stride, which is what the
    // default allocator gives; the `VideoMeta` states it rather than leaving
    // `glupload` to infer it from the caps.
    let size = w as usize * h as usize * 4;
    let mut buffer = gst::Buffer::with_size(size).expect("one overlay frame fits in memory");
    let writable = buffer
        .get_mut()
        .expect("a freshly allocated buffer is writable");
    gst_video::VideoMeta::add(
        writable,
        gst_video::VideoFrameFlags::empty(),
        gst_video::VideoFormat::Rgba,
        w,
        h,
    )
    .expect("RGBA meta for a buffer allocated at exactly that size");
    let mut map = writable
        .map_writable()
        .expect("a freshly allocated buffer maps writable");
    let mut pixmap =
        PixmapMut::from_bytes(map.as_mut_slice(), w, h).expect("the picture rect is never empty");
    draw(&mut pixmap, clip, record_time);
    // The map holds the buffer borrowed until it goes, and it would otherwise
    // go at the end of the function -- after the return.
    drop(map);
    buffer
}

/// Draws the visible strokes over `pixmap`, in pixels of the picture rect.
fn draw(pixmap: &mut PixmapMut, clip: &Clip, record_time: f64) {
    // The allocator hands back whatever was in that memory, and nothing else
    // clears it: `from_bytes` adopts the bytes as they are.
    pixmap.fill(Color::TRANSPARENT);

    let (w, h) = (f64::from(pixmap.width()), f64::from(pixmap.height()));
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

    for visible in visible_strokes(clip, record_time) {
        let stroke = visible.stroke;
        let Some((first, rest)) = stroke.points[..visible.drawn_point_count].split_first() else {
            continue;
        };
        let point =
            |p: &video_coach_core::stroke::StrokePoint| ((p.x * w) as f32, (p.y * h) as f32);

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

    /// The rendered pixels, as `[r, g, b, a]` per pixel in row order.
    fn render(clip: &Clip, record_time: f64, w: u32, h: u32) -> Vec<[u8; 4]> {
        gst::init().unwrap();
        let buffer = render_overlay(clip, record_time, w, h);
        let meta = buffer.meta::<gst_video::VideoMeta>().expect("a VideoMeta");
        assert_eq!(meta.format(), gst_video::VideoFormat::Rgba);
        assert_eq!((meta.width(), meta.height()), (w, h));
        assert_eq!(meta.stride(), [w as i32 * 4]);

        let map = buffer.map_readable().unwrap();
        assert_eq!(map.len(), (w * h * 4) as usize);
        map.as_chunks::<4>().0.to_vec()
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
    /// rect's height and ignores its width. Rendering at the output size
    /// instead of the picture rect would get this wrong on a non-16:9 source.
    #[test]
    fn the_line_width_scales_with_the_rect_height_only() {
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
}
