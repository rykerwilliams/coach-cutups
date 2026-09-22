//! The goals reel: one output video of every confirmed goal (spec R).
//!
//! A reel is an [`ExportTarget::Reel`](crate::plan::ExportTarget::Reel), never
//! a clip. Each entry is one `Play` segment of game video around a goal, with
//! no clip behind it ([`PlanEntry::clip_id`] is `None`): no drawings, no zoom,
//! no picture-in-picture and no commentary. What makes it a reel rather than
//! a list of clips is the span each goal gets, which lives here.

use crate::export::frame_count;
use crate::plan::PlanEntry;
use crate::project::Project;
use crate::scoreboard::{MatchEventKind, MatchEventRecord, ScoreboardContext};
use crate::timeline::{PlaybackSegment, SegmentKind};

/// How long a goal's entry runs before the goal, unless its
/// [`MatchEventRecord::reel_lead_in`] says otherwise.
///
/// Generous on purpose: it covers the build-up and the assist of any ordinary
/// youth move, and the coach trims it down, which is cheaper than finding
/// footage that was cut off. **Never replace it with a guess that could be
/// shorter**: a cut-off assist is the one failure the reel must not have.
pub const REEL_LEAD_IN: f64 = 30.0;

/// How long a goal's entry runs after the goal, unless its
/// [`MatchEventRecord::reel_tail`] says otherwise.
pub const REEL_TAIL: f64 = 6.0;

/// One entry's span of game video, before it is numbered.
struct Span<'a> {
    /// The entry's first goal, which names it.
    goal: &'a MatchEventRecord,
    start: f64,
    end: f64,
}

/// The reel's entries: one per goal in match order, except that a goal
/// already inside the previous entry extends it instead (spec R2).
///
/// A span is `[goal − lead-in, goal + tail]` on the goal's own source, clamped
/// to that source and never starting before the previous entry on the same
/// source ends, so two goals a minute apart don't replay the same footage.
pub(crate) fn reel_entries(project: &Project) -> Vec<PlanEntry> {
    // Match order. Stable, so goals tagged at the same instant keep the order
    // they were tagged in.
    let mut goals: Vec<(f64, &MatchEventRecord)> = project
        .match_events
        .iter()
        .filter(|m| m.kind.is_goal())
        .map(|m| (project.abs_seconds(m.source_index, m.source_seconds), m))
        .collect();
    goals.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut spans: Vec<Span> = Vec::with_capacity(goals.len());
    for (_, goal) in goals {
        // `Project::remove_source` refuses a source a goal is on, so this
        // fallback is for a malformed file only: it leaves the tail unclamped.
        let duration = project
            .source_videos
            .get(goal.source_index)
            .map_or(f64::INFINITY, |s| s.duration_seconds);
        let at = goal.source_seconds;
        let end = duration.min(at + goal.reel_tail.unwrap_or(REEL_TAIL));

        let prev_end = match spans.last_mut() {
            Some(prev) if prev.goal.source_index == goal.source_index => {
                if at <= prev.end {
                    prev.end = prev.end.max(end);
                    continue;
                }
                prev.end
            }
            _ => 0.0,
        };
        let start = (at - goal.reel_lead_in.unwrap_or(REEL_LEAD_IN)).max(prev_end);
        // Only a goal past its source's end (a file edited by hand, or a
        // duration that shrank on a relink) has nothing to play.
        if end > start {
            spans.push(Span { goal, start, end });
        }
    }

    let scoreboard = ScoreboardContext::for_project(project);
    let total = spans.len();
    let mut start_frame = 0;
    spans
        .into_iter()
        .enumerate()
        .map(|(i, span)| {
            let frames = frame_count(span.end - span.start);
            let entry = PlanEntry {
                clip_id: None,
                source_index: span.goal.source_index,
                segments: vec![PlaybackSegment {
                    kind: SegmentKind::Play,
                    source_start: span.start,
                    out_duration: span.end - span.start,
                }],
                start_frame,
                frames,
                text: entry_text(span.goal, scoreboard.as_ref(), i + 1, total),
            };
            start_frame += frames;
            entry
        })
        .collect()
}

/// `"<n> / <total> | <team> goal | <home>-<away>"`, the score being the one
/// after `goal`. The score part is dropped where the scoreboard has no state
/// (no kick-off tagged by then), and with no scoreboard the team is "Home" or
/// "Away". A goal the scoreboard doesn't count shows the score unchanged.
fn entry_text(
    goal: &MatchEventRecord,
    scoreboard: Option<&ScoreboardContext>,
    n: usize,
    total: usize,
) -> String {
    let home = goal.kind == MatchEventKind::HomeGoal;
    let team = match scoreboard.map(|s| s.config()) {
        Some(config) if home => config.home.name.as_str(),
        Some(config) => config.away.name.as_str(),
        None if home => "Home",
        None => "Away",
    };
    let mut text = format!("{n} / {total} | {team} goal");
    if let Some(state) = scoreboard.and_then(|s| s.state_at(goal.source_index, goal.source_seconds))
    {
        text += &format!(" | {}-{}", state.home_score, state.away_score);
    }
    text
}
