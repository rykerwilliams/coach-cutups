//! The goals reel: one output video of every confirmed goal (spec R).
//!
//! A reel is an [`ExportTarget::Reel`](crate::plan::ExportTarget::Reel), never
//! a clip. Each entry is one `Play` segment of game video around a goal, with
//! no clip behind it ([`PlanEntry::clip_id`] is `None`): no drawings, no zoom,
//! no picture-in-picture and no commentary. What makes it a reel rather than
//! a list of clips is the span each goal gets, which lives here.
//!
//! A reel carries a [`ReelSide`]: both sides' goals, or one team's (spec
//! R1b). Only the side is filtered — the spans, the merges, the trims and the
//! captions are the same rules, and the burned-in score is still the match's.

use crate::export::frame_count;
use crate::plan::PlanEntry;
use crate::project::Project;
use crate::scoreboard::{team_name, MatchEventKind, MatchEventRecord, ScoreboardContext};
use crate::timeline::{PlaybackSegment, SegmentKind};

/// How long a goal's entry runs before the goal, unless its
/// [`MatchEventRecord::reel_lead_in`] says otherwise.
///
/// Generous on purpose: it covers the build-up and the assist of any ordinary
/// youth move, and the coach trims it down, which is cheaper than finding
/// footage that was cut off. **Never replace it with a guess that could be
/// shorter**: a cut-off assist is the one failure the reel must not have.
const REEL_LEAD_IN: f64 = 30.0;

/// How long a goal's entry runs after the goal, unless its
/// [`MatchEventRecord::reel_tail`] says otherwise.
const REEL_TAIL: f64 = 6.0;

impl MatchEventRecord {
    /// `(lead-in, tail)`: how long this goal's reel entry runs before and
    /// after it, each its stored trim or the default. A stored trim that
    /// isn't a positive, finite number of seconds (only a file edited by hand
    /// has one) is ignored for the default, one side at a time.
    pub fn reel_span(&self) -> (f64, f64) {
        let or = |trim: Option<f64>, default| {
            trim.filter(|s| s.is_finite() && *s > 0.0)
                .unwrap_or(default)
        };
        (
            or(self.reel_lead_in, REEL_LEAD_IN),
            or(self.reel_tail, REEL_TAIL),
        )
    }
}

/// Whose goals a reel holds (spec R1b).
///
/// A trim belongs to the goal, not to the reel, so it holds in every reel the
/// goal appears in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReelSide {
    /// Both sides' goals, in match order.
    All,
    Home,
    Away,
}

impl ReelSide {
    /// Whether a goal of this kind belongs in this reel.
    fn covers(self, kind: MatchEventKind) -> bool {
        match self {
            ReelSide::All => kind.is_goal(),
            ReelSide::Home => kind == MatchEventKind::HomeGoal,
            ReelSide::Away => kind == MatchEventKind::AwayGoal,
        }
    }
}

/// One entry's span of game video, before it is numbered.
struct Span<'a> {
    /// The entry's first goal, which names it.
    goal: &'a MatchEventRecord,
    start: f64,
    end: f64,
}

/// Every goal `side`'s reel holds, in match order: what "goal n" and "N
/// goals" count. Not the entries, which can be fewer, since a goal inside the
/// previous entry makes none of its own.
pub fn reel_goals(project: &Project, side: ReelSide) -> Vec<&MatchEventRecord> {
    let mut goals: Vec<(f64, &MatchEventRecord)> = project
        .match_events
        .iter()
        .filter(|m| side.covers(m.kind))
        .map(|m| (project.abs_seconds(m.source_index, m.source_seconds), m))
        .collect();
    // Stable, so goals tagged at the same instant keep the order they were
    // tagged in.
    goals.sort_by(|a, b| a.0.total_cmp(&b.0));
    goals.into_iter().map(|(_, goal)| goal).collect()
}

/// `side`'s entries: one per goal in match order, except that a goal already
/// inside the previous entry extends it instead (spec R2).
///
/// A span is `[goal − lead-in, goal + tail]` on the goal's own source, clamped
/// to that source and never starting before the previous entry on the same
/// source ends, so two goals a minute apart don't replay the same footage.
/// Only a goal in the *same* reel clamps or merges: the other side's are not
/// in it at all.
pub(crate) fn reel_entries(project: &Project, side: ReelSide) -> Vec<PlanEntry> {
    let goals = reel_goals(project, side);
    let mut spans: Vec<Span> = Vec::with_capacity(goals.len());
    for goal in goals {
        // `Project::remove_source` refuses a source a goal is on, so this
        // fallback is for a malformed file only: it leaves the tail unclamped.
        let duration = project
            .source_videos
            .get(goal.source_index)
            .map_or(f64::INFINITY, |s| s.duration_seconds);
        let at = goal.source_seconds;
        let (lead_in, tail) = goal.reel_span();
        let end = duration.min(at + tail);

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
        let start = (at - lead_in).max(prev_end);
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
    let team = team_name(scoreboard.map(|s| s.config()), home);
    let mut text = format!("{n} / {total} | {team} goal");
    if let Some(state) = scoreboard.and_then(|s| s.state_at(goal.source_index, goal.source_seconds))
    {
        text += &format!(" | {}-{}", state.home_score, state.away_score);
    }
    text
}
