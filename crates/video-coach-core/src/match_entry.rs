//! One line per match event, typed or pasted — the grammar the editor's row
//! field and its paste box both parse with.
//!
//! One grammar, one parser: the mark that says "this field is good" and the
//! parse that builds the command are the same call, so they cannot drift
//! apart. The app owns the wording around a verdict (the glyphs, the summary
//! line); every verdict itself is decided here, where it is tested with no
//! GStreamer.
//!
//! A line is `[<video>] <time> <words>`:
//!
//! ```text
//! 2 14:05.0 home goal
//! 3:20 away          # the default video
//! # a comment, ignored
//! ```
//!
//! **A time has a colon, and a leading bare integer is a video number** — a
//! bare `14` is fourteen seconds to a computer and fourteen minutes to a
//! coach, and this grammar's whole job is to be exactly the number the coach
//! meant.

use crate::project::Project;
use crate::scoreboard::{MatchEventKind, ScoreboardConfig, START_STOP_CAP_REFUSAL};

/// How near an existing event of the same kind on the same source a *pasted*
/// line has to be to count as the same event, in seconds.
///
/// The list itself allows duplicates — two events at one instant are legal and
/// keep their stored order — but a paste box's input is a block the coach may
/// well paste twice. Two genuine goals inside one second do not happen; a
/// doubled paste does, and a doubled line inside one paste does too.
pub const SAME_EVENT_SECONDS: f64 = 1.0;

/// An event the coach typed, ready for the bus to append. No id yet: the
/// project mints one when it lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingMatchEvent {
    pub kind: MatchEventKind,
    pub source_index: usize,
    pub source_seconds: f64,
}

/// What one line means.
#[derive(Debug, Clone, PartialEq)]
pub enum LineVerdict {
    /// Blank, or a comment and nothing else. It gets no echo row at all.
    Nothing,
    Event(PendingMatchEvent),
    /// A sentence for the coach, already naming what is wrong.
    Refused(String),
}

// ------------------------------------------------------------------- time

/// `m:ss`, `mm:ss` or `h:mm:ss`, each optionally with a fraction
/// (`14:05.5`), into seconds. `None` for anything else.
///
/// The leading field is unbounded — `75:20` is 4520 s in an 80-minute file —
/// and every field after it is exactly two digits under 60, so `14:5:3` is
/// refused rather than guessed at. **A colon is required** — see the module
/// doc for why.
pub fn parse_time(text: &str) -> Option<f64> {
    let text = text.trim();
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };
    let fraction = match fraction {
        None => 0.0,
        Some(f) if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) => {
            format!("0.{f}").parse().ok()?
        }
        Some(_) => return None,
    };

    let mut fields = whole.split(':');
    let leading: u64 = digits(fields.next()?, None)?;
    let mut seconds = leading as f64;
    let mut count = 1;
    for field in fields {
        let value = digits(field, Some(2))?;
        if value >= 60 {
            return None;
        }
        seconds = seconds * 60.0 + value as f64;
        count += 1;
    }
    // Two fields are m:ss, three are h:mm:ss. One is a bare number.
    (2..=3).contains(&count).then_some(seconds + fraction)
}

/// `field` as a number, optionally required to be exactly `width` digits.
fn digits(field: &str, width: Option<usize>) -> Option<u64> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if matches!(width, Some(w) if field.len() != w) {
        return None;
    }
    field.parse().ok()
}

/// Seconds as `M:SS.t` or `H:MM:SS.t`, floored in integer tenths.
///
/// A deliberate duplicate of the app's `format::format_hms_tenths`, which
/// renders the same shapes the same way: that module imports `gstreamer::glib`
/// and this crate declares no media dependency, and the grammar needs its own
/// because it builds the line it seeds the row's field with. A test in the app
/// crate pins the two together.
pub fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0:00.0".into();
    }
    // In integer tenths, so the floor can't disagree with `format_hms`'s.
    let tenths = (seconds * 10.0).floor() as u64;
    let (whole, t) = (tenths / 10, tenths % 10);
    let (h, m, s) = (whole / 3600, whole % 3600 / 60, whole % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{t}")
    } else {
        format!("{m}:{s:02}.{t}")
    }
}

// ------------------------------------------------------------- vocabulary

/// Words that are refused with a reason of their own, never mapped to a kind.
///
/// `kickoffs.txt` is *by definition* a list of post-goal restarts, and a
/// restart is not a stored event kind. `interpret` is positional, so a
/// spurious start/stop does not add a stray row — it shifts every period
/// boundary after it, and the match clock burned into every export with it.
/// `whistle` joins them because a whistle is as often a foul as a boundary.
const REFUSED_WORDS: [&str; 5] = ["kickoff", "kick", "ko", "restart", "whistle"];

/// Words that carry no kind and are not a mistake either: the filler in
/// "home goal", "the ht", "rovers scored".
const IGNORED_WORDS: [&str; 5] = ["goal", "goals", "at", "the", "scored"];

const KICK_OFF_REASON: &str = "a restart after a goal isn't a stored event, and reading it as a \
                               period boundary would move every later period. Only a period's own \
                               start or end is tagged: use `start`, `end`, `ht` or `ft`";

/// The kind one word names. `z`, `x` and `v` are the app's own three tagging
/// keys, so the vocabulary starts from what the coach's fingers already know.
fn word_kind(token: &str) -> Option<MatchEventKind> {
    Some(match token {
        "home" | "hg" | "z" => MatchEventKind::HomeGoal,
        "away" | "ag" | "x" => MatchEventKind::AwayGoal,
        "v" | "start" | "stop" | "end" | "period" | "half" | "ht" | "ft" | "fulltime"
        | "halftime" => MatchEventKind::StartStop,
        _ => return None,
    })
}

/// Lowercased words, with `-`, `_` and `,` read as spaces and runs of space
/// collapsed — so `kick-off`, `Home, goal` and `HOME  GOAL` all normalize.
fn tokens(text: &str) -> Vec<String> {
    text.chars()
        .map(|c| match c {
            '-' | '_' | ',' => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect()
}

/// The two team names as token runs, or `None` when they can't be told apart.
///
/// A name is matched as a **contiguous run**, so a two-word name works. When
/// one normalized name equals or contains the other the match is ambiguous,
/// and a wrong side is a wrong scoreboard — so neither is used at all.
fn team_tokens(config: Option<&ScoreboardConfig>) -> Option<(Vec<String>, Vec<String>)> {
    let config = config?;
    let (home, away) = (tokens(&config.home.name), tokens(&config.away.name));
    if home.is_empty() || away.is_empty() {
        return None;
    }
    let (h, a) = (home.join(" "), away.join(" "));
    (!h.contains(&a) && !a.contains(&h)).then_some((home, away))
}

/// Which kind the words after the time name, or why they name none.
///
/// Exactly one distinct kind wins; none is "no event word"; two or more is
/// ambiguous, as is any word that is neither vocabulary nor filler — a stray
/// word is more likely a misspelled side than noise.
fn parse_kind(
    words: &[String],
    config: Option<&ScoreboardConfig>,
) -> Result<MatchEventKind, String> {
    let teams = team_tokens(config);
    let mut kinds: Vec<MatchEventKind> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let word = &words[i];
        if REFUSED_WORDS.contains(&word.as_str()) {
            return Err(KICK_OFF_REASON.into());
        }
        let team_run = teams.as_ref().and_then(|(home, away)| {
            let run = |name: &Vec<String>| words[i..].starts_with(name).then_some(name.len());
            run(home)
                .map(|n| (MatchEventKind::HomeGoal, n))
                .or_else(|| run(away).map(|n| (MatchEventKind::AwayGoal, n)))
        });
        if let Some((kind, len)) = team_run {
            push_kind(&mut kinds, kind);
            i += len;
            continue;
        }
        match word_kind(word) {
            Some(kind) => push_kind(&mut kinds, kind),
            None if IGNORED_WORDS.contains(&word.as_str()) => {}
            None => return Err(format!("ambiguous — \"{word}\" isn't an event word")),
        }
        i += 1;
    }
    match kinds.len() {
        1 => Ok(kinds[0]),
        0 => Err("no event word".into()),
        _ => Err("ambiguous — more than one kind on the line".into()),
    }
}

fn push_kind(kinds: &mut Vec<MatchEventKind>, kind: MatchEventKind) {
    if !kinds.contains(&kind) {
        kinds.push(kind);
    }
}

// ------------------------------------------------------------------ lines

/// One typed line, against `project`, for a line with no video number of its
/// own to belong to `default_source` (0-based).
pub fn parse_line(project: &Project, default_source: usize, line: &str) -> LineVerdict {
    parse_line_with(project, default_source, line, None)
}

/// The canonical rendering of an event as a line: what the row's field is
/// seeded with, and what [`parse_line`] reads back into the same event.
///
/// The start/stop word is the neutral `period`: a stored start/stop does not
/// know whether it opens or closes a half — `interpret` decides that from the
/// order — so the line cannot say.
pub fn format_line(kind: MatchEventKind, source_index: usize, source_seconds: f64) -> String {
    let kind = match kind {
        MatchEventKind::HomeGoal => "home goal",
        MatchEventKind::AwayGoal => "away goal",
        MatchEventKind::StartStop => "period",
    };
    format!(
        "{} {} {kind}",
        source_index + 1,
        format_time(source_seconds)
    )
}

/// A row's field committed: `typed`, against the `seed` the field was filled
/// with by [`format_line`] and the row's `stored_seconds`.
///
/// **When the time token is byte-identical to the seed's, the stored seconds
/// are kept**, not what `parse_time` makes of the text. A coach who selects a
/// row to change `home goal` to `away goal` would otherwise silently re-round
/// a stored `14.06` to `14.0`, because the line displays floored tenths.
pub fn edit_from_line(
    project: &Project,
    default_source: usize,
    seed: &str,
    typed: &str,
    stored_seconds: f64,
) -> LineVerdict {
    let unchanged = time_token(seed).is_some() && time_token(seed) == time_token(typed);
    let keep = unchanged.then_some(stored_seconds);
    parse_line_with(project, default_source, typed, keep)
}

/// The line's time as it was typed — the token after an optional leading
/// video number.
fn time_token(line: &str) -> Option<&str> {
    let mut fields = body(line).split_whitespace();
    let first = fields.next()?;
    if is_video_number(first) {
        fields.next()
    } else {
        Some(first)
    }
}

/// The line without its comment, trimmed. `#` runs to the end of the line,
/// which is `kickoffs.txt`'s own convention.
fn body(line: &str) -> &str {
    line.split('#').next().unwrap_or("").trim()
}

/// A leading bare integer is **always** a video number (the module doc).
fn is_video_number(field: &str) -> bool {
    !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit())
}

/// [`parse_line`], optionally overriding the seconds the time text parsed to
/// (see [`edit_from_line`]). The override is applied *before* the duration
/// bound, so nothing can carry a stored time past the end of a source.
fn parse_line_with(
    project: &Project,
    default_source: usize,
    line: &str,
    seconds_override: Option<f64>,
) -> LineVerdict {
    let body = body(line);
    if body.is_empty() {
        return LineVerdict::Nothing;
    }
    let mut fields = body.split_whitespace();
    let mut next = fields.next();

    let mut source_index = default_source;
    if let Some(first) = next.filter(|f| is_video_number(f)) {
        // Out of range it is refused as the video it is, rather than being
        // reinterpreted as anything else.
        match first.parse::<usize>() {
            Ok(n) if (1..=project.source_videos.len()).contains(&n) => source_index = n - 1,
            _ => return refuse_video(first),
        }
        next = fields.next();
    }
    let Some(source) = project.source_videos.get(source_index) else {
        return refuse_video(&(source_index + 1).to_string());
    };

    let Some(source_seconds) = next.and_then(parse_time) else {
        return LineVerdict::Refused("no time (use m:ss)".into());
    };
    let source_seconds = seconds_override.unwrap_or(source_seconds);

    let words = tokens(&fields.collect::<Vec<_>>().join(" "));
    let kind = match parse_kind(&words, project.scoreboard.as_ref()) {
        Ok(kind) => kind,
        Err(reason) => return LineVerdict::Refused(reason),
    };

    // `duration_seconds` is the duration authority, so the bound is exact.
    // Refused naming the length, never clamped: a clamped goal is a wrong
    // timestamp that looks right, and one past the end can never be seen.
    if source_seconds > source.duration_seconds {
        return LineVerdict::Refused(format!(
            "{} is {} long",
            source.display_name,
            format_time(source.duration_seconds)
        ));
    }
    LineVerdict::Event(PendingMatchEvent {
        kind,
        source_index,
        source_seconds,
    })
}

/// "there is no video 3 — a time needs a colon (15:00)": the number the coach
/// typed, refused as the video it claims to be, and the misreading it might
/// have been named rather than performed.
///
/// **The hint is always there**, not only on a number big enough to look like
/// seconds: a coach who types `14` meaning fourteen minutes into a two-video
/// project is the very reader it is for.
fn refuse_video(number: &str) -> LineVerdict {
    LineVerdict::Refused(format!(
        "there is no video {number} — a time needs a colon (15:00)"
    ))
}

// ----------------------------------------------------------------- batches

/// What one line of a pasted block came to.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchVerdict {
    Added(PendingMatchEvent),
    /// The same kind, on the same source, within [`SAME_EVENT_SECONDS`] of an
    /// event already in the project or already accepted from this block.
    AlreadyTagged(PendingMatchEvent),
    Refused(String),
}

/// One echoed line, in input order.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchLine {
    /// 1-based, counting every line of the block, so it names the line the
    /// coach can see.
    pub number: usize,
    /// The line as typed, trimmed — what a refusal quotes back.
    pub text: String,
    pub verdict: BatchVerdict,
}

/// A pasted block, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    /// Every line that will add, in input order: the command's payload.
    pub events: Vec<PendingMatchEvent>,
    /// One echo record per line that has anything to say. Blank and
    /// comment-only lines are not here.
    pub lines: Vec<BatchLine>,
    /// The **refused** lines, joined back into a block — what the box keeps
    /// after Add, so the coach fixes them in place and presses Add again.
    ///
    /// Only the refusals: a line that was added is done with, and one skipped
    /// as [`BatchVerdict::AlreadyTagged`] is not something editing can fix —
    /// the event it names is in the project already, so keeping it would ask
    /// the coach to clear a line whose only fault is being right. Blank and
    /// comment-only lines go with them, so a block that nothing refuses
    /// leaves the box empty.
    pub leftover: String,
}

/// A whole pasted block, against `project`.
///
/// The duplicate rule and the start/stop cap both count **existing +
/// accepted-so-far**: a block containing the same line twice adds it once, and
/// a block that would push past the format's last period has its excess lines
/// refused and the rest added. Best-effort, not all-or-nothing — a list of
/// twelve goals and one stray period line should add the twelve.
pub fn parse_batch(project: &Project, default_source: usize, text: &str) -> Batch {
    let cap = project
        .scoreboard
        .as_ref()
        .map(|s| s.format.expected_start_stop_events());
    let mut start_stops = project.start_stop_count();

    let mut batch = Batch {
        events: Vec::new(),
        lines: Vec::new(),
        leftover: String::new(),
    };
    let mut leftover: Vec<&str> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let verdict = match parse_line(project, default_source, line) {
            LineVerdict::Nothing => continue,
            LineVerdict::Refused(reason) => BatchVerdict::Refused(reason),
            LineVerdict::Event(event) if already_tagged(project, &batch.events, &event) => {
                BatchVerdict::AlreadyTagged(event)
            }
            LineVerdict::Event(event) => {
                let capped = event.kind == MatchEventKind::StartStop
                    && cap.is_some_and(|cap| start_stops >= cap);
                if capped {
                    BatchVerdict::Refused(START_STOP_CAP_REFUSAL.into())
                } else {
                    if event.kind == MatchEventKind::StartStop {
                        start_stops += 1;
                    }
                    batch.events.push(event);
                    BatchVerdict::Added(event)
                }
            }
        };
        if matches!(verdict, BatchVerdict::Refused(_)) {
            leftover.push(line.trim_end());
        }
        batch.lines.push(BatchLine {
            number: i + 1,
            text: line.trim().to_string(),
            verdict,
        });
    }
    batch.leftover = leftover.join("\n");
    batch
}

/// Is `event` the same event as one already stored, or as one this block has
/// already accepted? The second half matters: a block that contains the same
/// line twice must not add it twice, and the first copy is not in the project
/// yet.
fn already_tagged(
    project: &Project,
    accepted: &[PendingMatchEvent],
    event: &PendingMatchEvent,
) -> bool {
    let stored = project.match_events.iter().map(|m| PendingMatchEvent {
        kind: m.kind,
        source_index: m.source_index,
        source_seconds: m.source_seconds,
    });
    stored.chain(accepted.iter().copied()).any(|other| {
        other.kind == event.kind
            && other.source_index == event.source_index
            && (other.source_seconds - event.source_seconds).abs() <= SAME_EVENT_SECONDS
    })
}
