//! Scoreboard and match-format configuration — **data only**.
//!
//! The positional interpretation that turns tagged start/stop events into
//! period roles and a clock (`interpret`, `scoreboard_state`) is Phase 9, along
//! with `MatchFormat`'s derived accessors. Only what the project file stores
//! lives here.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::stroke::Rgba;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreboardConfig {
    pub home: TeamConfig,
    pub away: TeamConfig,
    #[serde(default)]
    pub format: MatchFormat,
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
/// via `Project::abs_seconds`; the match clock runs on that timeline, not on
/// any single source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchEventRecord {
    pub id: Uuid,
    pub kind: MatchEventKind,
    pub source_index: usize,
    pub source_seconds: f64,
    /// Set on the synthetic period-1 start inserted when the recording missed
    /// kickoff. Phase 9 uses it to back-compute the displayed clock.
    #[serde(default)]
    pub is_auto_back_anchor: bool,
}
