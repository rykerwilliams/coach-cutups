//! The stroke being drawn, and the SVG path a stroke renders as (spec D2,
//! D5). The UI thread owns the pointer and the clock; this module owns the
//! buffer, the thinning rule and the coordinates, so all of it is testable
//! without a window.
//!
//! **Coordinates.** The drawing area *is* the content rect (the letterboxed
//! picture at 1×), so a pointer position is already relative to it. An
//! in-progress stroke is buffered in those logical pixels and normalized once
//! at pen-up, by the rect as it is then. Do not route this through
//! `zoom_input::Viewport`, whose fractions are of the *player* area and would
//! shift every stroke by the letterbox offset.
//!
//! **Time.** Every method takes `now_ns` from the caller, on `now_ns()`'s
//! clock — the bus contract: a timestamp is captured at the input event, not
//! when something downstream gets round to it.

use uuid::Uuid;
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};

/// A point is kept only once this long has passed since the last kept one.
const MIN_INTERVAL: f64 = 1.0 / 60.0;
/// ... and this far, in content-rect logical pixels.
const MIN_DISTANCE: f64 = 1.0;
/// A stroke's width, as a fraction of the frame's height.
const LINE_WIDTH: f64 = 0.005;

/// The stroke under the pen: content-rect pixels, and seconds since the
/// press.
#[derive(Debug, Clone, PartialEq)]
pub struct InProgress {
    /// When the pen went down, on `now_ns()`'s clock.
    start_ns: u64,
    /// `(x, y)` in content-rect logical pixels, `t` in seconds from
    /// `start_ns`. Never empty: the press is the first point.
    points: Vec<(f64, f64, f64)>,
}

impl InProgress {
    /// The pen went down at `(x, y)`, which becomes the first point, at
    /// `t = 0`.
    pub fn start(start_ns: u64, x: f64, y: f64) -> InProgress {
        InProgress {
            start_ns,
            points: vec![(x, y, 0.0)],
        }
    }

    /// The pointer moved to `(x, y)`. It is kept only if it clears **both**
    /// gates against the last **kept** point; a rejected point doesn't move
    /// that reference, so a slow drift still eventually registers (macOS
    /// parity). Returns whether it was kept, i.e. whether [`Self::commands`]
    /// has changed — Slint re-parses a path whenever its `commands` is set.
    pub fn moved(&mut self, x: f64, y: f64, now_ns: u64) -> bool {
        let t = self.elapsed(now_ns);
        let &(lx, ly, lt) = self.last();
        let keep = t - lt >= MIN_INTERVAL && (x - lx).hypot(y - ly) >= MIN_DISTANCE;
        if keep {
            self.points.push((x, y, t));
        }
        keep
    }

    /// The commands for the part drawn so far, in content-rect pixels — the
    /// buffer's own units, so no rect is needed.
    pub fn commands(&self) -> String {
        commands(self.points.iter().map(|&(x, y, _)| (x, y)))
    }

    /// The pen came up at `(x, y)`: the finished stroke, and the moment of
    /// its last point on `now_ns()`'s clock, which is what
    /// `Command::Stroke`'s `host_ns` must be — `visible_strokes` back-computes
    /// the stroke's start from it.
    ///
    /// The release either appends its position or, when it fails the distance
    /// gate, **moves the last point's time** to the release (spec D2). Either
    /// way the last point's time is the real pen-up, so a stroke held still
    /// before lifting is not stamped early, and a plain click stays one point.
    ///
    /// `rect` is the content rect's `(width, height)`, which the caller has
    /// already established is positive: the press came from inside it.
    pub fn release(
        mut self,
        x: f64,
        y: f64,
        now_ns: u64,
        rect: (f64, f64),
        auto_clear_after_seconds: Option<f64>,
    ) -> (u64, Stroke) {
        let t = self.elapsed(now_ns);
        let &(lx, ly, _) = self.last();
        if (x - lx).hypot(y - ly) >= MIN_DISTANCE {
            self.points.push((x, y, t));
        } else {
            self.points.last_mut().expect("never empty").2 = t;
        }
        let (w, h) = rect;
        let last_t = self.last().2;
        let stroke = Stroke {
            id: Uuid::new_v4(),
            color: Rgba::RED,
            line_width: LINE_WIDTH,
            // macOS didn't clamp, so a drag past the edge drew into the
            // letterbox bars on export.
            points: self
                .points
                .iter()
                .map(|&(x, y, t)| StrokePoint {
                    x: (x / w).clamp(0.0, 1.0),
                    y: (y / h).clamp(0.0, 1.0),
                    t,
                })
                .collect(),
            auto_clear_after_seconds,
        };
        (self.start_ns + (last_t * 1e9) as u64, stroke)
    }

    /// Seconds since the press. The clock is monotonic, so this never goes
    /// backwards.
    fn elapsed(&self, now_ns: u64) -> f64 {
        now_ns.saturating_sub(self.start_ns) as f64 / 1e9
    }

    fn last(&self) -> &(f64, f64, f64) {
        self.points.last().expect("never empty")
    }
}

/// A finished stroke's commands over a content rect of `w` × `h` logical
/// pixels, for a `Path` with `fit: preserve` (which takes them as raw px).
pub fn path_commands(points: &[StrokePoint], w: f64, h: f64) -> String {
    commands(points.iter().map(|p| (p.x * w, p.y * h)))
}

/// `M` to the first point, `L` to the rest. A single point becomes the
/// degenerate `M x y L x y`, which a round cap rasterizes as a dot; a bare
/// `M x y` draws nothing at all.
fn commands(points: impl Iterator<Item = (f64, f64)>) -> String {
    let points: Vec<(f64, f64)> = points.collect();
    let Some(&(fx, fy)) = points.first() else {
        return String::new();
    };
    let mut out = format!("M {fx} {fy}");
    if points.len() == 1 {
        out.push_str(&format!(" L {fx} {fy}"));
    }
    for &(x, y) in &points[1..] {
        out.push_str(&format!(" L {x} {y}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    /// A 1000 × 500 content rect, and no auto-clear unless a test asks.
    fn release(ip: InProgress, x: f64, y: f64, now_ns: u64) -> (u64, Stroke) {
        ip.release(x, y, now_ns, (1000.0, 500.0), None)
    }

    #[test]
    fn a_move_needs_both_the_time_and_the_distance() {
        let mut ip = InProgress::start(0, 10.0, 10.0);
        assert!(!ip.moved(200.0, 10.0, S / 240)); // far enough, too soon
        assert!(!ip.moved(10.5, 10.0, S)); // long enough, too close
        assert_eq!(ip.points, vec![(10.0, 10.0, 0.0)]);
        assert!(ip.moved(200.0, 10.0, S));
        assert_eq!(ip.points.len(), 2);
    }

    #[test]
    fn a_rejected_move_doesnt_become_the_reference() {
        let mut ip = InProgress::start(0, 10.0, 10.0);
        // Rejected on distance. Were it kept as the reference, the next move
        // would be measured from it and rejected too.
        ip.moved(10.5, 10.0, S);
        ip.moved(11.2, 10.0, 2 * S);
        assert_eq!(ip.points, vec![(10.0, 10.0, 0.0), (11.2, 10.0, 2.0)]);
    }

    #[test]
    fn a_click_is_one_point_stamped_at_the_release() {
        let ip = InProgress::start(7 * S, 250.0, 100.0);
        let (host_ns, stroke) = release(ip, 250.0, 100.0, 9 * S);
        assert_eq!(stroke.points.len(), 1);
        assert_eq!(stroke.points[0].t, 2.0);
        assert_eq!(host_ns, 9 * S);
        // Normalized by the content rect, top-left origin.
        assert_eq!((stroke.points[0].x, stroke.points[0].y), (0.25, 0.2));
    }

    #[test]
    fn a_release_that_fails_the_distance_gate_still_moves_the_last_time() {
        let mut ip = InProgress::start(0, 10.0, 10.0);
        ip.moved(500.0, 250.0, S);
        // Held still for five seconds before lifting: without this the
        // stroke would be stamped five seconds early and clear early on
        // replay (spec D2).
        let (host_ns, stroke) = release(ip, 500.2, 250.0, 6 * S);
        assert_eq!(stroke.points.len(), 2);
        assert_eq!(stroke.points[1].t, 6.0);
        assert_eq!((stroke.points[1].x, stroke.points[1].y), (0.5, 0.5));
        assert_eq!(host_ns, 6 * S);
    }

    #[test]
    fn a_release_that_moved_far_enough_is_appended() {
        let mut ip = InProgress::start(0, 10.0, 10.0);
        ip.moved(500.0, 250.0, S);
        let (host_ns, stroke) = release(ip, 800.0, 250.0, 2 * S);
        assert_eq!(stroke.points.len(), 3);
        assert_eq!(stroke.points[2].t, 2.0);
        assert_eq!(host_ns, 2 * S);
    }

    /// The invariant `visible_strokes` replays against: the event's time
    /// minus the last point's `t` is the press.
    #[test]
    fn the_pen_up_time_back_computes_the_press() {
        let start_ns = 1_234_567_891_011_u64;
        let mut ip = InProgress::start(start_ns, 0.0, 0.0);
        ip.moved(400.0, 300.0, start_ns + S / 2);
        let (host_ns, stroke) = release(ip, 900.0, 300.0, start_ns + 3 * S);
        let last_t = stroke.points.last().unwrap().t;
        assert_eq!(host_ns - (last_t * 1e9) as u64, start_ns);
    }

    #[test]
    fn coordinates_are_clamped_to_the_content_rect() {
        // The pointer is grabbed on press, so a drag off the picture keeps
        // delivering moves; they draw along the edge.
        let mut ip = InProgress::start(0, -40.0, -10.0);
        ip.moved(1400.0, 900.0, S);
        let (_, stroke) = release(ip, 1400.0, 900.0, 2 * S);
        assert_eq!((stroke.points[0].x, stroke.points[0].y), (0.0, 0.0));
        assert_eq!((stroke.points[1].x, stroke.points[1].y), (1.0, 1.0));
    }

    #[test]
    fn a_finished_stroke_is_red_and_carries_the_auto_clear() {
        let ip = InProgress::start(0, 1.0, 1.0);
        let (_, stroke) = ip.release(1.0, 1.0, S, (1000.0, 500.0), Some(5.0));
        assert_eq!(stroke.color, Rgba::RED);
        assert_eq!(stroke.line_width, LINE_WIDTH);
        assert_eq!(stroke.auto_clear_after_seconds, Some(5.0));
    }

    #[test]
    fn commands_scale_normalized_points_back_to_pixels() {
        let points = [
            StrokePoint {
                x: 0.0,
                y: 0.5,
                t: 0.0,
            },
            StrokePoint {
                x: 1.0,
                y: 0.25,
                t: 1.0,
            },
        ];
        assert_eq!(path_commands(&points, 800.0, 400.0), "M 0 200 L 800 100");
    }

    /// A bare `M x y` has no segment and draws nothing; the degenerate one
    /// plus a round cap is the dot (spec D5).
    #[test]
    fn a_single_point_draws_a_dot() {
        let points = [StrokePoint {
            x: 0.5,
            y: 0.5,
            t: 0.0,
        }];
        assert_eq!(path_commands(&points, 800.0, 400.0), "M 400 200 L 400 200");
        assert_eq!(InProgress::start(0, 3.0, 4.0).commands(), "M 3 4 L 3 4");
    }

    #[test]
    fn the_live_commands_are_the_buffered_pixels() {
        let mut ip = InProgress::start(0, 10.0, 20.0);
        ip.moved(30.0, 40.0, S);
        assert_eq!(ip.commands(), "M 10 20 L 30 40");
    }
}
