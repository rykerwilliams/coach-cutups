//! The Match panel's text and the setup sheet's fields (Phase 9 spec S4),
//! kept out of the UI code so they're tested headless.
//!
//! The panel is a rendering of the project plus one instant of the scan: the
//! rows come from `ProjectChanged`, the score and clock from the tick's scan
//! anchor. Nothing here decides anything — tagging, deleting and the setup all
//! go through the bus.

use uuid::Uuid;
use video_coach_core::match_entry::{
    format_line, format_time, Batch, BatchVerdict, PendingMatchEvent,
};
use video_coach_core::project::Project;
use video_coach_core::scoreboard::{
    format_clock, labelled_events, MatchEventKind, MatchEventRecord, MatchFormat, ScoreboardConfig,
    ScoreboardState, TeamConfig,
};
use video_coach_core::stroke::Rgba;

use crate::format::format_hms;

/// How near the playhead a chapter can be and still count as the one under
/// it, which `[` and `]` step past, either side alike. A seek lands on a frame,
/// not on the tag's exact instant, so a playhead sent to a chapter sits a hair
/// either side of it — and a press there has to move on, not land again.
pub const CHAPTER_TOLERANCE: f64 = 0.5;

/// One row of the panel's event list, which is also a chapter (spec C1): the
/// scrubber's marks are built from these rows, and `[` / `]` from the same
/// times ([`match_abs`]).
pub struct MatchRowText {
    pub id: Uuid,
    pub kind: MatchEventKind,
    /// Which source video it is tagged on, and how far into it: where the
    /// editor's row reads it from ([`editor_row_where`]) and what the line in
    /// its field says. The panel ignores both — they are here rather than in a
    /// second builder, so the editor cannot order its list differently from
    /// the panel, the scrubber's marks and `[` / `]` (spec T1).
    pub source_index: usize,
    pub source_seconds: f64,
    /// Where it sits on the concat timeline, in seconds.
    pub abs: f64,
    /// Where it sits on the concat timeline, already formatted.
    pub time: String,
    /// `"1H start"`, `"Home goal"`, …
    pub label: String,
    /// A start/stop the format has no period for
    /// ([`LabelledEvent::role_less`](video_coach_core::scoreboard::LabelledEvent::role_less)).
    pub role_less: bool,
    /// A goal's span in the reel, `"−20 s / +6 s"`: its trims, or the
    /// defaults (spec R3). `None` for anything but a goal.
    pub reel_span: Option<String>,
}

/// Every tagged event in match order, as the panel lists it.
///
/// The labels are core's ([`labelled_events`]): the tagging vocabulary, which
/// is deliberately not how a whole-match export words the same events in its
/// chapters (`core::scoreboard::chapter_events`, spec W3).
pub fn match_rows(project: &Project) -> Vec<MatchRowText> {
    labelled_events(project)
        .into_iter()
        .map(|e| MatchRowText {
            id: e.event.id,
            kind: e.event.kind,
            source_index: e.event.source_index,
            source_seconds: e.event.source_seconds,
            abs: e.abs_seconds,
            time: format_hms(e.abs_seconds),
            label: e.label,
            role_less: e.role_less,
            reel_span: e.event.kind.is_goal().then(|| reel_span(e.event)),
        })
        .collect()
}

/// `"−20 s / +6 s"`: how far the goal's reel entry runs either side of it,
/// before the clamps (spec R2), which only the export sees.
fn reel_span(goal: &MatchEventRecord) -> String {
    // To the tenth, and whole seconds without one, since the defaults are
    // whole and a trim set from a paused frame rarely is.
    let seconds = |s: f64| {
        let tenths = (s * 10.0).round() / 10.0;
        match tenths.fract() == 0.0 {
            true => format!("{tenths:.0}"),
            false => format!("{tenths:.1}"),
        }
    };
    let (lead_in, tail) = goal.reel_span();
    format!("−{} s / +{} s", seconds(lead_in), seconds(tail))
}

// ------------------------------------------------- the match event editor
//
// The editor's every string, so the sheet holds none (spec T, B). The
// verdicts are core's `match_entry`, which the bus parses the same text with:
// the wording lives here and the rules live there, and the sheet's mark and
// the command it sends can't reach different answers.

/// The line the editor seeds a selected row's field with: core's
/// [`format_line`] for that row's record.
///
/// The bus rebuilds the same seed from the same record when the line comes
/// back (`bus::scoreboard::edit_match_event`), so an edit that leaves the time
/// alone keeps the stored seconds to the last decimal. Empty for a row with no
/// record behind it, which [`match_rows`] never produces.
pub fn editor_row_line(project: &Project, row: &MatchRowText) -> String {
    project
        .match_events
        .iter()
        .find(|m| m.id == row.id)
        .map(|record| format_line(project, record))
        .unwrap_or_default()
}

/// A row's "where": the 1-based video number and the time into that video, the
/// paste grammar's own numbering (spec T2).
pub fn editor_row_where(row: &MatchRowText) -> String {
    where_text(row.source_index, row.source_seconds)
}

fn where_text(source_index: usize, source_seconds: f64) -> String {
    format!("{} · {}", source_index + 1, format_time(source_seconds))
}

/// One echoed line of the paste box (spec B3).
pub struct PasteLineText {
    /// `✓` it will add, `•` it is already tagged, `✗` it is refused.
    pub glyph: &'static str,
    pub text: String,
}

/// Everything the paste box says about the block it holds: a line each, the
/// summary above the button, and the button's own label.
pub struct PasteEcho {
    pub lines: Vec<PasteLineText>,
    pub summary: String,
    pub button: String,
}

/// Reads a parsed block back to the coach, line by line, before Add is pressed
/// (spec B3).
///
/// Blank and comment-only lines say nothing, so they are not in `batch.lines`
/// and get no row here. A refusal quotes its line back with its number, which
/// is how the coach finds it in a block of twenty.
pub fn paste_echo(batch: &Batch) -> PasteEcho {
    let (mut adding, mut tagged, mut refused) = (0, 0, 0);
    let lines = batch
        .lines
        .iter()
        .map(|line| match &line.verdict {
            BatchVerdict::Added(event) => {
                adding += 1;
                PasteLineText {
                    glyph: "✓",
                    text: reads_as(event),
                }
            }
            BatchVerdict::AlreadyTagged(event) => {
                tagged += 1;
                PasteLineText {
                    glyph: "•",
                    text: format!("{} — already tagged, skipped", reads_as(event)),
                }
            }
            BatchVerdict::Refused(reason) => {
                refused += 1;
                PasteLineText {
                    glyph: "✗",
                    text: format!("line {}: \"{}\" — {reason}", line.number, line.text),
                }
            }
        })
        .collect();

    let mut parts: Vec<String> = Vec::new();
    if adding > 0 {
        parts.push(format!("{adding} {} to add", plural(adding, "event")));
    }
    if tagged > 0 {
        parts.push(format!("{tagged} already tagged"));
    }
    if refused > 0 {
        parts.push(format!("{refused} {} refused", plural(refused, "line")));
    }
    PasteEcho {
        lines,
        summary: parts.join(" · "),
        // Nothing to add is a disabled button, which says nothing but its name.
        button: match adding {
            0 => "Add".to_string(),
            n => format!("Add {n} {}", plural(n, "event")),
        },
    }
}

fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => noun.to_string(),
        _ => format!("{noun}s"),
    }
}

/// A line that will add, or one already tagged, as the echo reads it back:
/// where it lands and what it is.
fn reads_as(event: &PendingMatchEvent) -> String {
    format!(
        "{} · {}",
        where_text(event.source_index, event.source_seconds),
        pending_label(event.kind)
    )
}

/// An untagged event's kind in the panel's own words ([`labelled_events`]).
///
/// A start/stop is just that: `interpret` gives it a period from its place
/// among the others, which it does not have until it lands.
fn pending_label(kind: MatchEventKind) -> &'static str {
    match kind {
        MatchEventKind::HomeGoal => "Home goal",
        MatchEventKind::AwayGoal => "Away goal",
        MatchEventKind::StartStop => "Start/stop",
    }
}

/// Every match event's place on the concat timeline, in match order: the
/// chapters `[` and `]` step through, the same times as [`match_rows`]'s —
/// the same list, so they cannot drift apart.
pub fn match_abs(project: &Project) -> Vec<f64> {
    labelled_events(project)
        .into_iter()
        .map(|e| e.abs_seconds)
        .collect()
}

/// Where `]` goes from `abs`: the first chapter more than
/// [`CHAPTER_TOLERANCE`] after it. `chapters` are in order, as
/// [`match_abs`] gives them.
pub fn next_chapter(abs: f64, chapters: &[f64]) -> Option<f64> {
    chapters
        .iter()
        .copied()
        .find(|&at| at > abs + CHAPTER_TOLERANCE)
}

/// Where `[` goes from `abs`: the last chapter more than
/// [`CHAPTER_TOLERANCE`] before it.
pub fn previous_chapter(abs: f64, chapters: &[f64]) -> Option<f64> {
    chapters
        .iter()
        .rev()
        .copied()
        .find(|&at| at < abs - CHAPTER_TOLERANCE)
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
    use video_coach_core::match_entry::parse_batch;
    use video_coach_core::project::SourceRef;
    use video_coach_core::scoreboard::{ReelEnd, ScoreboardContext};

    fn project() -> Project {
        let mut p = Project::new("p");
        for i in 0..2 {
            p.source_videos.push(SourceRef {
                relative_path: format!("{i}.mp4"),
                // A name the editor's refusals can quote back readably.
                display_name: format!("clip {}", i + 1),
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
    fn a_goal_row_shows_its_reel_span() {
        let mut p = project();
        p.append_match_event(MatchEventKind::StartStop, 0, 10.0);
        let goal = p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
        let spans = |p: &Project| -> Vec<Option<String>> {
            match_rows(p).into_iter().map(|r| r.reel_span).collect()
        };
        // The defaults, and no span on a start/stop.
        assert_eq!(spans(&p), [None, Some("−20 s / +6 s".to_string())]);
        // A fractional trim reads to the tenth.
        p.set_reel_trim(goal, ReelEnd::End, Some((0, 104.5)))
            .unwrap();
        assert_eq!(spans(&p)[1].as_deref(), Some("−20 s / +4.5 s"));
    }

    #[test]
    fn previous_and_next_chapter_skip_the_one_under_the_playhead() {
        let mut p = project();
        // The second source starts 600 s in.
        p.append_match_event(MatchEventKind::HomeGoal, 0, 100.0);
        p.append_match_event(MatchEventKind::StartStop, 0, 400.0);
        p.append_match_event(MatchEventKind::AwayGoal, 1, 212.0);
        p.append_match_event(MatchEventKind::StartStop, 1, 400.0);
        let rows = match_abs(&p);
        assert_eq!(rows, [100.0, 400.0, 812.0, 1000.0]);
        assert_eq!(
            match_rows(&p).iter().map(|r| r.abs).collect::<Vec<_>>(),
            rows,
            "the chapters are the rows"
        );

        // The two ends: nothing before the first, nothing after the last.
        assert_eq!(previous_chapter(100.0, &rows), None);
        assert_eq!(previous_chapter(50.0, &rows), None);
        assert_eq!(next_chapter(1000.0, &rows), None);
        assert_eq!(next_chapter(1100.0, &rows), None);
        assert_eq!(next_chapter(0.0, &rows), Some(100.0));
        assert_eq!(previous_chapter(1100.0, &rows), Some(1000.0));

        // Sitting on a chapter moves on to the next one either way.
        assert_eq!(next_chapter(400.0, &rows), Some(812.0));
        assert_eq!(previous_chapter(400.0, &rows), Some(100.0));
        // And so does a hair either side of it (a seek lands on a frame, not
        // on the tag's instant), in both directions.
        assert_eq!(next_chapter(811.999, &rows), Some(1000.0));
        assert_eq!(previous_chapter(811.999, &rows), Some(400.0));
        assert_eq!(next_chapter(812.3, &rows), Some(1000.0));
        assert_eq!(previous_chapter(812.3, &rows), Some(400.0));
        // Past the tolerance, the one it has left is a chapter again.
        assert_eq!(previous_chapter(812.6, &rows), Some(812.0));
        assert_eq!(next_chapter(811.4, &rows), Some(812.0));
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

    /// The editor's row, from the same list the panel's rows come from: the
    /// video it is tagged on, the time into it, and the line its field is
    /// seeded with — core's, not a second rendering.
    #[test]
    fn an_editor_row_names_its_video_and_carries_its_line() {
        let mut p = project();
        p.append_match_event(MatchEventKind::HomeGoal, 0, 30.0);
        // The second source starts 600 s in, so these two sort after it.
        p.append_match_event(MatchEventKind::StartStop, 1, 0.0);
        p.append_match_event(MatchEventKind::AwayGoal, 1, 125.06);

        let rows = match_rows(&p);
        assert_eq!(
            rows.iter()
                .map(|r| (r.source_index, r.source_seconds))
                .collect::<Vec<_>>(),
            [(0, 30.0), (1, 0.0), (1, 125.06)]
        );
        assert_eq!(
            rows.iter().map(editor_row_where).collect::<Vec<_>>(),
            ["1 · 0:30.0", "2 · 0:00.0", "2 · 2:05.0"]
        );
        // One line per kind, and `period` for the start/stop, whose stored
        // record doesn't know which end of a half it is.
        assert_eq!(
            rows.iter()
                .map(|r| editor_row_line(&p, r))
                .collect::<Vec<_>>(),
            [
                "1 0:30.0 home goal",
                "2 0:00.0 period",
                "2 2:05.0 away goal"
            ]
        );
        // And it is core's own rendering, which the bus rebuilds as the seed.
        for row in &rows {
            let record = p.match_events.iter().find(|m| m.id == row.id).unwrap();
            assert_eq!(editor_row_line(&p, row), format_line(&p, record));
        }
    }

    /// The paste box's own feedback, for a block holding one of each verdict:
    /// the glyphs, the sentences and the summary, singular and plural both.
    #[test]
    fn the_echo_reads_back_every_verdict() {
        let mut p = project();
        p.append_match_event(MatchEventKind::HomeGoal, 0, 200.0);
        let batch = parse_batch(
            &p,
            0,
            "2 5:05 away goal\n\
             # notes\n\
             1 3:20 home goal\n\
             2 20:00 home goal\n\
             \n\
             1 14:05 kick-off\n",
        );
        let echo = paste_echo(&batch);

        // Blanks and comments say nothing at all.
        assert_eq!(echo.lines.len(), 4);
        assert_eq!(
            echo.lines.iter().map(|l| l.glyph).collect::<Vec<_>>(),
            ["✓", "•", "✗", "✗"]
        );
        assert_eq!(echo.lines[0].text, "2 · 5:05.0 · Away goal");
        assert_eq!(
            echo.lines[1].text,
            "1 · 3:20.0 · Home goal — already tagged, skipped"
        );
        // A refusal quotes the line back with its number, so the coach finds
        // it in a block of twenty.
        assert_eq!(
            echo.lines[2].text,
            "line 4: \"2 20:00 home goal\" — clip 2 is 10:00.0 long"
        );
        assert!(
            echo.lines[3]
                .text
                .starts_with("line 6: \"1 14:05 kick-off\" — a restart after a goal"),
            "{}",
            echo.lines[3].text
        );
        assert!(
            echo.lines[3].text.contains("move every later period"),
            "{}",
            echo.lines[3].text
        );

        assert_eq!(
            echo.summary,
            "1 event to add · 1 already tagged · 2 lines refused"
        );
        assert_eq!(echo.button, "Add 1 event");

        // And the plural, from a block that lands whole.
        let batch = parse_batch(&p, 0, "2 5:05 away goal\n2 6:05 away goal\n");
        let echo = paste_echo(&batch);
        assert_eq!(echo.summary, "2 events to add");
        assert_eq!(echo.button, "Add 2 events");

        // Nothing typed: nothing to say, and nothing to press.
        let echo = paste_echo(&parse_batch(&p, 0, "  \n# just a note\n"));
        assert!(echo.lines.is_empty());
        assert_eq!(echo.summary, "");
        assert_eq!(echo.button, "Add");
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
