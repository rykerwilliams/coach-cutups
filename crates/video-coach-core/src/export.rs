//! The edit an export renders: which source frame and zoom land at each output
//! frame.
//!
//! Core owns the edit; media owns the pixels (spec X1). The schedule carries
//! **no play/freeze flag**: the pump answers "last decoded frame with PTS ≤
//! `source_time`", so a repeated `source_time` re-pushes the same buffer. That
//! one rule covers freezes, 25→30 fps duplication and 60→30 fps drops.

use crate::project::Clip;
use crate::timeline::{playback_segments, SegmentKind};
use crate::zoom::{zoom_at, Zoom};

/// Output frame rate. Frame `n` sits at `n / OUTPUT_FPS` seconds.
pub const OUTPUT_FPS: u32 = 30;

/// Slack, in frames, when comparing a frame index against a segment boundary.
///
/// Segment boundaries come from `f64` event times, so `8.3 · 30` is
/// `249.00000000000003`; without the slack that noise would add a frame to the
/// count or push a frame into the segment before the boundary it sits on. The
/// count and the segment lookup use the same slack so they can't disagree.
const FRAME_EPSILON: f64 = 1e-6;

/// One output frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameSpec {
    /// Source-video time to show: the pump pushes the last decoded frame at or
    /// before it.
    pub source_time: f64,
    pub zoom: Zoom,
}

/// Every output frame of `clip`, in order.
///
/// Frame `n` exists at `t = n/30` for every `t` inside the segment total, so the
/// count is `ceil(total·30 − ε)`. A segment gets a frame **if and only if it
/// contains some `n/30`**, so one shorter than a frame interval can still get
/// one.
///
/// Play maps `t` 1:1 onto the source from the segment's start; a freeze holds
/// its anchor, which [`playback_segments`] already caps short of the source
/// end. Zoom comes from [`zoom_at`], independent of segment boundaries.
pub fn frame_schedule(clip: &Clip, source_duration: f64) -> Vec<FrameSpec> {
    let fps = f64::from(OUTPUT_FPS);
    let segments = playback_segments(clip, source_duration);
    let total: f64 = segments.iter().map(|s| s.out_duration).sum();
    let count = (total * fps - FRAME_EPSILON).ceil().max(0.0) as usize;

    let mut frames = Vec::with_capacity(count);
    // Forward walk: output times only increase, so the segment index does too.
    let mut idx = 0;
    let mut out_start = 0.0;
    for n in 0..count {
        let t = n as f64 / fps;
        // Advance past every segment that ends at or before frame `n`. The last
        // segment is never passed, so noise at the very end can't run off it.
        while idx + 1 < segments.len()
            && n as f64 >= (out_start + segments[idx].out_duration) * fps - FRAME_EPSILON
        {
            out_start += segments[idx].out_duration;
            idx += 1;
        }
        let seg = &segments[idx];
        let source_time = match seg.kind {
            // `max(0.0)`: the boundary slack can put `t` a few ns before
            // `out_start`, and the pump rounds to ns, so an unclamped offset
            // would pull the frame before `source_start`.
            SegmentKind::Play => seg.source_start + (t - out_start).max(0.0),
            SegmentKind::Freeze => seg.source_start,
        };
        frames.push(FrameSpec {
            source_time,
            zoom: zoom_at(&clip.events, t),
        });
    }
    frames
}
