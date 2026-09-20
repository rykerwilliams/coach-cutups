//! The scoreboard: what the project stores, and the clock and score it shows.
//!
//! The coach tags kick-off, half-time, full-time and each goal while scanning;
//! everything else here is derived. [`scoreboard_state`] is a pure function of
//! **absolute time** on the virtual-concat timeline ([`Project::abs_seconds`]),
//! so the same call answers for a scan position, a preview frame and an export
//! frame. Nothing caches a per-clip constant: macOS did, and every pause in a
//! commentary recording then pushed the clock ahead of the footage
//! (BACKLOG #27).
//!
//! **The match clock is the *displayed* frame's source time**, i.e.
//! [`crate::export::FrameSpec::source_time`], not [`crate::timeline::source_time`]
//! — see `timeline.rs` for why the two differ by up to 50 ms at a freeze near
//! the end of a source.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::project::Project;
use crate::stroke::Rgba;

// ------------------------------------------------------------- stored types

/// One team's identity and colors.
///
/// `font_color` is required. The Swift original defaults it to
/// `secondary_color` in its *initializer*, which serde cannot express; on a
/// clean-slate format the constructor supplies that default instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamConfig {
    pub name: String,
    pub primary_color: Rgba,
    pub secondary_color: Rgba,
    pub font_color: Rgba,
}

impl TeamConfig {
    /// Mirrors the Swift initializer: `font_color` defaults to `secondary`.
    pub fn new(name: impl Into<String>, primary: Rgba, secondary: Rgba) -> Self {
        TeamConfig {
            name: name.into(),
            primary_color: primary,
            secondary_color: secondary,
            font_color: secondary,
        }
    }
}

/// How long the match is, in periods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchFormat {
    pub regulation_periods: u32,
    pub regulation_period_seconds: u32,
    pub overtime_periods: u32,
    pub overtime_period_seconds: u32,
}

impl Default for MatchFormat {
    /// Soccer: two 45-minute halves, no overtime.
    fn default() -> Self {
        MatchFormat {
            regulation_periods: 2,
            regulation_period_seconds: 45 * 60,
            overtime_periods: 0,
            overtime_period_seconds: 15 * 60,
        }
    }
}

impl MatchFormat {
    pub fn total_periods(&self) -> u32 {
        self.regulation_periods + self.overtime_periods
    }

    /// How many start/stops a fully-tagged match has: one pair per period.
    /// [`interpret`] never assigns more roles than this, so no period index it
    /// produces is outside the format.
    pub fn expected_start_stop_events(&self) -> usize {
        2 * self.total_periods() as usize
    }

    /// True when `period` falls in the overtime range.
    pub fn is_overtime(&self, period: u32) -> bool {
        period >= self.regulation_periods
    }

    /// Length of one period, regulation or overtime.
    pub fn period_seconds(&self, period: u32) -> f64 {
        f64::from(if self.is_overtime(period) {
            self.overtime_period_seconds
        } else {
            self.regulation_period_seconds
        })
    }

    /// User-facing name for a period: `"1H"`/`"2H"` for soccer's two halves,
    /// `"P1"`… otherwise, and `"OT1"`… in overtime.
    pub fn period_name(&self, period: u32) -> String {
        if self.is_overtime(period) {
            return format!("OT{}", period - self.regulation_periods + 1);
        }
        if self.regulation_periods == 2 {
            return if period == 0 { "1H" } else { "2H" }.to_string();
        }
        format!("P{}", period + 1)
    }

    /// What the clock reads during the break after `period`: soccer's half
    /// time, or a generic break between any other pair of periods.
    pub fn break_label(&self, after_period: u32) -> &'static str {
        if self.regulation_periods == 2 && after_period == 0 {
            "HT"
        } else {
            "BREAK"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreboardConfig {
    pub home: TeamConfig,
    pub away: TeamConfig,
    #[serde(default)]
    pub format: MatchFormat,
    /// "My footage starts after kick-off": [`interpret`] then derives a
    /// period-1 start rather than waiting for one to be tagged. Setup, not a
    /// tagged event — see [`interpret`].
    #[serde(default)]
    pub auto_back_anchor_p1: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchEventKind {
    StartStop,
    HomeGoal,
    AwayGoal,
}

/// A tagged match event, positioned on one source video.
///
/// `source_index` + `source_seconds` project onto the virtual-concat timeline
/// via [`Project::abs_seconds`]; the match clock runs on that timeline, not on
/// any single source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchEventRecord {
    pub id: Uuid,
    pub kind: MatchEventKind,
    pub source_index: usize,
    pub source_seconds: f64,
}

// ---------------------------------------------------------- interpretation

/// A match event on the virtual-concat timeline.
///
/// Derived per job and **never cached across a source add, move, remove or
/// relink**: a relink can change a source's duration, which moves every later
/// source's offset and so every later event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbsoluteMatchEvent {
    /// The record this came from, or `None` for [`interpret`]'s derived
    /// back-anchor, which has none.
    pub id: Option<Uuid>,
    pub kind: MatchEventKind,
    pub abs_seconds: f64,
}

/// What a start/stop turned out to be, by its position in the match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeriodRole {
    Start(u32),
    End(u32),
}

/// One start/stop with the role its position gave it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InterpretedEvent {
    /// The record this came from, or `None` for the derived back-anchor. The
    /// Match panel keys its rows off this rather than indexing a second,
    /// separately-filtered list the way macOS did.
    pub id: Option<Uuid>,
    pub abs_seconds: f64,
    pub role: PeriodRole,
}

/// Assign period roles to the tagged start/stops.
///
/// Positional: sorted by absolute time, the first starts period 0, the second
/// ends it, the third starts period 1, and so on. Nothing is tagged as "the
/// half-time whistle" — quarters, overtime and a single-period format all fall
/// out of the same walk. Goals in `events` are ignored.
///
/// The sort is **stable**, so two events sharing an absolute time keep the
/// order they were tagged in.
///
/// **The back-anchor is derived, never stored.** When
/// [`ScoreboardConfig::auto_back_anchor_p1`] is set the coach's footage starts
/// after kick-off, so a period-1 start is prepended at `period_seconds(0)`
/// before the first tagged start/stop — which is then the *end* of period 1, so
/// that end lands at exactly one period length and the clock reads 45:00 there.
/// Before any start/stop is tagged the derived start sits at absolute 0, so the
/// clock runs from the beginning of the footage through the half the coach most
/// wants one, and snaps to the true alignment when half-time is tagged. macOS
/// instead stored a flagged `(0, 0)` event and added an offset to the
/// *displayed* number, which left the clock reading 50:00 while still counted as
/// running, so stoppage never began.
///
/// **At most `expected_start_stop_events` roles are assigned**, the derived
/// start included. Start/stops past that get no role: a period index outside
/// the format would name a period that does not exist and would keep the clock
/// running past full time. The coach never loses a *stored* event to the
/// anchor — the cap the UI enforces is on the records, which the anchor is not
/// part of — so turning the anchor back off restores every role.
pub fn interpret(
    events: &[AbsoluteMatchEvent],
    config: &ScoreboardConfig,
) -> Vec<InterpretedEvent> {
    let mut sorted: Vec<(Option<Uuid>, f64)> = events
        .iter()
        .filter(|e| e.kind == MatchEventKind::StartStop)
        .map(|e| (e.id, e.abs_seconds))
        .collect();
    sorted.sort_by(|a, b| a.1.total_cmp(&b.1));

    if config.auto_back_anchor_p1 {
        let p1_start = sorted
            .first()
            .map_or(0.0, |&(_, p1_end)| p1_end - config.format.period_seconds(0));
        sorted.insert(0, (None, p1_start));
    }
    sorted.truncate(config.format.expected_start_stop_events());

    sorted
        .into_iter()
        .enumerate()
        .map(|(i, (id, abs_seconds))| {
            let period = (i / 2) as u32;
            InterpretedEvent {
                id,
                abs_seconds,
                role: if i % 2 == 0 {
                    PeriodRole::Start(period)
                } else {
                    PeriodRole::End(period)
                },
            }
        })
        .collect()
}

// ------------------------------------------------------------------- clock

/// What the clock cell reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClockDisplay {
    /// Match time, counting the periods before this one.
    Running {
        seconds: f64,
    },
    /// Past the period's length: `base` is where the clock stops, `plus` how
    /// far past it we are.
    Stoppage {
        base: f64,
        plus: f64,
    },
    /// Between two periods.
    OnBreak(&'static str),
    Fulltime,
}

/// The clock cell's text, and the `+M:SS` tail drawn beside it.
///
/// `trailing` is empty unless the clock is in stoppage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockLabels {
    pub main: String,
    pub trailing: String,
}

pub fn format_clock(clock: ClockDisplay) -> ClockLabels {
    // `as` truncates toward zero and saturates, and `f64::max` returns the
    // non-NaN operand, so nothing here can panic or read as a negative time.
    fn mmss(seconds: f64) -> String {
        let total = seconds.max(0.0) as u64;
        format!("{:02}:{:02}", total / 60, total % 60)
    }
    let plain = |main: String| ClockLabels {
        main,
        trailing: String::new(),
    };
    match clock {
        ClockDisplay::Running { seconds } => plain(mmss(seconds)),
        ClockDisplay::Stoppage { base, plus } => {
            let plus = plus.max(0.0) as u64;
            ClockLabels {
                main: mmss(base),
                trailing: format!("+{}:{:02}", plus / 60, plus % 60),
            }
        }
        ClockDisplay::OnBreak(label) => plain(label.to_string()),
        ClockDisplay::Fulltime => plain("FT".to_string()),
    }
}

// ------------------------------------------------------------------- state

/// What the scoreboard shows at one instant.
///
/// It does not carry the team configs: every caller already holds the
/// [`ScoreboardConfig`] it passed in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScoreboardState {
    pub home_score: u32,
    pub away_score: u32,
    pub clock: ClockDisplay,
}

/// The score and clock at `now_abs` seconds on the virtual-concat timeline.
///
/// `None` when nothing has been tagged yet, or when the first start is still
/// ahead of `now_abs` — there is no match to show a clock for. Whether a
/// scoreboard is configured at all is `Option<ScoreboardConfig>` at the caller,
/// and an empty team name is refused by the command that sets it, so the render
/// path has this one guard.
///
/// Cheap enough to call per frame: the work is a sort of the handful of tagged
/// start/stops.
pub fn scoreboard_state(
    now_abs: f64,
    config: &ScoreboardConfig,
    events: &[AbsoluteMatchEvent],
) -> Option<ScoreboardState> {
    let format = &config.format;
    let interp = interpret(events, config);

    // The current period is the last start at or before `now_abs`. `interp` is
    // sorted, so scanning back finds it; no eligible start means the match has
    // not begun, which subsumes the "nothing tagged" guard.
    let (period, period_start) = interp.iter().rev().find_map(|e| match e.role {
        PeriodRole::Start(p) if e.abs_seconds <= now_abs => Some((p, e.abs_seconds)),
        _ => None,
    })?;

    let period_end = interp
        .iter()
        .find(|e| e.role == PeriodRole::End(period))
        .map(|e| e.abs_seconds);

    let clock = match period_end {
        // Past this period's end: the break, or full time on the last period.
        Some(end) if now_abs >= end => {
            if period + 1 == format.total_periods() {
                ClockDisplay::Fulltime
            } else {
                ClockDisplay::OnBreak(format.break_label(period))
            }
        }
        // Stoppage is derived, not tagged: it is simply time past the period's
        // length. A back-anchored period 1 never reaches it, because its
        // derived start puts the tagged end at exactly one period length.
        _ => {
            let elapsed = now_abs - period_start;
            let length = format.period_seconds(period);
            let prior: f64 = (0..period).map(|p| format.period_seconds(p)).sum();
            if elapsed <= length {
                ClockDisplay::Running {
                    seconds: prior + elapsed,
                }
            } else {
                ClockDisplay::Stoppage {
                    base: prior + length,
                    plus: elapsed - length,
                }
            }
        }
    };

    // Goals count inside `[first start, final whistle]`. The match has a final
    // whistle only once the tagged start/stops fill the format; until then the
    // bound is open, so a late goal in a part-tagged match still counts.
    // (`total_periods` is at least 1 below: a zero-period format interprets
    // nothing, and no eligible start returned above.)
    let first_start = interp[0].abs_seconds;
    let last_end = match interp.last() {
        Some(last) if last.role == PeriodRole::End(format.total_periods() - 1) => last.abs_seconds,
        _ => f64::INFINITY,
    };
    let score = |kind| {
        events
            .iter()
            .filter(|e| {
                e.kind == kind
                    && e.abs_seconds <= now_abs
                    && (first_start..=last_end).contains(&e.abs_seconds)
            })
            .count() as u32
    };

    Some(ScoreboardState {
        home_score: score(MatchEventKind::HomeGoal),
        away_score: score(MatchEventKind::AwayGoal),
        clock,
    })
}

/// Everything the scoreboard needs for one preview or export run.
///
/// Built once by the bus and carried on the job, because a driver has a
/// `(source_index, source_time)` per frame and no project to project it
/// against. **Never cache one across a source add, move, remove or relink**:
/// all four change the offsets it froze.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreboardContext {
    config: ScoreboardConfig,
    events: Vec<AbsoluteMatchEvent>,
    /// Where each source starts on the concat timeline, with the project's
    /// total duration appended — [`Project::cumulative_offset`] frozen, so an
    /// index past the end clamps the same way rather than panicking.
    source_offsets: Vec<f64>,
}

impl ScoreboardContext {
    /// `None` when the project has no scoreboard configured.
    pub fn for_project(project: &Project) -> Option<Self> {
        let config = project.scoreboard.clone()?;
        let mut source_offsets = Vec::with_capacity(project.source_videos.len() + 1);
        let mut offset = 0.0;
        source_offsets.push(offset);
        for source in &project.source_videos {
            offset += source.duration_seconds;
            source_offsets.push(offset);
        }
        let events = project
            .match_events
            .iter()
            .map(|m| AbsoluteMatchEvent {
                id: Some(m.id),
                kind: m.kind,
                abs_seconds: project.abs_seconds(m.source_index, m.source_seconds),
            })
            .collect();
        Some(ScoreboardContext {
            config,
            events,
            source_offsets,
        })
    }

    /// The teams and format to draw with.
    pub fn config(&self) -> &ScoreboardConfig {
        &self.config
    }

    /// The scoreboard for the frame showing `source_time` of source
    /// `source_index`.
    ///
    /// A clip cannot span a source boundary — it has one `source_index` and
    /// every timeline mutation clamps within it — so one index per frame is the
    /// whole story.
    pub fn state_at(&self, source_index: usize, source_time: f64) -> Option<ScoreboardState> {
        let last = self.source_offsets.len() - 1;
        let now_abs = self.source_offsets[source_index.min(last)] + source_time;
        scoreboard_state(now_abs, &self.config, &self.events)
    }
}

// --------------------------------------------------------------- mutations

impl Project {
    /// Tag a match event at `(source_index, source_seconds)` and return its id.
    ///
    /// **No cap here.** A start/stop past the format's
    /// [`MatchFormat::expected_start_stop_events`] gets no role, but the UI
    /// disables the action there and the command refuses out loud; macOS's
    /// mutator silently did nothing instead, which is worse than a refusal.
    pub fn append_match_event(
        &mut self,
        kind: MatchEventKind,
        source_index: usize,
        source_seconds: f64,
    ) -> Uuid {
        let id = Uuid::new_v4();
        self.match_events.push(MatchEventRecord {
            id,
            kind,
            source_index,
            source_seconds,
        });
        id
    }

    /// Remove the event with `id` and return it, or `None` if there is none.
    pub fn delete_match_event(&mut self, id: Uuid) -> Option<MatchEventRecord> {
        let i = self.match_events.iter().position(|m| m.id == id)?;
        Some(self.match_events.remove(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ScoreboardConfig {
        ScoreboardConfig {
            home: TeamConfig::new(
                "Rovers",
                Rgba {
                    r: 0.1,
                    g: 0.2,
                    b: 0.8,
                    a: 1.0,
                },
                Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
            ),
            away: TeamConfig::new(
                "United",
                Rgba {
                    r: 0.8,
                    g: 0.1,
                    b: 0.1,
                    a: 1.0,
                },
                Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            ),
            format: MatchFormat::default(),
            auto_back_anchor_p1: false,
        }
    }

    #[test]
    fn scoreboard_config_round_trips() {
        let c = sample();
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<ScoreboardConfig>(&s).unwrap(), c);
    }

    /// `font_color` defaults to `secondary_color`, matching the Swift
    /// initializer. Serde cannot express "default to another field", so the
    /// field is required on disk and this constructor supplies the default.
    #[test]
    fn team_font_color_defaults_to_secondary() {
        let white = Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        };
        let t = TeamConfig::new("X", Rgba::RED, white);
        assert_eq!(t.font_color, white);
    }

    /// These strings are the on-disk format. A rename would silently make every
    /// existing project unreadable, and nothing else pins them.
    #[test]
    fn match_event_kinds_have_the_expected_wire_spellings() {
        for (kind, spelling) in [
            (MatchEventKind::StartStop, r#""startStop""#),
            (MatchEventKind::HomeGoal, r#""homeGoal""#),
            (MatchEventKind::AwayGoal, r#""awayGoal""#),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), spelling);
        }
    }

    #[test]
    fn match_event_record_round_trips() {
        let r = MatchEventRecord {
            id: uuid::Uuid::nil(),
            kind: MatchEventKind::HomeGoal,
            source_index: 1,
            source_seconds: 123.5,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""sourceSeconds":123.5"#), "got {s}");
        assert_eq!(serde_json::from_str::<MatchEventRecord>(&s).unwrap(), r);
    }

    /// Phase 9 dropped `isAutoBackAnchor` — the back-anchor is derived from the
    /// config now. A record written by a build that had it still loads, which
    /// is why the format stays at v7.
    #[test]
    fn a_record_carrying_the_old_anchor_flag_still_loads() {
        let with_flag = r#"{"id":"00000000-0000-0000-0000-000000000000","kind":"startStop","sourceIndex":0,"sourceSeconds":1.0,"isAutoBackAnchor":true}"#;
        let r: MatchEventRecord = serde_json::from_str(with_flag).unwrap();
        assert_eq!(r.kind, MatchEventKind::StartStop);
    }

    /// Soccer: two 45-minute halves, no overtime.
    #[test]
    fn match_format_defaults_to_soccer() {
        let f = MatchFormat::default();
        assert_eq!(
            (f.regulation_periods, f.regulation_period_seconds),
            (2, 2700)
        );
        assert_eq!(f.overtime_periods, 0);
    }

    /// `format` and `auto_back_anchor_p1` are additive on `ScoreboardConfig`,
    /// so a config without them loads.
    #[test]
    fn scoreboard_config_defaults_its_format_and_anchor() {
        let json = r#"{"home":{"name":"A","primaryColor":{"r":0,"g":0,"b":0,"a":1},"secondaryColor":{"r":1,"g":1,"b":1,"a":1},"fontColor":{"r":1,"g":1,"b":1,"a":1}},"away":{"name":"B","primaryColor":{"r":0,"g":0,"b":0,"a":1},"secondaryColor":{"r":1,"g":1,"b":1,"a":1},"fontColor":{"r":1,"g":1,"b":1,"a":1}}}"#;
        let c: ScoreboardConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.format, MatchFormat::default());
        assert!(!c.auto_back_anchor_p1);
    }
}
