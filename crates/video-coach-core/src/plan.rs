//! Turning a set of clips into a description of one output video.
//!
//! Pure data: no media dependency. The export layer consumes this to drive its
//! frame pump.

use uuid::Uuid;

use crate::export::{frame_count, OUTPUT_FPS};
use crate::project::{Clip, Project};
use crate::timeline::{playback_segments, PlaybackSegment};

/// Which clips an export covers.
///
/// `Tag` compares the tag verbatim. Tags are normalized by
/// [`crate::tag::normalize_tags`] on the way in (trimmed and lowercased), so a
/// caller passing `"Transition"` selects nothing — normalize first.
///
/// Replaces the macOS sentinel tag string `"__all-clips__"`, which was threaded
/// through the export sheet and compared in five separate places. This is the
/// same behavior with the stringly-typed escape hatch removed, and it collapses
/// two near-duplicate entry points into one function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportTarget {
    /// Every clip in the project, in stored order.
    AllClips,
    /// Only clips carrying this tag.
    Tag(String),
    /// One clip. A single-clip export is a one-entry compilation rather than a
    /// path of its own: one plan, one schedule, one progress model, one cancel.
    Clip(Uuid),
}

/// One entry's contribution to the output: a clip's, or a stretch of game
/// video with no clip behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    /// The clip this entry plays, or `None` for game video alone: no
    /// drawings, no zoom, no picture-in-picture and no commentary.
    pub clip_id: Option<Uuid>,
    /// Index into `Project::source_videos`. Every frame of this entry pulls
    /// from it, so [`crate::export::FrameSpec`] does not repeat it.
    pub source_index: usize,
    /// Walked play/freeze segments for this clip.
    pub segments: Vec<PlaybackSegment>,
    /// This entry's first output frame.
    ///
    /// Entries are quantized to whole output frames: `frames` is the `ceil` of
    /// this entry's segment total and the next entry starts on the next frame
    /// boundary. That keeps "record time is output time" exact *inside* every
    /// entry, at the cost of up to one frame of output per entry.
    pub start_frame: usize,
    /// How many output frames this entry gets.
    pub frames: usize,
    /// The text bar's line: `"<n> / <total> | <name> | tag1, tag2"`, where
    /// `<total>` is the target's clip count. An empty part is dropped along
    /// with its separator, so an unnamed, untagged clip reads `"3 / 7"`.
    pub text: String,
}

impl PlanEntry {
    /// The record time that output frame `frame` shows — the clock for stroke
    /// replay. **Not the scoreboard's clock**, which runs on the source video
    /// and comes from [`crate::export::FrameSpec::source_time`]: a per-clip
    /// constant plus record time is exactly the macOS bug that put the match
    /// clock ahead of the footage after every pause (BACKLOG #27).
    ///
    /// Derived from the entry and the frame index rather than stored on every
    /// [`crate::export::FrameSpec`]; `frame` is a global output frame index
    /// lying inside this entry.
    pub fn record_time(&self, frame: usize) -> f64 {
        debug_assert!(frame >= self.start_frame && frame < self.start_frame + self.frames);
        // In f64 so an out-of-range `frame` is merely wrong, not a wrapped
        // `usize` the size of the address space.
        (frame as f64 - self.start_frame as f64) / f64::from(OUTPUT_FPS)
    }
}

/// A description of one output video.
#[derive(Debug, Clone, PartialEq)]
pub struct CompilationPlan {
    pub entries: Vec<PlanEntry>,
}

impl CompilationPlan {
    /// Output frames in total — **the** denominator, and the only measure of
    /// the output's length. Per-entry quantization rounds each entry up to a
    /// whole frame, so a duration summed from the segments would be short of
    /// the rendered video by up to one frame per entry, and progress against
    /// it would climb past 100%.
    pub fn total_frames(&self) -> usize {
        self.entries.last().map_or(0, |e| e.start_frame + e.frames)
    }
}

/// The clips `target` covers, in stored order (Phase 3 spec C3).
///
/// [`compilation_plan`] builds its entries from this; an entry names its clip
/// by [`PlanEntry::clip_id`], so nothing downstream pairs entries with clips
/// by position.
pub(crate) fn selected_clips<'a>(project: &'a Project, target: &ExportTarget) -> Vec<&'a Clip> {
    project
        .clips
        .iter()
        .filter(|c| match target {
            ExportTarget::AllClips => true,
            ExportTarget::Tag(tag) => c.tags.iter().any(|t| t == tag),
            ExportTarget::Clip(id) => c.id == *id,
        })
        .collect()
}

/// The bar's line for the `n`th of `total` clips, empty parts collapsed.
fn entry_text(clip: &Clip, n: usize, total: usize) -> String {
    [
        format!("{n} / {total}"),
        clip.name.trim().to_string(),
        clip.tags.join(", "),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" | ")
}

/// Build a plan for `target`.
///
/// **`SourceRef::duration_seconds` is the single duration authority.** Phase 2's
/// probe writes it back when a source is added or relinked, so there is nothing
/// to override it with. An earlier draft took a `HashMap` of probed durations
/// that took precedence, which reintroduced exactly the two-duration-sources
/// disagreement the spec's golden rule exists to kill — preview clamping
/// against the persisted value while export clamped against the map.
///
/// When a clip's source is missing entirely, the fallback is
/// `start_source_seconds + recording_duration`: the smallest value guaranteed
/// to cover any in-range position the clip visits at rate 1, so the segment
/// builder never clamps a forward skip it should not have.
pub fn compilation_plan(project: &Project, target: &ExportTarget) -> CompilationPlan {
    let clips = selected_clips(project, target);
    let count = clips.len();

    let mut entries = Vec::with_capacity(count);
    let mut start_frame = 0;

    for (i, clip) in clips.into_iter().enumerate() {
        let source_duration = project
            .source_videos
            .get(clip.source_index)
            .map(|s| s.duration_seconds)
            .unwrap_or(clip.start_source_seconds + clip.recording_duration);

        let segments = playback_segments(clip, source_duration);
        // Quantized per entry, so the next one starts on a frame boundary.
        let frames = frame_count(segments.iter().map(|s| s.out_duration).sum());

        entries.push(PlanEntry {
            clip_id: Some(clip.id),
            source_index: clip.source_index,
            segments,
            start_frame,
            frames,
            text: entry_text(clip, i + 1, count),
        });
        start_frame += frames;
    }

    CompilationPlan { entries }
}
