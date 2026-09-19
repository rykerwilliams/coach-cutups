//! Which strokes are on screen at a given record time, and how much of each.
//!
//! **A `.stroke` event's `record_time` is when the stroke FINISHED**, not when
//! it started. The recorder appends it on pen-up, and each point carries `t`
//! measured from the stroke's own start, so replay back-computes the start as
//! `record_time - points.last().t`. Everything keys off that back-computed
//! value, never off the event's timestamp.
//!
//! Getting this wrong makes every stroke appear late by its own duration —
//! proportional to how long the coach held the pen, so long strokes are
//! visibly wrong while quick flicks look fine. That asymmetry makes it a
//! genuinely nasty bug to catch by eye.

use crate::event::EventKind;
use crate::project::Clip;
use crate::stroke::Stroke;

/// A stroke that is on screen, and how far through drawing it is.
#[derive(Debug, Clone, PartialEq)]
pub struct VisibleStroke<'a> {
    pub stroke: &'a Stroke,
    /// Back-computed record time at which this stroke began.
    pub first_point_record_time: f64,
    /// How many of `stroke.points` have been drawn by now.
    ///
    /// Zero-length strokes are possible; the compositor renders a
    /// single-point stroke as a **filled circle** of diameter `line_width`,
    /// because a zero-length path does not rasterize with a round cap.
    pub drawn_point_count: usize,
}

/// Strokes visible at `at_record_time`, in event order.
///
/// Borrows out of the clip rather than cloning: this runs once per output
/// frame on the export path, and the point vectors are the bulk of the data.
pub fn visible_strokes(clip: &Clip, at_record_time: f64) -> Vec<VisibleStroke<'_>> {
    crate::event::debug_assert_sorted(&clip.events);

    // FIRST PASS. Collecting clear-all times up front is not an optimization —
    // it is required for correctness. A single forward pass that clears as it
    // goes would already have pushed a stroke into the output before reaching
    // the later `ClearAll` that should have removed it. A correct algorithm has
    // to know about every clear-all up to `t` before deciding any stroke's
    // visibility.
    // A stroke is cleared iff SOME clear-all landed after it began, i.e. iff the
    // latest one at or before `t` is later than the stroke's start. That is a
    // fold to one value, not a collection — this runs once per output frame.
    let last_clear_all = clip
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ClearAll) && e.record_time <= at_record_time)
        .map(|e| e.record_time)
        .fold(f64::NEG_INFINITY, f64::max);

    let mut out = Vec::new();
    for ev in &clip.events {
        let EventKind::Stroke(stroke) = &ev.kind else {
            continue;
        };

        let first_t = ev.record_time - stroke.points.last().map_or(0.0, |p| p.t);

        // Not started yet.
        if at_record_time < first_t {
            continue;
        }
        // Auto-clear is an INCLUSIVE cutoff.
        if let Some(auto) = stroke.auto_clear_after_seconds {
            if at_record_time >= first_t + auto {
                continue;
            }
        }
        // A clear-all cancels this stroke only if it landed STRICTLY after the
        // stroke began — one arriving at the same instant does not erase it.
        if last_clear_all > first_t {
            continue;
        }

        let elapsed = at_record_time - first_t;
        // STRICTLY greater: a point whose `t` exactly equals `elapsed` is drawn.
        let drawn = stroke
            .points
            .iter()
            .position(|p| p.t > elapsed)
            .unwrap_or(stroke.points.len());

        out.push(VisibleStroke {
            stroke,
            first_point_record_time: first_t,
            drawn_point_count: drawn,
        });
    }
    out
}
