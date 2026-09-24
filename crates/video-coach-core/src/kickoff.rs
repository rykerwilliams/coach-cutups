//! The kick-off pattern and the coach's confirmation rule (spec D3, D4), as
//! pure functions over the series [`signals`](crate::signals) and
//! [`motion`](crate::motion) produce.
//!
//! The coach's own rule, in one sentence: **a goal is a cheer that is followed
//! by a restart.** So the pattern is found in the picture — the players walk
//! back, the ball sits on the centre spot, the camera holds, and then play
//! resumes — and the sound is what says which of those restarts was a goal.
//!
//! # The stages, and what each one costs
//!
//! 1. [`kickoffs`] reads the picture: a still interval, then motion again for
//!    [`RESUME_SECONDS`]. Its time `K` is a whistle near the end of the hold
//!    when there is one, and the end of the hold when there is not (D3).
//! 2. [`suggest`] reads the sound against those: the **first** kick-off in a
//!    source is the period start, the **last long whistle** is the period end,
//!    and every other kick-off is a goal — **high** tier when a cheer stands
//!    in `[window start, K − 15 s]`, **quiet** when none does.
//! 3. [`near_misses`] is the other half of the same rule: a cheer with no
//!    kick-off behind it is reported and never suggested, because a rule that
//!    suggests every cheer is a rule that suggests the crowd.
//!
//! # What P3 measured, and what it means for these constants
//!
//! **Read every number here as a measurement, not a setting.** On the three
//! tagged matches (2026-09-24, six halves, sixteen goals):
//!
//! - the cheer covers **7 of 16** goals at its initial constants and never more
//!   than 11 anywhere in a sweep, and only then at fifty firings a half;
//! - stillness at an absolute θ finds **1 of 6** tagged kick-offs, and the
//!   quantile threshold this module now reads
//!   ([`STILL_QUANTILE`](crate::motion::STILL_QUANTILE)) is what makes the cue
//!   mean the same thing in two venues 27 dB and a camera apart;
//! - **no post-goal restart has ever been timed** — `kickoffs.txt` is a blank
//!   template in all three folders — so [`GOAL_WINDOW_SECONDS`] rests on the
//!   spec's estimate and V-3 is unmeasured.
//!
//! **The whole rule was then swept over 540 combinations and chosen on the
//! tuning match**: a stillness quantile of 0.50, a hold of 15 s, a cheer 10 dB
//! over its median for 0.5 s, `W = 150 s`, the gate on. The picture half of
//! that point is [`motion`](crate::motion)'s constants; the sound half is
//! **not** — [`signals`](crate::signals)' own were chosen by the cue's own
//! measurement, and the two differ. On the held-out matches that point scores
//! **7 goals of 9 with 29 false ones**, against a chance recall of 0.63,
//! because its windows cover 57% of the match. A rule worth 0.15 over
//! highlighting half the game at random is not a rule to show a coach, and
//! **nothing here is wired to the UI.**
//!
//! The verdict those numbers add up to is
//! `docs/superpowers/spikes/2026-09-24-match-vision-measurements.md`, and it is
//! what to read before trusting any rule in this file.

use std::ops::Range;

use crate::motion::still_intervals_at;
use crate::signals::{Cheer, Whistle};

/// How far back a goal's window reaches from `K` — D4's `W`.
///
/// **Unmeasured.** V-3 sets it from the walk-back durations in `kickoffs.txt`,
/// and no restart has been written down yet, so this is still the spec's
/// estimate of "30–90 s of walking back, generously".
pub const GOAL_WINDOW_SECONDS: f64 = 150.0;

/// How long the picture must move again for a hold to have ended in a restart
/// rather than in a camera twitch (D3).
pub const RESUME_SECONDS: f64 = 3.0;

/// How long before a hold's end a whistle may sound and still be its restart
/// (D3).
pub const WHISTLE_BEFORE_SECONDS: f64 = 5.0;

/// How long after a hold's end a whistle may sound and still be its restart
/// (D3).
pub const WHISTLE_AFTER_SECONDS: f64 = 2.0;

/// How near `K` a cheer stops counting as the goal's and starts being the
/// restart's own (D4).
///
/// Measured on one traced goal, the restart drew its own cheer of +44 dB for
/// 2.6 s about a minute after the goal — so this clamp is what stops a rule
/// reading the applause for the kick-off as evidence of the kick-off.
pub const CHEER_CLAMP_SECONDS: f64 = 15.0;

/// How far before its cheer's onset a high-tier goal's estimated instant sits
/// (D4). Measured, a goal's cheer starts 0.4–1.4 s **after** the frame the
/// coach tags.
pub const AT_LEAD_SECONDS: f64 = 1.0;

/// How near a resolved or dismissed suggestion of the same kind a fresh one
/// has to be for a re-run to drop it (D7).
pub const DEDUP_SECONDS: f64 = 10.0;

/// Whether a kick-off with no cheer behind it is dropped rather than suggested
/// as a quiet-tier goal — the flag D3 leaves to P3, **decided by P3's numbers**
/// and not by argument.
///
/// It is `true`, **measured**. At the chosen constants, turning it off on the
/// tuning match adds 18 quiet-tier rows carrying 2 goals — a precision of
/// **0.11** against the 40% bar — and takes the share of the match the
/// suggestions cover from 50% to **80%**, which is most of a half of football
/// highlighted as "a goal may be in here". A tier the coach would dismiss every
/// row of costs more than it finds, so it never reaches P4.
pub const CHEER_GATES_CANDIDATES: bool = true;

/// What anchored a kick-off's time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// A whistle near the end of the hold — the restart itself.
    Whistle,
    /// The end of the hold, when no whistle was audible.
    StillEnd,
}

/// One candidate restart: a hold in the picture that ended in play.
#[derive(Debug, Clone, PartialEq)]
pub struct KickOff {
    /// `K`: the whistle when there was one, the end of the hold otherwise.
    pub seconds: f64,
    /// The hold it followed, which is the evidence a miss is traced to.
    pub still: Range<f64>,
    /// Which of the two `seconds` is.
    pub anchor: Anchor,
}

/// How much evidence a goal suggestion has (D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalTier {
    /// A cheer in `[window start, K − 15 s]`.
    High,
    /// A kick-off with no cheer behind it.
    Quiet,
}

impl std::fmt::Display for GoalTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GoalTier::High => "high",
            GoalTier::Quiet => "quiet",
        })
    }
}

/// What a suggestion claims to have found (D6's `SuggestionKind`).
///
/// P3 does not store these: the harness positions one on a source and scores
/// it, and P4 is what puts the shape in `project.json`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SuggestionKind {
    Goal {
        tier: GoalTier,
        /// `[start, end]` in source seconds: where the goal must be (D4).
        window: (f64, f64),
        /// The estimated instant, high tier only.
        at: Option<f64>,
    },
    PeriodStart,
    PeriodEnd,
}

impl SuggestionKind {
    /// Whether two suggestions are the same **kind** for D7's de-duplication —
    /// a goal is a goal whatever its tier, window or estimate.
    pub fn same_kind(&self, other: &SuggestionKind) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// One suggestion on one source, in that source's own seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Suggestion {
    /// `K` for a goal or a period start; the whistle for a period end.
    pub seconds: f64,
    pub kind: SuggestionKind,
}

/// The two constants a run is free to choose, so a sweep is a value rather than
/// a rebuild (spec G2).
///
/// Everything else in this module is a constant because nothing measured has
/// ever moved it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    /// D4's `W`.
    pub window_seconds: f64,
    /// [`CHEER_GATES_CANDIDATES`] for this run.
    pub cheer_gates: bool,
    /// What counts as a long whistle, and so as a period's end. A parameter
    /// because the measured floor is the one constant the footage flatly
    /// refuted: no half holds a whistle longer than 0.78 s, and
    /// [`WHISTLE_LONG_SECONDS`](crate::signals::WHISTLE_LONG_SECONDS) is 0.8.
    pub long_whistle_seconds: f64,
}

impl Default for Rule {
    fn default() -> Rule {
        Rule {
            window_seconds: GOAL_WINDOW_SECONDS,
            cheer_gates: CHEER_GATES_CANDIDATES,
            long_whistle_seconds: crate::signals::WHISTLE_LONG_SECONDS,
        }
    }
}

/// Every kick-off in one source's picture: a hold of at least
/// `still_min_seconds` under `theta`, then [`RESUME_SECONDS`] of movement.
///
/// `theta` comes from [`still_theta`](crate::motion::still_theta) on this same
/// series — the threshold is a property of the half, not of the codebase.
///
/// **A hold the file ends in is not a kick-off.** Play has to resume for there
/// to have been a restart, so a half that fades out still yields nothing at its
/// tail, which is what keeps the last seconds of every file from being a
/// candidate.
pub fn kickoffs(
    motion: &[f32],
    hz: f64,
    theta: f32,
    still_min_seconds: f64,
    whistles: &[Whistle],
) -> Vec<KickOff> {
    still_intervals_at(motion, hz, theta, still_min_seconds)
        .into_iter()
        .filter(|still| resumes(motion, hz, still.end, theta))
        .map(|still| match whistle_near(whistles, still.end) {
            Some(whistle) => KickOff {
                seconds: whistle,
                still,
                anchor: Anchor::Whistle,
            },
            None => KickOff {
                seconds: still.end,
                still,
                anchor: Anchor::StillEnd,
            },
        })
        .collect()
}

/// Whether the picture moved for [`RESUME_SECONDS`] from `seconds`.
///
/// **Most of it, not every frame of it** — the median of the run has to stand
/// over θ. Measured over six halves, the literal reading of D3 ("motion above
/// θ for at least 3 s") is not a rule at all: at a threshold near the fifth of
/// the half that holds still, a lone frame dips back under θ in nearly every
/// restart, and requiring all fifteen frames left **0 candidates in 6 halves**
/// at every threshold worth trying. The median asks the same question — has
/// play started again? — without asking the picture to be noiseless.
fn resumes(motion: &[f32], hz: f64, seconds: f64, theta: f32) -> bool {
    let from = (seconds * hz).round() as usize;
    let need = (RESUME_SECONDS * hz).round() as usize;
    motion
        .get(from..from + need)
        .is_some_and(|run| crate::motion::at_quantile(run, 0.5) > theta)
}

/// The whistle nearest `seconds` inside `[−5 s, +2 s]` of it (D3), as a time.
fn whistle_near(whistles: &[Whistle], seconds: f64) -> Option<f64> {
    whistles
        .iter()
        .map(|w| w.start)
        .filter(|&start| {
            start >= seconds - WHISTLE_BEFORE_SECONDS && start <= seconds + WHISTLE_AFTER_SECONDS
        })
        .min_by(|a, b| (a - seconds).abs().total_cmp(&(b - seconds).abs()))
}

/// The coach's confirmation rule over one source (D4).
///
/// `kickoffs` and `cheers` are in time order, as the functions that build them
/// return them.
///
/// **A gated kick-off does not clamp the next window.** `K_prev` is the
/// previous kick-off *kept*, so dropping a candidate cannot shorten the window
/// of the goal after it — which would make gating cost recall twice.
pub fn suggest(
    kickoffs: &[KickOff],
    cheers: &[Cheer],
    whistles: &[Whistle],
    rule: Rule,
) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let mut previous = 0.0f64;
    for (i, k) in kickoffs.iter().enumerate() {
        if i == 0 {
            out.push(Suggestion {
                seconds: k.seconds,
                kind: SuggestionKind::PeriodStart,
            });
            previous = k.seconds;
            continue;
        }
        let window = ((k.seconds - rule.window_seconds).max(previous), k.seconds);
        match last_cheer_in(cheers, window.0, k.seconds - CHEER_CLAMP_SECONDS) {
            Some(onset) => {
                out.push(Suggestion {
                    seconds: k.seconds,
                    kind: SuggestionKind::Goal {
                        tier: GoalTier::High,
                        window,
                        at: Some(onset - AT_LEAD_SECONDS),
                    },
                });
                previous = k.seconds;
            }
            // Quiet: a restart the crowd said nothing about. Whether that is a
            // suggestion or a silence is [`CHEER_GATES_CANDIDATES`], and P3
            // measured it.
            None if rule.cheer_gates => {}
            None => {
                out.push(Suggestion {
                    seconds: k.seconds,
                    kind: SuggestionKind::Goal {
                        tier: GoalTier::Quiet,
                        window,
                        at: None,
                    },
                });
                previous = k.seconds;
            }
        }
    }
    // The period ends on the last long whistle (D4) — measured, on this
    // footage there is rarely one at all, which is a fact about the rule and
    // not about this line.
    if let Some(end) = whistles
        .iter()
        .rev()
        .find(|w| w.is_longer_than(rule.long_whistle_seconds))
    {
        out.push(Suggestion {
            seconds: end.start,
            kind: SuggestionKind::PeriodEnd,
        });
    }
    out
}

/// The onset of the last cheer in `[from, to]`, which is the one D4 takes `at`
/// from: earlier cheers in the same window were near misses.
fn last_cheer_in(cheers: &[Cheer], from: f64, to: f64) -> Option<f64> {
    cheers
        .iter()
        .rev()
        .map(|c| c.onset)
        .find(|&onset| onset >= from && onset <= to)
}

/// Every cheer no kick-off explains (D4): the crowd made a noise and play never
/// restarted behind it.
///
/// Reported, never suggested. These are the rule's own account of what it threw
/// away, and on this footage they are most of what the crowd does.
pub fn near_misses(kickoffs: &[KickOff], cheers: &[Cheer], rule: Rule) -> Vec<f64> {
    cheers
        .iter()
        .map(|c| c.onset)
        .filter(|&onset| {
            !kickoffs.iter().any(|k| {
                onset >= k.seconds - rule.window_seconds && onset <= k.seconds - CHEER_CLAMP_SECONDS
            })
        })
        .collect()
}

/// `fresh` without the suggestions a re-run should not bring back (D7): one
/// within [`DEDUP_SECONDS`] of a `kept` suggestion of the same kind, which is
/// how a resolved or dismissed row stays dealt with.
pub fn dedup(fresh: Vec<Suggestion>, kept: &[Suggestion]) -> Vec<Suggestion> {
    fresh
        .into_iter()
        .filter(|new| {
            !kept.iter().any(|old| {
                old.kind.same_kind(&new.kind) && (old.seconds - new.seconds).abs() <= DEDUP_SECONDS
            })
        })
        .collect()
}
