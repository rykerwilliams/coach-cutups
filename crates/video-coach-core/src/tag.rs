//! Tag normalization.

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
