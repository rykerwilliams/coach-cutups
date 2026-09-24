//! The one grammar behind the editor row's field and the paste box.

use video_coach_core::match_entry::{
    edit_from_line, format_line, format_time, parse_batch, parse_line, parse_time, BatchVerdict,
    LineVerdict, PendingMatchEvent,
};
use video_coach_core::project::{Project, SourceRef};
use video_coach_core::scoreboard::{
    MatchEventKind, MatchEventRecord, MatchFormat, ScoreboardConfig, TeamConfig,
};
use video_coach_core::stroke::Rgba;

const HOME: MatchEventKind = MatchEventKind::HomeGoal;
const AWAY: MatchEventKind = MatchEventKind::AwayGoal;
const START_STOP: MatchEventKind = MatchEventKind::StartStop;

fn project_with_sources(durations: &[f64]) -> Project {
    let mut p = Project::new("p");
    for (i, &duration) in durations.iter().enumerate() {
        p.source_videos.push(SourceRef {
            relative_path: format!("{i}.mp4"),
            display_name: format!("half {}", i + 1),
            duration_seconds: duration,
            display_aspect: 16.0 / 9.0,
        });
    }
    p
}

fn scoreboard(home: &str, away: &str) -> ScoreboardConfig {
    ScoreboardConfig {
        home: TeamConfig::new(home, Rgba::RED, Rgba::RED),
        away: TeamConfig::new(away, Rgba::RED, Rgba::RED),
        format: MatchFormat::default(),
        auto_back_anchor_p1: false,
    }
}

/// The event a line makes, or a panic naming what came back instead.
fn event(project: &Project, line: &str) -> PendingMatchEvent {
    match parse_line(project, 0, line) {
        LineVerdict::Event(e) => e,
        other => panic!("{line:?} should be an event, got {other:?}"),
    }
}

/// Why a line was refused.
fn refusal(project: &Project, line: &str) -> String {
    match parse_line(project, 0, line) {
        LineVerdict::Refused(reason) => reason,
        other => panic!("{line:?} should be refused, got {other:?}"),
    }
}

fn record(kind: MatchEventKind, source_index: usize, source_seconds: f64) -> MatchEventRecord {
    MatchEventRecord {
        id: uuid::Uuid::new_v4(),
        kind,
        source_index,
        source_seconds,
        reel_lead_in: None,
        reel_tail: None,
    }
}

// ---------------------------------------------------------- the kick-off words

/// The invariant the whole grammar hangs off: a restart is not a period
/// boundary. `interpret` is positional, so one spurious start/stop moves every
/// later period — and the clock burned into every export with it.
#[test]
fn a_kick_off_word_is_never_a_period_boundary() {
    let p = project_with_sources(&[3000.0]);
    for word in ["kickoff", "kick-off", "ko", "restart", "whistle"] {
        let line = format!("1 14:05 {word}");
        match parse_line(&p, 0, &line) {
            LineVerdict::Refused(reason) => {
                let reason = reason.to_lowercase();
                assert!(
                    reason.contains("period"),
                    "{word}: the reason must name the period shift, got {reason:?}"
                );
            }
            other => panic!("{word} must be refused, got {other:?}"),
        }
    }
}

/// And a period's own words still read as a start/stop, so the refusal above
/// is a decision and not a parser that understands nothing.
#[test]
fn a_periods_own_words_still_read_as_start_stop() {
    let p = project_with_sources(&[3000.0]);
    for word in ["v", "start", "stop", "end", "period", "half", "ht", "ft"] {
        assert_eq!(
            event(&p, &format!("1 14:05 {word}")).kind,
            START_STOP,
            "{word}"
        );
    }
}

// ------------------------------------------------------------------- the time

#[test]
fn a_time_needs_a_colon_and_every_field_after_the_first_is_two_digits_under_sixty() {
    assert_eq!(parse_time("0:05"), Some(5.0));
    assert_eq!(parse_time("14:05"), Some(845.0));
    // The leading field is unbounded: 75:20 in an 80-minute file.
    assert_eq!(parse_time("75:20"), Some(4520.0));
    assert_eq!(parse_time("1:02:03"), Some(3723.0));
    assert_eq!(parse_time("14:05.5"), Some(845.5));
    assert_eq!(parse_time("  14:05  "), Some(845.0));

    for bad in [
        "1405", "14", "14:60", "-1:00", "14:5:3", "", "14:", ":05", "14:05.", "a:bc",
    ] {
        assert_eq!(parse_time(bad), None, "{bad:?} must not parse");
    }
}

#[test]
fn format_time_floors_in_tenths_and_grows_an_hours_field() {
    assert_eq!(format_time(0.0), "0:00.0");
    assert_eq!(format_time(14.06), "0:14.0");
    assert_eq!(format_time(845.5), "14:05.5");
    assert_eq!(format_time(3723.45), "1:02:03.4");
    assert_eq!(format_time(f64::NAN), "0:00.0");

    for &seconds in &[0.0, 14.06, 754.99, 3723.45, 4520.0] {
        let round_tripped = parse_time(&format_time(seconds)).expect("its own rendering parses");
        assert!(
            (seconds - round_tripped).abs() < 0.1,
            "{seconds} rendered as {}",
            format_time(seconds)
        );
    }
}

// ------------------------------------------------------------- the vocabulary

#[test]
fn every_word_of_the_vocabulary_names_its_kind() {
    let p = project_with_sources(&[3000.0]);
    for word in ["home", "hg", "z"] {
        assert_eq!(event(&p, &format!("14:05 {word}")).kind, HOME, "{word}");
    }
    for word in ["away", "ag", "x"] {
        assert_eq!(event(&p, &format!("14:05 {word}")).kind, AWAY, "{word}");
    }
    for word in ["fulltime", "halftime"] {
        assert_eq!(
            event(&p, &format!("14:05 {word}")).kind,
            START_STOP,
            "{word}"
        );
    }
}

#[test]
fn case_separators_and_filler_words_dont_change_the_kind() {
    let p = project_with_sources(&[3000.0]);
    for line in [
        "14:05 HOME GOAL",
        "14:05 Home, goal",
        "14:05 home-goal",
        "14:05 home_goal",
        "14:05   home    goals  ",
        "14:05 the home goal # a comment",
    ] {
        assert_eq!(event(&p, line).kind, HOME, "{line:?}");
    }
}

#[test]
fn a_blank_or_comment_only_line_says_nothing_at_all() {
    let p = project_with_sources(&[3000.0]);
    for line in ["", "   ", "# 14:05 home goal", "   # notes"] {
        assert_eq!(parse_line(&p, 0, line), LineVerdict::Nothing, "{line:?}");
    }
}

#[test]
fn a_line_that_names_no_kind_or_two_is_refused_rather_than_guessed_at() {
    let p = project_with_sources(&[3000.0]);
    // Which side scored?
    assert_eq!(refusal(&p, "14:05 goal"), "no event word");
    assert_eq!(refusal(&p, "14:05"), "no event word");
    assert!(refusal(&p, "14:05 home away").contains("ambiguous"));
    // A stray word is more likely a misspelled side than noise.
    let stray = refusal(&p, "14:05 hme goal");
    assert!(
        stray.contains("ambiguous") && stray.contains("hme"),
        "{stray}"
    );
    // The same word twice is still one kind.
    assert_eq!(event(&p, "14:05 home home goal").kind, HOME);
}

// ------------------------------------------------------------- the team names

#[test]
fn a_team_name_names_its_side_even_in_two_words() {
    let mut p = project_with_sources(&[3000.0]);
    p.scoreboard = Some(scoreboard("Green Rovers", "City"));
    assert_eq!(event(&p, "14:05 city").kind, AWAY);
    assert_eq!(event(&p, "14:05 green rovers").kind, HOME);
    assert_eq!(event(&p, "14:05 Green Rovers scored").kind, HOME);
    // Only as a contiguous run: half a name is a stray word.
    assert!(refusal(&p, "14:05 green").contains("ambiguous"));
}

#[test]
fn team_names_are_ignored_when_one_contains_the_other() {
    let mut p = project_with_sources(&[3000.0]);
    p.scoreboard = Some(scoreboard("Rovers", "Rovers Reserves"));
    // A wrong side is a wrong scoreboard, so neither name is used at all.
    assert!(refusal(&p, "14:05 rovers").contains("ambiguous"));
    assert_eq!(event(&p, "14:05 home goal").kind, HOME);
}

// ----------------------------------------------------------- the video number

#[test]
fn a_leading_bare_integer_is_always_a_video_number() {
    let p = project_with_sources(&[3000.0, 1633.0]);
    assert_eq!(event(&p, "2 14:05 home goal").source_index, 1);
    // No number: the caller's default.
    match parse_line(&p, 1, "14:05 home goal") {
        LineVerdict::Event(e) => assert_eq!(e.source_index, 1),
        other => panic!("{other:?}"),
    }
    // Refused as the video it claims to be, and the misreading it might have
    // been (a time) is named rather than performed — on every number, not
    // only one big enough to look like seconds: `14` in a two-video project
    // is a coach who meant fourteen minutes.
    for (line, number) in [
        ("3 14:05 home goal", "3"),
        ("0 14:05 home goal", "0"),
        ("14 home goal", "14"),
        ("900 home goal", "900"),
    ] {
        let reason = refusal(&p, line);
        assert_eq!(
            reason,
            format!("there is no video {number} — a time needs a colon (15:00)"),
        );
    }

    // A number and nothing else is a video number with no time after it.
    assert_eq!(refusal(&p, "2 home goal"), "no time (use m:ss)");
    assert_eq!(refusal(&p, "2"), "no time (use m:ss)");
}

#[test]
fn a_time_past_the_end_of_its_video_is_refused_naming_the_length() {
    let p = project_with_sources(&[3000.0, 1633.0]);
    let reason = refusal(&p, "2 28:00 home goal");
    assert!(
        reason.contains("half 2") && reason.contains("27:13.0"),
        "{reason}"
    );
    // The last frame is in bounds.
    assert_eq!(event(&p, "2 27:13 home goal").source_index, 1);
}

// ------------------------------------------------------- the line, round-trip

#[test]
fn a_line_and_a_record_round_trip_through_each_other() {
    let p = project_with_sources(&[3000.0, 1633.0]);
    for (kind, text) in [
        (HOME, "2 14:05.0 home goal"),
        (AWAY, "2 14:05.0 away goal"),
        (START_STOP, "2 14:05.0 period"),
    ] {
        let stored = record(kind, 1, 845.0);
        assert_eq!(
            format_line(stored.kind, stored.source_index, stored.source_seconds),
            text
        );
        assert_eq!(
            event(&p, text),
            PendingMatchEvent {
                kind,
                source_index: 1,
                source_seconds: 845.0
            }
        );
    }
}

#[test]
fn changing_only_the_kind_keeps_the_stored_seconds() {
    let p = project_with_sources(&[3000.0]);
    let stored = record(HOME, 0, 14.06);
    let seed = format_line(stored.kind, stored.source_index, stored.source_seconds);
    assert_eq!(seed, "1 0:14.0 home goal");

    // The line displays floored tenths, so re-parsing it would re-round 14.06.
    let typed = "1 0:14.0 away goal";
    match edit_from_line(&p, 0, &seed, typed, stored.source_seconds) {
        LineVerdict::Event(e) => {
            assert_eq!(e.kind, AWAY);
            assert_eq!(e.source_seconds, 14.06);
        }
        other => panic!("{other:?}"),
    }

    // A time the coach actually retyped is the one that counts.
    match edit_from_line(&p, 0, &seed, "1 0:20.0 home goal", stored.source_seconds) {
        LineVerdict::Event(e) => assert_eq!(e.source_seconds, 20.0),
        other => panic!("{other:?}"),
    }
    // Even when it renders the same instant: the token is not byte-identical.
    match edit_from_line(&p, 0, &seed, "1 0:14 home goal", stored.source_seconds) {
        LineVerdict::Event(e) => assert_eq!(e.source_seconds, 14.0),
        other => panic!("{other:?}"),
    }
}

/// The row's field is the paste box's grammar, so every refusal the box makes
/// is a refusal it makes too — the kick-off words included, which are the
/// ones most likely to be in the coach's own notes.
#[test]
fn a_retyped_row_refuses_the_kick_off_words_as_the_paste_box_does() {
    let p = project_with_sources(&[3000.0]);
    let stored = record(HOME, 0, 845.0);
    let seed = format_line(stored.kind, stored.source_index, stored.source_seconds);
    for word in ["kickoff", "kick-off", "ko", "restart", "whistle"] {
        let typed = format!("1 14:05.0 {word}");
        match edit_from_line(&p, 0, &seed, &typed, stored.source_seconds) {
            LineVerdict::Refused(reason) => assert!(
                reason.contains("move every later period"),
                "{typed:?}: {reason}"
            ),
            other => panic!("{typed:?} should be refused, got {other:?}"),
        }
    }
}

/// Moving a row to another video **keeps the stored seconds**: the time token
/// is what the carry-over is decided on, and it hasn't changed. So an event
/// stored at 14.06 lands on the other video at 14.06, not at the 14.0 the
/// line shows.
#[test]
fn moving_a_row_to_another_video_carries_the_stored_seconds() {
    let p = project_with_sources(&[3000.0, 3000.0]);
    let stored = record(HOME, 0, 14.06);
    let seed = format_line(stored.kind, stored.source_index, stored.source_seconds);
    assert_eq!(seed, "1 0:14.0 home goal");

    match edit_from_line(&p, 0, &seed, "2 0:14.0 home goal", stored.source_seconds) {
        LineVerdict::Event(e) => {
            assert_eq!(e.source_index, 1);
            assert_eq!(e.source_seconds, 14.06);
        }
        other => panic!("{other:?}"),
    }
    // Retyping the time as well is a time the coach chose, on either video.
    match edit_from_line(&p, 0, &seed, "2 0:20.0 home goal", stored.source_seconds) {
        LineVerdict::Event(e) => {
            assert_eq!(e.source_index, 1);
            assert_eq!(e.source_seconds, 20.0);
        }
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------------ the batch

/// The kinds a batch added, in order.
fn added(batch: &video_coach_core::match_entry::Batch) -> Vec<f64> {
    batch.events.iter().map(|e| e.source_seconds).collect()
}

#[test]
fn a_pasted_block_echoes_every_line_in_input_order() {
    let p = project_with_sources(&[3000.0, 1633.0]);
    let batch = parse_batch(
        &p,
        0,
        "# the first half\n\
         3:20 home goal\n\
         \n\
         2 14:05 away goal\n\
         2 1405 home\n",
    );
    assert_eq!(batch.lines.len(), 3, "comments and blanks get no echo row");
    assert_eq!(batch.lines[0].number, 2);
    assert_eq!(batch.lines[0].text, "3:20 home goal");
    assert!(matches!(batch.lines[0].verdict, BatchVerdict::Added(_)));
    assert!(matches!(batch.lines[1].verdict, BatchVerdict::Added(_)));
    match &batch.lines[2].verdict {
        BatchVerdict::Refused(reason) => assert_eq!(reason, "no time (use m:ss)"),
        other => panic!("{other:?}"),
    }
    assert_eq!(added(&batch), vec![200.0, 845.0]);
    // Only the refused line comes back, for the coach to fix in place.
    assert_eq!(batch.leftover, "2 1405 home");
}

#[test]
fn a_line_already_tagged_is_skipped_within_a_second_either_side() {
    let mut p = project_with_sources(&[3000.0]);
    p.append_match_event(HOME, 0, 200.0);
    for line in ["3:19 home goal", "3:21 home goal", "3:20 home goal"] {
        let batch = parse_batch(&p, 0, line);
        assert!(
            matches!(batch.lines[0].verdict, BatchVerdict::AlreadyTagged(_)),
            "{line:?} is the same event"
        );
        assert!(batch.events.is_empty());
        // And it does not come back in the box: the event it names is in
        // the project already, so there is nothing to fix by editing it.
        assert_eq!(batch.leftover, "");
    }
    // Just outside, and on the other side of the kind, it is a new event.
    for line in ["3:18.9 home goal", "3:21.1 home goal", "3:20 away goal"] {
        let batch = parse_batch(&p, 0, line);
        assert!(
            matches!(batch.lines[0].verdict, BatchVerdict::Added(_)),
            "{line:?} is not the same event"
        );
    }
}

#[test]
fn the_same_line_twice_in_one_block_adds_once() {
    let p = project_with_sources(&[3000.0]);
    // The second copy is a duplicate of the first, which is not in the
    // project yet: the count is existing + accepted-so-far.
    let batch = parse_batch(&p, 0, "3:20 home goal\n3:20 home goal\n");
    assert_eq!(added(&batch), vec![200.0]);
    assert!(matches!(
        batch.lines[1].verdict,
        BatchVerdict::AlreadyTagged(_)
    ));
    assert_eq!(batch.leftover, "", "the block landed; nothing to fix");
}

#[test]
fn the_start_stop_cap_is_counted_across_the_batch_and_the_rest_still_lands() {
    let mut p = project_with_sources(&[6000.0]);
    p.scoreboard = Some(scoreboard("Rovers", "City"));
    p.append_match_event(START_STOP, 0, 10.0);

    // Soccer has four places; one is taken, so three of these four land.
    let batch = parse_batch(
        &p,
        0,
        "20:00 start\n\
         30:00 home goal\n\
         40:00 end\n\
         50:00 start\n\
         60:00 end\n",
    );
    assert_eq!(added(&batch), vec![1200.0, 1800.0, 2400.0, 3000.0]);
    match &batch.lines[4].verdict {
        BatchVerdict::Refused(reason) => assert!(reason.contains("already tagged"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(batch.leftover, "60:00 end");
}
