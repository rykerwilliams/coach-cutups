//! The pasteable chapter list: YouTube's rules, and what the app's own
//! chapters are bent into to meet them.

use video_coach_core::chapters::chapter_list;

/// A chapter list from `(seconds, title)` pairs.
fn list(chapters: &[(f64, &str)]) -> Option<String> {
    let owned: Vec<(f64, String)> = chapters
        .iter()
        .map(|(at, title)| (*at, (*title).to_owned()))
        .collect();
    chapter_list(&owned)
}

/// The shape a real match produces: a kick-off at zero, goals, half time.
#[test]
fn a_match_reads_as_youtube_wants_it() {
    assert_eq!(
        list(&[
            (0.0, "Kick-off"),
            (845.4, "Rovers goal 1-0"),
            (1651.9, "Half time"),
        ])
        .unwrap(),
        "0:00 Kick-off\n14:05 Rovers goal 1-0\n27:31 Half time\n"
    );
}

/// A time is floored, never rounded: a timestamp is a seek target, and `0:15`
/// for a goal at 14.9 s lands after the moment it names.
#[test]
fn times_are_floored_to_the_second() {
    let text = list(&[(0.0, "a"), (14.9, "b"), (30.999, "c")]).unwrap();
    assert_eq!(text, "0:00 a\n0:14 b\n0:30 c\n");
}

/// Under ten seconds in, the first chapter is moved to zero rather than
/// having a lead-in invented above it: it is the opening chapter either way,
/// and its own words are better than "Start".
#[test]
fn a_first_chapter_just_after_zero_is_moved_to_zero() {
    assert_eq!(
        list(&[(4.0, "Kick-off"), (60.0, "Goal"), (120.0, "Half time")]).unwrap(),
        "0:00 Kick-off\n1:00 Goal\n2:00 Half time\n"
    );
    // Nine seconds is still the opening chapter; the count is unchanged.
    assert_eq!(
        list(&[(9.99, "Kick-off"), (60.0, "Goal"), (120.0, "Half time")])
            .unwrap()
            .lines()
            .count(),
        3
    );
}

/// Ten seconds or more in, a chapter at zero is prepended instead — moving
/// the first one that far would put its timestamp before the moment it names.
#[test]
fn a_first_chapter_well_after_zero_gets_a_lead_in() {
    assert_eq!(
        list(&[(10.0, "Kick-off"), (60.0, "Goal"), (120.0, "Half time")]).unwrap(),
        "0:00 Start\n0:10 Kick-off\n1:00 Goal\n2:00 Half time\n"
    );
}

/// Inside ten seconds of the one before it, a chapter is dropped, not merged:
/// merging would invent a title neither of them has. The **later** one goes,
/// so a kept timestamp never lands after the moment it names.
#[test]
fn a_chapter_four_seconds_after_the_last_is_dropped() {
    assert_eq!(
        list(&[
            (0.0, "Kick-off"),
            (4.0, "Scramble"),
            (60.0, "Goal"),
            (120.0, "Half time"),
        ])
        .unwrap(),
        "0:00 Kick-off\n1:00 Goal\n2:00 Half time\n"
    );
}

/// The gap is measured from the chapter that was **kept**, not from the one
/// that was dropped — which is what YouTube measures, since the dropped one
/// is not in the list. `0:16` is 16 s after the line above it and would be
/// only 8 s after the line that went.
#[test]
fn the_gap_is_measured_from_the_last_kept_chapter() {
    assert_eq!(
        list(&[(0.0, "a"), (8.0, "b"), (16.0, "c"), (30.0, "d")]).unwrap(),
        "0:00 a\n0:16 c\n0:30 d\n"
    );
}

/// Exactly ten seconds is far enough apart — YouTube's rule is "at least".
#[test]
fn ten_seconds_apart_is_far_enough() {
    assert_eq!(
        list(&[(0.0, "a"), (10.0, "b"), (20.0, "c")]).unwrap(),
        "0:00 a\n0:10 b\n0:20 c\n"
    );
}

/// Fewer than three survivors is no file at all: a two-line list is not a
/// shorter list of chapters, it is a list YouTube ignores, leaving loose
/// timestamps in the description and no hint why.
#[test]
fn fewer_than_three_chapters_writes_nothing() {
    assert_eq!(list(&[]), None);
    assert_eq!(list(&[(0.0, "Kick-off")]), None);
    assert_eq!(list(&[(0.0, "Kick-off"), (60.0, "Half time")]), None);
    // Three chapters, but two of them collapse into one.
    assert_eq!(list(&[(0.0, "a"), (3.0, "b"), (6.0, "c")]), None);
}

/// Past an hour the timestamps grow a field, and the minutes are padded there
/// where they are not in `m:ss`. A whole match crosses it every time.
#[test]
fn an_hour_long_list_crosses_into_h_mm_ss() {
    assert_eq!(
        list(&[
            (0.0, "Kick-off"),
            (3599.0, "Late goal"),
            (3610.0, "Half time"),
            (3665.0, "Second half"),
            (7384.0, "Full time"),
        ])
        .unwrap(),
        "0:00 Kick-off\n\
         59:59 Late goal\n\
         1:00:10 Half time\n\
         1:01:05 Second half\n\
         2:03:04 Full time\n"
    );
}

/// Titles are the coach's own words, in whatever alphabet: they are copied
/// through untouched.
#[test]
fn non_ascii_titles_are_carried_through() {
    assert_eq!(
        list(&[
            (0.0, "Anstoß"),
            (30.0, "Café press — Rovers 1-0"),
            (90.0, "Δεύτερο ημίχρονο"),
            (150.0, "後半"),
        ])
        .unwrap(),
        "0:00 Anstoß\n\
         0:30 Café press — Rovers 1-0\n\
         1:30 Δεύτερο ημίχρονο\n\
         2:30 後半\n"
    );
}

/// A newline in a clip name would cost **every** chapter, not just its own:
/// the line after it wouldn't start with a timestamp and YouTube would drop
/// the whole list. Runs of whitespace collapse to one space.
#[test]
fn a_title_is_flattened_to_one_line() {
    assert_eq!(
        list(&[
            (0.0, "two\nlines"),
            (30.0, "  padded \t out  "),
            (60.0, "c"),
        ])
        .unwrap(),
        "0:00 two lines\n0:30 padded out\n1:00 c\n"
    );
}

/// A timestamp on its own is not a chapter, so a title with no words in it is
/// named rather than left blank.
#[test]
fn an_empty_title_is_named() {
    assert_eq!(
        list(&[(0.0, ""), (30.0, "   "), (60.0, "c")]).unwrap(),
        "0:00 Untitled\n0:30 Untitled\n1:00 c\n"
    );
}

/// No plan produces these, but the list must be ascending whatever it is
/// handed: the same comparison that enforces the gap drops them.
#[test]
fn out_of_order_and_nonsense_times_are_dropped() {
    assert_eq!(
        list(&[
            (0.0, "a"),
            (f64::NAN, "nan"),
            (-5.0, "before the start"),
            (30.0, "b"),
            (20.0, "backwards"),
            (60.0, "c"),
        ])
        .unwrap(),
        "0:00 a\n0:30 b\n1:00 c\n"
    );
}
