//! The whole match, end to end (spec W): one output video of every source, in
//! order, with the clock and the score burned in and no commentary.
//!
//! A whole match is an [`ExportTarget::WholeMatch`](crate::plan::ExportTarget),
//! never a clip. Like a reel entry ([`crate::reel`]) each entry has no clip
//! behind it ([`PlanEntry::clip_id`] is `None`): game sound, no drawings, no
//! zoom and no picture-in-picture, with the scoreboard and any highlights
//! drawn from each displayed frame. It carries no caption either — the
//! scoreboard already names the period and the clock, and a text bar across a
//! whole match would be noise.

use crate::export::{frame_count, OUTPUT_FPS};
use crate::plan::PlanEntry;
use crate::project::Project;
use crate::scoreboard::chapter_events;
use crate::timeline::{PlaybackSegment, SegmentKind};

/// One entry per source video, in order, whole.
///
/// A source with no length to play (only a file edited by hand, or a duration
/// that shrank on a relink) makes no entry, since an entry of no frames would
/// be a chapter and a pad with nothing behind them.
pub(crate) fn whole_match_entries(project: &Project) -> Vec<PlanEntry> {
    let mut start_frame = 0;
    project
        .source_videos
        .iter()
        .enumerate()
        .filter(|(_, source)| source.duration_seconds.is_finite() && source.duration_seconds > 0.0)
        .map(|(source_index, source)| {
            let duration = source.duration_seconds;
            let frames = frame_count(duration);
            let entry = PlanEntry {
                clip_id: None,
                source_index,
                segments: vec![PlaybackSegment {
                    kind: SegmentKind::Play,
                    source_start: 0.0,
                    out_duration: duration,
                }],
                start_frame,
                frames,
                text: String::new(),
            };
            start_frame += frames;
            entry
        })
        .collect()
}

/// The match's own moments as chapters (spec W3): every period start and stop
/// and every goal, at its **output** time, worded as a film's chapters
/// ([`chapter_events`]) rather than as the Match panel's rows.
///
/// With nothing tagged, one chapter per source instead, named after the file —
/// and, as for every other target, fewer than two chapters is none at all,
/// since a single chapter only repeats the file.
///
/// `entries` are [`whole_match_entries`]'s, which is what makes the times
/// output times: an event sits at its entry's first frame plus its offset into
/// that entry, never at its absolute project time. Whole sources in order make
/// the two nearly the same, but per-entry quantization moves each later entry
/// by up to a frame.
pub(crate) fn whole_match_chapters(project: &Project, entries: &[PlanEntry]) -> Vec<(f64, String)> {
    let start_of = |entry: &PlanEntry| entry.start_frame as f64 / f64::from(OUTPUT_FPS);
    let at = |source_index: usize, source_seconds: f64| {
        let entry = entries.iter().find(|e| e.source_index == source_index)?;
        let segment = entry.segments.first()?;
        // Clamped to the entry: a tag past a source that shrank on a relink
        // still belongs to that source's chapter, not to the next one's.
        let span = entry.frames as f64 / f64::from(OUTPUT_FPS);
        let offset = (source_seconds - segment.source_start).clamp(0.0, span);
        Some(start_of(entry) + offset)
    };

    let tagged: Vec<(f64, String)> = chapter_events(project)
        .into_iter()
        .filter_map(|e| Some((at(e.event.source_index, e.event.source_seconds)?, e.label)))
        .collect();
    if !tagged.is_empty() {
        return tagged;
    }
    if entries.len() < 2 {
        return Vec::new();
    }
    entries
        .iter()
        .map(|entry| {
            let name = project.source_videos[entry.source_index]
                .display_name
                .clone();
            (start_of(entry), name)
        })
        .collect()
}
