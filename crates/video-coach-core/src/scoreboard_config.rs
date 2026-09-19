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
    fn match_event_record_round_trips_and_defaults_the_anchor_flag() {
        let r = MatchEventRecord {
            id: uuid::Uuid::nil(),
            kind: MatchEventKind::HomeGoal,
            source_index: 1,
            source_seconds: 123.5,
            is_auto_back_anchor: false,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""sourceSeconds":123.5"#), "got {s}");
        assert_eq!(serde_json::from_str::<MatchEventRecord>(&s).unwrap(), r);

        // The flag is additive, so a record without it must still load.
        let without = r#"{"id":"00000000-0000-0000-0000-000000000000","kind":"awayGoal","sourceIndex":0,"sourceSeconds":1.0}"#;
        assert!(
            !serde_json::from_str::<MatchEventRecord>(without)
                .unwrap()
                .is_auto_back_anchor
        );
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

    /// `format` is additive on `ScoreboardConfig`, so a config without it loads.
    #[test]
    fn scoreboard_config_defaults_its_format() {
        let json = r#"{"home":{"name":"A","primaryColor":{"r":0,"g":0,"b":0,"a":1},"secondaryColor":{"r":1,"g":1,"b":1,"a":1},"fontColor":{"r":1,"g":1,"b":1,"a":1}},"away":{"name":"B","primaryColor":{"r":0,"g":0,"b":0,"a":1},"secondaryColor":{"r":1,"g":1,"b":1,"a":1},"fontColor":{"r":1,"g":1,"b":1,"a":1}}}"#;
        let c: ScoreboardConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.format, MatchFormat::default());
    }
}
