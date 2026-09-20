//! The Match panel's text and the setup sheet's fields (Phase 9 spec S4),
//! kept out of the UI code so they're tested headless.
//!
//! The panel is a rendering of the project plus one instant of the scan: the
//! rows come from `ProjectChanged`, the score and clock from the tick's scan
//! anchor. Nothing here decides anything — tagging, deleting and the setup all
//! go through the bus.

use uuid::Uuid;
use video_coach_core::project::Project;
use video_coach_core::scoreboard::{
    format_clock, interpret, MatchEventKind, MatchFormat, PeriodRole, ScoreboardConfig,
    ScoreboardState, TeamConfig,
};
use video_coach_core::stroke::Rgba;

use crate::format::format_hms;

/// One row of the panel's event list.
pub struct MatchRowText {
    pub id: Uuid,
    /// Where it sits on the concat timeline, already formatted.
    pub time: String,
    /// `"1H start"`, `"Home goal"`, …
    pub label: String,
    /// A start/stop the format has no period for, so [`interpret`] gives it no
    /// role. Reachable with the back-anchor on, whose derived start takes a
    /// period without taking one of the cap's places (spec S5): the record is
    /// kept and turning the anchor off restores its role, so the row says so
    /// rather than looking like every other one.
    pub role_less: bool,
}

/// Every tagged event in match order — the order [`interpret`] walks, so a
/// row's role is the role the scoreboard gives it.
pub fn match_rows(project: &Project) -> Vec<MatchRowText> {
    let abs = |source_index, source_seconds| project.abs_seconds(source_index, source_seconds);
    // Roles only exist once there is a format to interpret against.
    let roles: Vec<(Uuid, PeriodRole)> = project.scoreboard.as_ref().map_or_else(Vec::new, |c| {
        interpret(&project.absolute_match_events(), c)
            .into_iter()
            .filter_map(|e| Some((e.id?, e.role)))
            .collect()
    });

    let mut rows: Vec<(f64, MatchRowText)> = project
        .match_events
        .iter()
        .map(|m| {
            let at = abs(m.source_index, m.source_seconds);
            let role = roles.iter().find(|(id, _)| *id == m.id).map(|(_, r)| *r);
            let (label, role_less) = match (m.kind, &project.scoreboard) {
                (MatchEventKind::HomeGoal, _) => ("Home goal".to_string(), false),
                (MatchEventKind::AwayGoal, _) => ("Away goal".to_string(), false),
                (MatchEventKind::StartStop, None) => ("Start/stop".to_string(), false),
                (MatchEventKind::StartStop, Some(c)) => match role {
                    Some(PeriodRole::Start(p)) => {
                        (format!("{} start", c.format.period_name(p)), false)
                    }
                    Some(PeriodRole::End(p)) => (format!("{} end", c.format.period_name(p)), false),
                    None => ("Start/stop (no period)".to_string(), true),
                },
            };
            let row = MatchRowText {
                id: m.id,
                time: format_hms(at),
                label,
                role_less,
            };
            (at, row)
        })
        .collect();
    // Stable, so two events at the same instant keep their tag order, as
    // `interpret` does.
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// The panel's live line: the score once the match has started, and the two
/// names before it.
pub fn score_line(config: &ScoreboardConfig, state: Option<&ScoreboardState>) -> String {
    let (home, away) = (&config.home.name, &config.away.name);
    match state {
        Some(s) => format!("{home} {} – {} {away}", s.home_score, s.away_score),
        None => format!("{home} – {away}"),
    }
}

/// The panel's clock: what the scoreboard's clock cell reads, with the
/// stoppage tail beside it, and a dash before the match has started.
pub fn clock_text(state: Option<&ScoreboardState>) -> String {
    let Some(state) = state else {
        return "–".to_string();
    };
    let labels = format_clock(state.clock);
    if labels.trailing.is_empty() {
        labels.main
    } else {
        format!("{} {}", labels.main, labels.trailing)
    }
}

/// How many tagged start/stops a format of `total_periods` has no period for.
///
/// `back_anchor` is [`ScoreboardConfig::auto_back_anchor_p1`] as the sheet
/// currently has it: [`interpret`] prepends the derived start and *then* caps
/// the list, so the anchor takes a period, leaving `2 × total_periods − 1`
/// places for stored events. Without it this disagreed with the rows, which
/// already mark the leftover start/stop role-less.
///
/// `Project::start_stops_at_cap` deliberately counts records instead, so the
/// coach never loses a *stored* event to the anchor (spec S1): the two numbers
/// are different questions, and with the anchor on the last storable start/stop
/// is over this cap.
///
/// The `2 ×` is [`MatchFormat::expected_start_stop_events`]'s rule, which core
/// keeps in one place — but the caller here has two period counts typed into a
/// sheet and no format to hand, and building one to ask would be more
/// ceremony than the rule is long.
pub fn over_cap(project: &Project, total_periods: u32, back_anchor: bool) -> usize {
    let places = (2 * total_periods as usize).saturating_sub(usize::from(back_anchor));
    project.start_stop_count().saturating_sub(places)
}

/// The setup sheet's warning for start/stops the format being typed has no
/// period for; empty when there are none.
///
/// The records are never dropped — the cap is on what [`interpret`] gives a
/// role to — so this is a warning, not a refusal.
pub fn over_cap_warning(project: &Project, total_periods: u32, back_anchor: bool) -> String {
    match over_cap(project, total_periods, back_anchor) {
        0 => String::new(),
        1 => "1 tagged start/stop has no period in this format. It is kept, but \
              the scoreboard ignores it until there is a period for it."
            .to_string(),
        n => format!(
            "{n} tagged start/stops have no period in this format. They are kept, \
             but the scoreboard ignores them until there are periods for them."
        ),
    }
}

/// What the setup sheet starts from when the project has no scoreboard yet:
/// no names (the bus refuses those, so Save waits for them), a colour each
/// and white lettering, and the default format.
pub fn blank_config() -> ScoreboardConfig {
    let team = |primary| TeamConfig::new("", primary, WHITE);
    ScoreboardConfig {
        home: team(Rgba {
            r: 0.12,
            g: 0.31,
            b: 0.85,
            a: 1.0,
        }),
        away: team(Rgba {
            r: 0.78,
            g: 0.17,
            b: 0.11,
            a: 1.0,
        }),
        format: MatchFormat::default(),
        auto_back_anchor_p1: false,
    }
}

const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// A team colour as the setup sheet's field shows it.
///
/// Opaque: the scoreboard's cells are fills and macOS's picker had
/// `supportsOpacity: false`, so alpha is not editable and [`parse_hex`]
/// always returns 1. A colour that goes out to a field and back is rounded to
/// 8 bits a channel, which is what a colour typed as hex is anyway.
pub fn hex(color: Rgba) -> String {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    )
}

/// `#RRGGBB` (or bare `RRGGBB`) back to a colour; `None` for anything else,
/// which the sheet marks and refuses to save.
pub fn parse_hex(text: &str) -> Option<Rgba> {
    let text = text.trim();
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| {
        u8::from_str_radix(&digits[i..i + 2], 16)
            .map(|v| f64::from(v) / 255.0)
            .ok()
    };
    Some(Rgba {
        r: channel(0)?,
        g: channel(2)?,
        b: channel(4)?,
        a: 1.0,
    })
}

// The setup sheet's numeric fields, one function each, so a range is written
// once: the sheet's "this field is good" mark and the parse that builds the
// config call the same one and can't drift apart. Written on both sides they
// did, and a drift leaves Save enabled on a setup that then fails to read.
// Each returns `None` for anything out of range or not a plain number, which
// the sheet marks and refuses to save on.

/// Regulation periods: a match has at least one.
pub fn parse_periods(text: &str) -> Option<u32> {
    parse_count(text, 1, 10)
}

/// Overtime periods, which unlike regulation ones may be none at all.
pub fn parse_overtime_periods(text: &str) -> Option<u32> {
    parse_count(text, 0, 10)
}

/// A period's length in whole minutes, regulation or overtime.
pub fn parse_minutes(text: &str) -> Option<u32> {
    parse_count(text, 1, 180)
}

fn parse_count(text: &str, min: u32, max: u32) -> Option<u32> {
    let n: u32 = text.trim().parse().ok()?;
    (min..=max).contains(&n).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use video_coach_core::project::SourceRef;
    use video_coach_core::scoreboard::ScoreboardContext;

    fn project() -> Project {
        let mut p = Project::new("p");
        for i in 0..2 {
            p.source_videos.push(SourceRef {
                relative_path: format!("{i}.mp4"),
                display_name: format!("{i}"),
                duration_seconds: 600.0,
                display_aspect: 16.0 / 9.0,
            });
        }
        p.scoreboard = Some(ScoreboardConfig {
            home: TeamConfig::new("Rovers", Rgba::RED, Rgba::RED),
            away: TeamConfig::new("United", Rgba::RED, Rgba::RED),
            format: MatchFormat {
                regulation_period_seconds: 60,
                ..MatchFormat::default()
            },
            auto_back_anchor_p1: false,
        });
        p
    }

    fn labels(project: &Project) -> Vec<String> {
        match_rows(project).into_iter().map(|r| r.label).collect()
    }

    #[test]
    fn rows_are_in_match_order_with_the_roles_the_scoreboard_gives_them() {
        let mut p = project();
        // Tagged out of order, and the second source's events are 600 s on.
        p.append_match_event(MatchEventKind::StartStop, 1, 10.0);
        p.append_match_event(MatchEventKind::StartStop, 0, 0.0);
        p.append_match_event(MatchEventKind::HomeGoal, 0, 30.0);

        let rows = match_rows(&p);
        assert_eq!(
            rows.iter().map(|r| r.time.as_str()).collect::<Vec<_>>(),
            ["0:00", "0:30", "10:10"]
        );
        assert_eq!(
            rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            ["1H start", "Home goal", "1H end"]
        );
        assert!(rows.iter().all(|r| !r.role_less));
    }

    /// The row a back-anchored match can store past the format's last period:
    /// kept, listed, visibly without one — and counted by the warning, which
    /// has to agree with the row rather than with the record cap.
    #[test]
    fn a_start_stop_the_format_has_no_period_for_says_so() {
        let mut p = project();
        let config = p.scoreboard.take().unwrap();
        p.scoreboard = Some(ScoreboardConfig {
            auto_back_anchor_p1: true,
            ..config
        });
        for i in 0..4 {
            p.append_match_event(MatchEventKind::StartStop, 0, f64::from(i));
        }
        assert_eq!(
            labels(&p),
            ["1H end", "2H start", "2H end", "Start/stop (no period)"]
        );
        assert!(match_rows(&p).last().unwrap().role_less);
        // The anchor takes a period, so four records don't fit two of them:
        // the warning counts the same one the row marks.
        assert_eq!(over_cap(&p, 2, true), 1);
        assert_eq!(over_cap(&p, 1, true), 3);
        // Turning the anchor off gives that record its role back, and the
        // warning goes with it.
        assert_eq!(over_cap(&p, 2, false), 0);
        assert!(
            over_cap_warning(&p, 2, true).starts_with("1 tagged start/stop has no period"),
            "{}",
            over_cap_warning(&p, 2, true)
        );
        assert_eq!(over_cap_warning(&p, 2, false), "");
        assert!(
            over_cap_warning(&p, 1, false).starts_with("2 tagged start/stops have no period"),
            "{}",
            over_cap_warning(&p, 1, false)
        );
        p.delete_match_event(match_rows(&p)[0].id);
        assert!(
            over_cap_warning(&p, 1, false).starts_with("1 tagged start/stop has no period"),
            "{}",
            over_cap_warning(&p, 1, false)
        );
    }

    /// Without a scoreboard there is no format, so no row claims a period.
    #[test]
    fn with_no_scoreboard_a_start_stop_is_just_a_start_stop() {
        let mut p = project();
        p.scoreboard = None;
        p.append_match_event(MatchEventKind::StartStop, 0, 1.0);
        assert_eq!(labels(&p), ["Start/stop"]);
        assert!(!match_rows(&p).last().unwrap().role_less);
    }

    #[test]
    fn the_live_line_shows_the_names_until_the_match_starts() {
        let mut p = project();
        p.append_match_event(MatchEventKind::StartStop, 0, 100.0);
        p.append_match_event(MatchEventKind::HomeGoal, 0, 130.0);
        let ctx = ScoreboardContext::for_project(&p).unwrap();
        let config = p.scoreboard.as_ref().unwrap();

        let before = ctx.state_at(0, 50.0);
        assert_eq!(score_line(config, before.as_ref()), "Rovers – United");
        assert_eq!(clock_text(before.as_ref()), "–");

        let during = ctx.state_at(0, 140.0);
        assert_eq!(score_line(config, during.as_ref()), "Rovers 1 – 0 United");
        assert_eq!(clock_text(during.as_ref()), "00:40");

        // Past the one-minute period: the tail rides beside the clock.
        let stoppage = ctx.state_at(0, 175.0);
        assert_eq!(clock_text(stoppage.as_ref()), "01:00 +0:15");
    }

    #[test]
    fn colours_round_trip_through_the_sheet_s_field() {
        // A field holds 8 bits a channel, so a colour goes through it rounded.
        let color = Rgba {
            r: 0.0,
            g: 128.0 / 255.0,
            b: 1.0,
            a: 1.0,
        };
        assert_eq!(hex(color), "#0080ff");
        assert_eq!(parse_hex("#0080ff"), Some(color));
        assert_eq!(parse_hex(" 0080FF "), Some(color));
        assert_eq!(hex(parse_hex("#123456").unwrap()), "#123456");
        for bad in ["", "#12345", "#1234567", "#12345g", "fuchsia"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_format_count_has_to_be_a_number_in_its_field_s_range() {
        assert_eq!(parse_minutes(" 45 "), Some(45));
        assert_eq!(parse_minutes("180"), Some(180));
        assert_eq!(parse_minutes("181"), None);
        assert_eq!(parse_minutes("0"), None);
        // Only overtime may be none at all.
        assert_eq!(parse_overtime_periods("0"), Some(0));
        assert_eq!(parse_periods("0"), None);
        assert_eq!(parse_periods("10"), Some(10));
        assert_eq!(parse_periods("11"), None);
        for bad in ["-1", "4.5", "", "two"] {
            assert_eq!(parse_periods(bad), None, "{bad}");
        }
    }
}
