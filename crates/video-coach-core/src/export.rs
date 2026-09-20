//! The edit an export renders: which source frame and zoom land at each output
//! frame.
//!
//! Core owns the edit; media owns the pixels (spec X1). The schedule carries
//! **no play/freeze flag**: the pump answers "last decoded frame with PTS ≤
//! `source_time`", so a repeated `source_time` re-pushes the same buffer. That
//! one rule covers freezes, 25→30 fps duplication and 60→30 fps drops.
//!
//! [`RateWindow`] lives here too: it measures the same frames the schedule
//! hands out, so a run's progress and its estimate count the same thing.

use std::collections::VecDeque;

use crate::event::CommentaryEvent;
use crate::plan::{compilation_plan, selected_clips, CompilationPlan, ExportTarget};
use crate::project::Project;
use crate::timeline::{PlaybackSegment, SegmentKind};
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

/// How many output frames a span of `seconds` gets.
///
/// Frame `n` exists at `t = n/30` for every `t` inside the span, so the count
/// is `ceil(seconds·30 − ε)`. A segment gets a frame **if and only if it
/// contains some `n/30`**, so one shorter than a frame interval can still get
/// one.
pub(crate) fn frame_count(seconds: f64) -> usize {
    (seconds * f64::from(OUTPUT_FPS) - FRAME_EPSILON)
        .ceil()
        .max(0.0) as usize
}

/// How far back the rate window looks.
const RATE_WINDOW_SECONDS: f64 = 30.0;

/// Samples a rate needs before it is reported at all.
///
/// macOS's gate, kept: it is about **rate stability**, not progress accuracy.
/// The first seconds of an export are the pipeline filling and the encoder
/// settling, and a "4 minutes left" that becomes "40 seconds left" is worse
/// than no estimate.
const RATE_MIN_SAMPLES: usize = 5;
const RATE_MIN_SPAN: f64 = 2.0;

/// The export's rate over a trailing window, in **output frames per wall
/// second**.
///
/// The caller divides its remaining frames by this; frames are what the pump
/// counts, so nothing has to convert through a duration at all.
///
/// macOS's monotonic clamp is deliberately **not** ported: it existed to
/// absorb `AVFoundation`'s `fractionCompleted` overshooting 1.0. A pushed-frame
/// count only rises, and a clamp would hide it if it ever didn't.
#[derive(Debug, Clone, Default)]
pub struct RateWindow {
    /// `(elapsed, frames_done)`, oldest first.
    samples: VecDeque<(f64, usize)>,
}

impl RateWindow {
    /// Record `frames_done` at `elapsed` seconds into the run and return the
    /// rate, or `None` until the window is wide enough to trust.
    pub fn sample(&mut self, frames_done: usize, elapsed: f64) -> Option<f64> {
        self.samples.push_back((elapsed, frames_done));
        let cutoff = elapsed - RATE_WINDOW_SECONDS;
        while self.samples.front().is_some_and(|&(t, _)| t < cutoff) {
            self.samples.pop_front();
        }

        if self.samples.len() < RATE_MIN_SAMPLES {
            return None;
        }
        let (t0, f0) = self.samples[0];
        let (t1, f1) = self.samples[self.samples.len() - 1];
        let span = t1 - t0;
        if span < RATE_MIN_SPAN {
            return None;
        }
        // In f64: a frame count that went backwards is merely a wrong rate, not
        // a `usize` that wrapped to the size of the address space.
        Some((f1 as f64 - f0 as f64) / span)
    }
}

/// One output frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameSpec {
    /// Index into `CompilationPlan::entries` of the clip this frame belongs
    /// to. The source video and the record time are derived from that entry
    /// rather than repeated on every frame.
    pub entry: usize,
    /// Source-video time to show: the pump pushes the last decoded frame at or
    /// before it.
    pub source_time: f64,
    pub zoom: Zoom,
}

/// Every output frame of one export target, with the plan they came from.
///
/// `frames` is flat across entries: frame `n` of the output is `frames[n]`, at
/// `n/30` seconds. Everything else a frame needs — its source video, its
/// record time, its picture-in-picture and its text — hangs off
/// `plan.entries[frames[n].entry]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Compilation {
    pub frames: Vec<FrameSpec>,
    pub plan: CompilationPlan,
}

/// Schedule every output frame of `target`.
///
/// Built on [`compilation_plan`] rather than walking the clips again, so the
/// plan's frame counts and the frames actually produced cannot disagree: each
/// entry emits exactly its `frames`, starting at its `start_frame`.
pub fn compilation_schedule(project: &Project, target: &ExportTarget) -> Compilation {
    let plan = compilation_plan(project, target);
    let clips = selected_clips(project, target);

    let mut frames = Vec::with_capacity(plan.total_frames());
    for (i, (entry, clip)) in plan.entries.iter().zip(clips).enumerate() {
        debug_assert_eq!(entry.clip_id, clip.id, "the plan and the clips diverged");
        debug_assert_eq!(entry.start_frame, frames.len());
        walk(&entry.segments, &clip.events, entry.frames, i, &mut frames);
    }

    Compilation { frames, plan }
}

/// Append `count` frames covering `segments`, tagged with `entry`.
///
/// Play maps `t` 1:1 onto the source from the segment's start; a freeze holds
/// its anchor, which [`playback_segments`] already caps short of the source
/// end. Zoom comes from [`zoom_at`], independent of segment boundaries.
fn walk(
    segments: &[PlaybackSegment],
    events: &[CommentaryEvent],
    count: usize,
    entry: usize,
    out: &mut Vec<FrameSpec>,
) {
    if segments.is_empty() {
        return;
    }
    let fps = f64::from(OUTPUT_FPS);

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
        out.push(FrameSpec {
            entry,
            source_time,
            zoom: zoom_at(events, t),
        });
    }
}
