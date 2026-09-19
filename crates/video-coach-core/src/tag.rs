//! Tags: normalization, the overview's summaries, and the tag field's
//! suggestions.

use std::collections::BTreeMap;

use crate::project::Clip;

/// The most suggestions [`tag_suggestions`] returns.
pub const MAX_SUGGESTIONS: usize = 8;

/// One row of the tag overview.
#[derive(Debug, Clone, PartialEq)]
pub struct TagSummary {
    pub tag: String,
    pub clip_count: usize,
    /// Sum of the tagged clips' recording durations.
    pub total_seconds: f64,
}

/// Split a comma-separated tag string into normalized tags.
///
/// Trims whitespace, lowercases, drops empty fragments, and de-duplicates
/// **preserving first-seen order**.
pub fn normalize_tags(input: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for fragment in input.split(',') {
        let trimmed = fragment.trim().to_lowercase();
        if trimmed.is_empty() || seen.contains(&trimmed) {
            continue;
        }
        seen.push(trimmed);
    }
    seen
}

/// Every tag in `clips` with its clip count and total length, sorted
/// alphabetically. Ported from macOS `TagAggregation.aggregate`.
pub fn tag_summaries(clips: &[Clip]) -> Vec<TagSummary> {
    let mut by_tag: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
    for clip in clips {
        for tag in &clip.tags {
            let entry = by_tag.entry(tag).or_default();
            entry.0 += 1;
            entry.1 += clip.recording_duration;
        }
    }
    by_tag
        .into_iter()
        .map(|(tag, (clip_count, total_seconds))| TagSummary {
            tag: tag.to_owned(),
            clip_count,
            total_seconds,
        })
        .collect()
}

/// Existing tags to suggest while the tag field holds `text`.
///
/// The fragment after the last comma, trimmed and lowercased, is matched as a
/// prefix. Tags already in `text` (normalized, which includes an exact match
/// of the fragment) are excluded, as on macOS. At most [`MAX_SUGGESTIONS`],
/// sorted. An empty fragment suggests nothing.
pub fn tag_suggestions(summaries: &[TagSummary], text: &str) -> Vec<String> {
    let fragment = text.rsplit(',').next().unwrap_or("").trim().to_lowercase();
    if fragment.is_empty() {
        return Vec::new();
    }
    let present = normalize_tags(text);
    let mut out: Vec<String> = summaries
        .iter()
        .map(|s| &s.tag)
        .filter(|t| t.starts_with(&fragment) && !present.contains(t))
        .cloned()
        .collect();
    out.sort();
    out.truncate(MAX_SUGGESTIONS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_trims_and_lowercases() {
        assert_eq!(
            normalize_tags(" Transition , SET PIECE "),
            ["transition", "set piece"]
        );
    }

    #[test]
    fn drops_empty_fragments() {
        assert_eq!(normalize_tags("a,,  ,b"), ["a", "b"]);
    }

    #[test]
    fn dedupes_preserving_first_seen_order() {
        assert_eq!(normalize_tags("b, a, B, A, c"), ["b", "a", "c"]);
    }

    #[test]
    fn single_untagged_string_is_one_tag() {
        assert_eq!(normalize_tags("shot"), ["shot"]);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(normalize_tags("").is_empty());
        assert!(normalize_tags("  , ,").is_empty());
    }
}
