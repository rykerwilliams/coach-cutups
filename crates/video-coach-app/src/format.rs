//! Text the window shows, kept out of the UI code so it's tested headless.

/// Seconds as `H:MM:SS` when there are hours, else `M:SS`, floored; `0:00`
/// for anything non-finite or not positive. macOS `formatDurationHMS`.
pub fn format_hms(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00".into();
    }
    // Saturates for absurd values rather than wrapping.
    let total = seconds.floor() as u64;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `message` with its first letter capitalized, for showing a
/// [`UserError`](crate::bus::UserError)'s `Display` text as a sentence.
pub fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_hms_matches_macos() {
        assert_eq!(format_hms(0.0), "0:00");
        assert_eq!(format_hms(-5.0), "0:00");
        assert_eq!(format_hms(f64::NAN), "0:00");
        assert_eq!(format_hms(f64::INFINITY), "0:00");
        assert_eq!(format_hms(59.9), "0:59");
        assert_eq!(format_hms(3599.9), "59:59");
        assert_eq!(format_hms(3600.0), "1:00:00");
        assert_eq!(format_hms(1242.17), "20:42");
    }

    #[test]
    fn sentence_capitalizes_the_first_letter() {
        assert_eq!(
            sentence("the file has no video stream"),
            "The file has no video stream"
        );
        assert_eq!(sentence(""), "");
    }
}
