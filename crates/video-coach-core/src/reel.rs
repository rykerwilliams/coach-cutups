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

use crate::export::{frame_count, OUTPUT_FPS};
use crate::plan::{CompilationPlan, PlanEntry};
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
///
/// It was 30 s until the coach watched a reel and called the cuts long
/// (2026-09-23). 20 s still reaches the halfway line on this footage; a move
/// that starts earlier is what the goal's own "Reel starts here" is for.
const REEL_LEAD_IN: f64 = 20.0;

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

/// `side`'s spans: one per goal in match order, except that a goal already
/// inside the previous span extends it instead (spec R2).
///
/// A span is `[goal − lead-in, goal + tail]` on the goal's own source, clamped
/// to that source and never starting before the previous entry on the same
/// source ends, so two goals a minute apart don't replay the same footage.
/// Only a goal in the *same* reel clamps or merges: the other side's are not
/// in it at all.
fn reel_spans(project: &Project, side: ReelSide) -> Vec<Span<'_>> {
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
    spans
}

/// `side`'s reel as a plan: one entry per [`reel_spans`] span, and the
/// chapters that go with them.
///
/// **Entries and chapters are built together**, unlike every other target's,
/// because a reel's chapter needs the goal its entry was cut around — the
/// goal's number, the score after it and the period it falls in — and the
/// entry keeps none of that.
///
/// **A period boundary is marked on the chapter that is already there**, as a
/// prefix (`"Second half: Goal 3 — Rovers 2-1"`), not as a chapter of its
/// own. The boundary between the last goal of one period and the first of the
/// next *is* an entry start — a reel's entries run back to back, so there is
/// no gap between them to put a chapter in — and two chapters at one instant
/// would be a zero-length chapter in the MP4 and, in
/// [`crate::chapters::chapter_list`], a goal silently dropped by the
/// ten-second rule. Prefixing loses nothing and invents nothing: the words
/// are [`MatchFormat::period_title`](crate::scoreboard::MatchFormat::period_title)'s,
/// the same ones the whole match's chapters use. It also leaves both limits
/// exactly where they were — a reel still has one chapter per entry, so the
/// `chpl` box's 255 and the ten-second spacing are unchanged by the marking.
///
/// A reel whose goals all fall in one period gets no marker, and neither does
/// its first entry: a boundary needs an entry on each side of it.
pub(crate) fn reel_plan(project: &Project, side: ReelSide) -> CompilationPlan {
    let spans = reel_spans(project, side);
    let scoreboard = ScoreboardContext::for_project(project);
    let total = spans.len();
    let mut entries = Vec::with_capacity(total);
    let mut chapters = Vec::with_capacity(total);
    let mut start_frame = 0;
    let mut previous_period = None;

    for (i, span) in spans.into_iter().enumerate() {
        let goal = span.goal;
        let frames = frame_count(span.end - span.start);
        let at = start_frame as f64 / f64::from(OUTPUT_FPS);
        entries.push(PlanEntry {
            clip_id: None,
            source_index: goal.source_index,
            segments: vec![PlaybackSegment {
                kind: SegmentKind::Play,
                source_start: span.start,
                out_duration: span.end - span.start,
            }],
            start_frame,
            frames,
            text: entry_text(goal, scoreboard.as_ref(), i + 1, total),
        });
        start_frame += frames;

        let period = scoreboard
            .as_ref()
            .and_then(|s| s.period_at(goal.source_index, goal.source_seconds));
        let opens = match (i, previous_period, period) {
            (0, _, _) => None,
            (_, Some(was), Some(now)) if was != now => scoreboard
                .as_ref()
                .map(|s| s.config().format.period_title(now)),
            _ => None,
        };
        previous_period = period;
        let title = chapter_text(goal, scoreboard.as_ref(), i + 1);
        chapters.push((
            at,
            match opens {
                Some(period) => format!("{period}: {title}"),
                None => title,
            },
        ));
    }
    // As for every other target: fewer than two chapters is none at all,
    // since a single chapter only repeats the file.
    if entries.len() < 2 {
        chapters.clear();
    }
    CompilationPlan { entries, chapters }
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

/// `"Goal 3 — Rovers 2-1"`: how a reel's chapters name the same goal.
///
/// **The second wording of one entry, on purpose.** [`entry_text`]'s
/// `"3 / 6 | Rovers goal | 2-1"` is the bar burned across the picture, where
/// the columns and the `n / total` are read at a glance against a film that
/// is playing. A chapter is read on its own, in a list, in someone else's
/// player or pasted into a description, so it reads as a line of prose
/// instead — the same split as the Match panel's
/// [`labelled_events`](crate::scoreboard::labelled_events) and the whole
/// match's [`chapter_events`](crate::scoreboard::chapter_events) (spec W3).
/// The two are meant to differ; changing one leaves the other alone.
///
/// No `total`: a list already shows how many there are. The score is the one
/// after the goal, dropped where the scoreboard has no state (no kick-off
/// tagged by then) rather than claimed as 0-0, and with no scoreboard the
/// team is "Home" or "Away" — [`entry_text`]'s rules, since it is the same
/// fact worded twice.
fn chapter_text(
    goal: &MatchEventRecord,
    scoreboard: Option<&ScoreboardContext>,
    n: usize,
) -> String {
    let home = goal.kind == MatchEventKind::HomeGoal;
    let mut text = format!(
        "Goal {n} — {}",
        team_name(scoreboard.map(|s| s.config()), home)
    );
    if let Some(state) = scoreboard.and_then(|s| s.state_at(goal.source_index, goal.source_seconds))
    {
        text += &format!(" {}-{}", state.home_score, state.away_score);
    }
    text
}
