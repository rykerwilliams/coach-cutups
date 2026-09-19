//! Tag summaries (ported from `TagAggregationTests.swift`) and tag-field
//! suggestions (new: macOS computed them inline in `TagField`).

use uuid::Uuid;

use video_coach_core::project::Clip;
use video_coach_core::tag::{tag_suggestions, tag_summaries, TagSummary, MAX_SUGGESTIONS};

fn clip(tags: &[&str], duration: f64) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: String::new(),
        notes: String::new(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        source_index: 0,
        start_source_seconds: 0.0,
        recording_duration: duration,
        recording_filename: "x.mkv".into(),
        events: Vec::new(),
        show_pip: true,
        sort_index: 0,
        created_at: String::new(),
        transcript: String::new(),
    }
}

fn summaries(tags: &[&str]) -> Vec<TagSummary> {
    tags.iter()
        .map(|t| TagSummary {
            tag: t.to_string(),
            clip_count: 1,
            total_seconds: 1.0,
        })
        .collect()
}

// ----------------------------------------------------------- tag_summaries

/// Ported: `test_aggregatesByTag_withCountAndDuration`.
#[test]
fn summaries_count_clips_and_sum_durations() {
    let s = tag_summaries(&[
        clip(&["attacking-chance", "wing"], 4.0),
        clip(&["attacking-chance"], 6.0),
        clip(&["transitions"], 3.0),
    ]);
    assert_eq!(
        s,
        [
            TagSummary {
                tag: "attacking-chance".into(),
                clip_count: 2,
                total_seconds: 10.0,
            },
            TagSummary {
                tag: "transitions".into(),
                clip_count: 1,
                total_seconds: 3.0,
            },
            TagSummary {
                tag: "wing".into(),
                clip_count: 1,
                total_seconds: 4.0,
            },
        ]
    );
}

/// Ported: `test_isAlphabeticallySorted`.
#[test]
fn summaries_are_alphabetical() {
    let s = tag_summaries(&[
        clip(&["zebra"], 1.0),
        clip(&["alpha"], 1.0),
        clip(&["mango"], 1.0),
    ]);
    let tags: Vec<_> = s.iter().map(|s| s.tag.as_str()).collect();
    assert_eq!(tags, ["alpha", "mango", "zebra"]);
}

#[test]
fn no_tags_no_summaries() {
    assert!(tag_summaries(&[]).is_empty());
    assert!(tag_summaries(&[clip(&[], 5.0)]).is_empty());
}

// --------------------------------------------------------- tag_suggestions

#[test]
fn suggests_prefix_matches_sorted() {
    let all = summaries(&["wing", "attack", "wide", "set piece"]);
    assert_eq!(tag_suggestions(&all, "wi"), ["wide", "wing"]);
    assert_eq!(
        tag_suggestions(&all, "  WI "),
        ["wide", "wing"],
        "normalized"
    );
    assert!(tag_suggestions(&all, "x").is_empty());
}

#[test]
fn matches_the_fragment_after_the_last_comma() {
    let all = summaries(&["wing", "attack", "wide"]);
    assert_eq!(tag_suggestions(&all, "attack, wi"), ["wide", "wing"]);
    assert_eq!(tag_suggestions(&all, "wing, at"), ["attack"]);
}

#[test]
fn an_empty_fragment_suggests_nothing() {
    let all = summaries(&["wing"]);
    assert!(tag_suggestions(&all, "").is_empty());
    assert!(tag_suggestions(&all, "attack, ").is_empty());
}

/// Tags already in the field are not suggested again, and neither is a
/// fragment that already names a tag exactly.
#[test]
fn excludes_tags_already_in_the_text_and_exact_matches() {
    let all = summaries(&["wing", "wingback", "wide"]);
    assert_eq!(tag_suggestions(&all, "Wing, wi"), ["wide", "wingback"]);
    assert_eq!(tag_suggestions(&all, "wing"), ["wingback"]);
}

#[test]
fn caps_the_suggestions() {
    let tags: Vec<String> = (0..MAX_SUGGESTIONS + 3)
        .map(|i| format!("t{i:02}"))
        .collect();
    let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
    let mut all = summaries(&tags);
    all.reverse();
    let s = tag_suggestions(&all, "t");
    assert_eq!(s.len(), MAX_SUGGESTIONS);
    assert_eq!(s[0], "t00", "sorted before the cap");
}
