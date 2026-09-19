//! Turning a set of clips into a description of one output video.
//!
//! Pure data: no media dependency. The export layer consumes this to drive its
//! frame pump.

use std::collections::HashMap;

use uuid::Uuid;

use crate::project::Project;
use crate::timeline::{playback_segments, PlaybackSegment};

/// Which clips an export covers.
///
/// Replaces the macOS sentinel tag string `"__all-clips__"`, which was threaded
/// through the export sheet and compared in five separate places. This is the
/// same behavior with the stringly-typed escape hatch removed, and it collapses
/// two near-duplicate entry points into one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportTarget {
    /// Every clip in the project, in sort order.
    AllClips,
    /// Only clips carrying this tag.
    Tag(String),
}

/// One clip's contribution to the output.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    pub clip_id: Uuid,
    /// Walked play/freeze segments for this clip.
    pub segments: Vec<PlaybackSegment>,
    /// The clip's recording duration, for display. **Not** a timing source —
    /// see `CompilationPlan::total_duration_seconds`.
    pub recording_duration: f64,
}

/// A description of one output video.
#[derive(Debug, Clone, PartialEq)]
pub struct CompilationPlan {
    /// Sum of every segment's `out_duration` across every entry.
    ///
    /// Deliberately **not** the sum of `recording_duration`. Those two agree
    /// only when every event's `record_time` lies inside
    /// `[0, recording_duration]`: an out-of-range event advances the record
    /// cursor past the end, the closing emit then produces nothing, and the
    /// segment sum exceeds the recording duration. Since this value is the
    /// export-progress denominator, the disagreement would surface as progress
    /// climbing past 100%. Defining it from segments removes the class.
    pub total_duration_seconds: f64,
    pub entries: Vec<PlanEntry>,
}

/// Build a plan for `target`.
///
/// `source_durations` is a fallback lookup only — `SourceRef::duration_seconds`
/// is the duration authority, written back by the probe when a source is added
/// or relinked. Two duration sources would let the preview clock and the export
/// clock disagree at end-of-source for the same clip.
///
/// When a clip's source is missing entirely, the fallback is
/// `start_source_seconds + recording_duration`: the smallest value guaranteed
/// to cover any in-range position the clip visits at rate 1, so the segment
/// builder never clamps a forward skip it should not have.
pub fn compilation_plan(
    project: &Project,
    target: &ExportTarget,
    source_durations: &HashMap<usize, f64>,
) -> CompilationPlan {
    let mut clips: Vec<_> = project
        .clips
        .iter()
        .filter(|c| match target {
            ExportTarget::AllClips => true,
            ExportTarget::Tag(tag) => c.tags.iter().any(|t| t == tag),
        })
        .collect();

    // `sort_by_key` is stable, so ties resolve to insertion order. Swift's
    // `sorted(by:)` is not documented stable, making this a free determinism
    // improvement over the original.
    clips.sort_by_key(|c| c.sort_index);

    let mut entries = Vec::with_capacity(clips.len());
    let mut total = 0.0;

    for clip in clips {
        let source_duration = source_durations
            .get(&clip.source_index)
            .copied()
            .or_else(|| project.source_videos.get(clip.source_index).map(|s| s.duration_seconds))
            .unwrap_or(clip.start_source_seconds + clip.recording_duration);

        let segments = playback_segments(clip, source_duration);
        total += segments.iter().map(|s| s.out_duration).sum::<f64>();

        entries.push(PlanEntry {
            clip_id: clip.id,
            segments,
            recording_duration: clip.recording_duration,
        });
    }

    CompilationPlan { total_duration_seconds: total, entries }
}
