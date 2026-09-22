//! Text the window shows, kept out of the UI code so it's tested headless.

use gstreamer::glib;

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

/// [`format_hms`] with tenths, `M:SS.t` or `H:MM:SS.t`, for the readout
/// while paused: a frame step moves the tenths. Floored like it, so a frame
/// at 12:34.99 never reads 12:35.0.
pub fn format_hms_tenths(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00.0".into();
    }
    // In integer tenths, so the floor can't disagree with `format_hms`'s.
    let tenths = (seconds * 10.0).floor() as u64;
    format!("{}.{}", format_hms((tenths / 10) as f64), tenths % 10)
}

/// When an export with `seconds_left` still to render ends, as the sheet's
/// one line of estimate reads it: `"Finishes at 3:42 PM"` (spec E5, E8).
///
/// A clock time rather than a countdown: it is the only rendering of the
/// estimate, and a wall-clock time stays true while the user is away from the
/// window. `None` for an estimate that isn't a time.
pub fn finish_at(seconds_left: f64) -> Option<String> {
    finish_from(&glib::DateTime::now_local().ok()?, seconds_left)
}

/// [`finish_at`] from a given moment, so a test can pin the clock.
fn finish_from(now: &glib::DateTime, seconds_left: f64) -> Option<String> {
    if !seconds_left.is_finite() || seconds_left < 0.0 {
        return None;
    }
    let at = now.add_seconds(seconds_left.round()).ok()?;
    // `%p` is empty where the day isn't halved, and `%-l` would then read
    // 15:42 as "3:42": the 24-hour clock is what those locales read.
    let text = match at.format("%p").is_ok_and(|half| half.is_empty()) {
        true => at.format("%H:%M").ok()?,
        false => at.format("%-l:%M %p").ok()?,
    };
    Some(format!("Finishes at {text}"))
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
    fn format_hms_tenths_floors_like_format_hms() {
        assert_eq!(format_hms_tenths(754.99), "12:34.9");
        assert_eq!(format_hms_tenths(754.0), "12:34.0");
        assert_eq!(format_hms_tenths(3723.45), "1:02:03.4");
        assert_eq!(format_hms_tenths(0.05), "0:00.0");
        assert_eq!(format_hms_tenths(0.0), "0:00.0");
        assert_eq!(format_hms_tenths(-5.0), "0:00.0");
        assert_eq!(format_hms_tenths(f64::NAN), "0:00.0");
        assert_eq!(format_hms_tenths(f64::INFINITY), "0:00.0");
    }

    /// The clock's shape is the locale's, so both renderings are accepted:
    /// what's pinned here is the arithmetic and the refusals.
    #[test]
    fn a_finish_time_is_the_local_clock_time_the_run_ends_at() {
        let at = |seconds_left| {
            let now = glib::DateTime::from_local(2026, 9, 20, 15, 30, 0.0).unwrap();
            finish_from(&now, seconds_left)
        };
        let text = at(720.0).unwrap();
        assert!(
            text == "Finishes at 3:42 PM" || text == "Finishes at 15:42",
            "{text}"
        );
        assert_eq!(at(0.0), at(29.0));
        assert_eq!(at(f64::NAN), None);
        assert_eq!(at(f64::INFINITY), None);
        assert_eq!(at(-1.0), None);
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
