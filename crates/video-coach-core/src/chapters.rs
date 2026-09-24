//! The file's chapters as a block of text to paste into a YouTube
//! description.
//!
//! A YouTube upload ignores the chapters inside an MP4 — the `chpl` box
//! [`crate::plan::CompilationPlan::chapters`] is written into
//! (`video-coach-media`'s `chapters` module) is read by `ffprobe`, mpv and
//! VLC and by nothing else. What YouTube reads is the description, so the
//! same list goes beside the file as plain text the coach can paste.
//!
//! **YouTube is all or nothing.** Break any one of its rules and it silently
//! shows no chapters at all, leaving a description full of stray timestamps
//! and no way to tell what went wrong. So the rules are enforced here rather
//! than hoped for:
//!
//! - the first line is exactly `0:00`;
//! - there are at least three lines;
//! - consecutive lines are at least ten seconds apart, ascending;
//! - a line is `m:ss` or `h:mm:ss`, then a space, then the title.
//!
//! The app's own chapters are a coach's moments, not a list built to those
//! rules, so [`chapter_list`] is where the two are reconciled. Like
//! [`crate::cues`] and [`crate::metadata`] it is pure: media writes the
//! string it returns and knows nothing about the format.

use crate::metadata::UNTITLED;

/// The shortest gap YouTube accepts between two chapters.
pub const MIN_GAP_SECONDS: u64 = 10;

/// The fewest chapters YouTube accepts.
pub const MIN_CHAPTERS: usize = 3;

/// What the chapter prepended at zero is called, when the first real chapter
/// is too far in to be moved there ([`chapter_list`]).
pub const LEAD_IN: &str = "Start";

/// One chapter's time as YouTube writes it: `m:ss` under an hour, `h:mm:ss`
/// from an hour on. Zero is `0:00`, which is the first line's required form.
fn timestamp(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    match h {
        0 => format!("{m}:{s:02}"),
        _ => format!("{h}:{m:02}:{s:02}"),
    }
}

/// A title as one line: runs of whitespace — including the newlines a clip
/// name can hold — collapsed to single spaces, and a title left empty named
/// [`UNTITLED`].
///
/// **A newline in a title would cost every chapter**, not just its own: the
/// line after it wouldn't start with a timestamp, and YouTube would drop the
/// whole list. A title is also never empty, because a timestamp alone is not
/// a chapter.
fn one_line(title: &str) -> String {
    let words: Vec<&str> = title.split_whitespace().collect();
    match words.is_empty() {
        true => UNTITLED.to_owned(),
        false => words.join(" "),
    }
}

/// `chapters` as a YouTube description block, or `None` when too few of them
/// survive the rules to make a list YouTube would read.
///
/// The input is [`crate::plan::CompilationPlan::chapters`]: `(start in output
/// seconds, title)`, in order. The three awkward cases and what happens:
///
/// **A first chapter that isn't at zero.** Under ten seconds in, it is moved
/// to `0:00` — it is the opening chapter either way, and inventing a line
/// above it would throw away the words the coach wrote for the thing that
/// starts the film. Ten seconds or more in, a `0:00 `[`LEAD_IN`] line is
/// prepended instead, which names the stretch before the first moment rather
/// than mislabelling it.
///
/// **A chapter inside ten seconds of the one before it is dropped, not
/// merged.** Merging would invent a title ("Kick-off / Rovers goal 1-0") that
/// is neither of the two and that nothing else in the app would ever write;
/// dropping keeps every line exactly as worded. It is the **later** one that
/// goes, so a kept timestamp never lands after the start of the moment it
/// names. The same comparison drops a chapter that is out of order, so the
/// list is ascending whatever it is handed.
///
/// **Fewer than three survivors write no file at all** (the caller treats
/// `None` as "remove the stale one"). A two-line list is not a shorter list
/// of chapters, it is a list YouTube ignores: pasting it gives a description
/// with loose timestamps in it and no chapters, and no hint why. A file that
/// only exists when it works is the one that can be pasted without reading
/// it.
///
/// Times are floored to whole seconds, because a timestamp is a seek target:
/// `0:14` must not land after the moment `14.9 s` names. A non-finite or
/// negative time — which no plan produces — is skipped rather than guessed.
pub fn chapter_list(chapters: &[(f64, String)]) -> Option<String> {
    let mut kept: Vec<(u64, String)> = Vec::with_capacity(chapters.len() + 1);
    for (at, title) in chapters {
        if !at.is_finite() || *at < 0.0 {
            continue;
        }
        // `as` saturates, so nothing here wraps however long the film.
        let seconds = at.floor() as u64;
        match kept.last() {
            Some((last, _)) => {
                if seconds >= last.saturating_add(MIN_GAP_SECONDS) {
                    kept.push((seconds, one_line(title)));
                }
            }
            None if seconds >= MIN_GAP_SECONDS => {
                kept.push((0, LEAD_IN.to_owned()));
                kept.push((seconds, one_line(title)));
            }
            // At zero already, or near enough to be moved there.
            None => kept.push((0, one_line(title))),
        }
    }
    if kept.len() < MIN_CHAPTERS {
        return None;
    }
    Some(
        kept.iter()
            .map(|(at, title)| format!("{} {title}\n", timestamp(*at)))
            .collect(),
    )
}
