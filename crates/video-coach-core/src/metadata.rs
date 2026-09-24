//! What an exported file is called, and what it says about itself.
//!
//! Two things live here, because they are the same words twice over: what an
//! [`ExportTarget`] is called (in the sheet, and in the file name the app
//! builds from it), and the tag set that goes into the file's header so a
//! media library can read the match, the result and the teams off it without
//! opening the app.
//!
//! **No media dependency**, like the rest of this crate: [`file_tags`] is a
//! pure function of the project, the target and a date the caller supplies,
//! and `video-coach-media` is what turns the result into MP4 header boxes.
//! It is the sibling of [`crate::whole_match`]'s chapter wording,
//! [`crate::reel`]'s captions and [`crate::scoreboard::chapter_events`]: one
//! place per kind of sentence the app writes into a file.
//!
//! **Where a tag can't be told the truth it is left out** rather than
//! guessed: a project with no scoreboard has no teams to name and no result
//! to report, so it gets a title from the project's own name and neither a
//! comment nor keywords.

use crate::plan::ExportTarget;
use crate::project::{Clip, Project};
use crate::reel::ReelSide;
use crate::scoreboard::{team_name, ScoreboardContext};

/// The application's name, as it tags what it writes. The application ID
/// (`coach-cuts`) is the same name as a file name; this is the one to read.
pub const APP_NAME: &str = "Coach Cuts";

/// What the every-clip target is called, in the sheet and in its file name.
pub const ALL_CLIPS_LABEL: &str = "All clips";
/// What a reel of both sides' goals is called, in the sheet and in its file
/// name. One side's is named after the team ([`reel_label`]).
pub const REEL_LABEL: &str = "All goals";
/// What the whole-match export is called, in the sheet and in its file name.
pub const WHOLE_MATCH_LABEL: &str = "Whole match";
/// What a clip with no name of its own is called.
pub const UNTITLED: &str = "Untitled";

/// What a clip is called in a file name and in a refusal: its own name, or
/// [`UNTITLED`] when it hasn't been given one.
pub fn clip_label(clip: &Clip) -> &str {
    match clip.name.trim() {
        "" => UNTITLED,
        name => name,
    }
}

/// What a reel is called, in the sheet and in its file name (spec R1b): the
/// side's configured team name ("Rovers goals"), or "Home goals" / "Away
/// goals" where no scoreboard is set up. Both sides' is [`REEL_LABEL`].
pub fn reel_label(project: &Project, side: ReelSide) -> String {
    let home = match side {
        ReelSide::All => return REEL_LABEL.to_owned(),
        ReelSide::Home => true,
        ReelSide::Away => false,
    };
    format!("{} goals", team_name(project.scoreboard.as_ref(), home))
}

/// A day on the calendar, with no time and no zone: what an exported file's
/// `date` tag carries.
///
/// The caller supplies it, because the day a timestamp falls on depends on the
/// reader's time zone and this crate has no clock. The app derives it from the
/// game video's own modification time, which for downloaded match footage is
/// within a day of the match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarDate {
    pub year: i32,
    /// 1–12.
    pub month: u32,
    /// 1–31.
    pub day: u32,
}

/// What an exported file says about itself: its header tags, ready for a
/// muxer.
///
/// An empty `String` and an empty `Vec` mean "write no such tag" — which is
/// what a project with no scoreboard gets for `comment` and `keywords`, and
/// what [`FileTags::default`] is for in a test that doesn't care.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTags {
    /// `"Rovers v Athletic — whole match"`: the match, then what this file is
    /// of it.
    pub title: String,
    /// One sentence naming what the export is and which project it came from.
    pub description: String,
    /// The final score, `"Rovers 2 - 1 Athletic"`, or empty where the match
    /// has none to report.
    pub comment: String,
    /// Both team names, so a library can group by team. Empty where no
    /// scoreboard is set up.
    pub keywords: Vec<String>,
    /// `"Coach Cuts 0.4.0"`: the app and its version.
    pub encoder: String,
    /// The game video's own date, or `None` when the caller couldn't read
    /// one.
    pub date: Option<CalendarDate>,
}

/// The tags an export of `target` should carry, with `date` as the footage's
/// own day (`None` writes no date).
///
/// **The score is the scoreboard's at the end of the match**, which is the
/// end of the last game video — after full time the score no longer moves, so
/// asking there is asking for the result. A project with no scoreboard, or one
/// whose kick-off isn't tagged yet, has no result to state and gets no comment
/// (see [`crate::scoreboard::scoreboard_state`]).
pub fn file_tags(project: &Project, target: &ExportTarget, date: Option<CalendarDate>) -> FileTags {
    let phrase = target_phrase(project, target);
    let project_name = project.name.trim();
    FileTags {
        title: match match_name(project) {
            Some(match_name) => format!("{match_name} — {phrase}"),
            None => sentence_case(&phrase),
        },
        description: match project_name {
            "" => format!("{}, from a Coach Cuts project.", sentence_case(&phrase)),
            name => format!(
                "{}, from the Coach Cuts project “{name}”.",
                sentence_case(&phrase)
            ),
        },
        comment: final_score(project).unwrap_or_default(),
        keywords: team_keywords(project),
        encoder: format!("{APP_NAME} {}", env!("CARGO_PKG_VERSION")),
        date,
    }
}

/// `"Rovers v Athletic"`, or `None` where no scoreboard names the teams — a
/// title then falls back to what the export is, since inventing "Home v Away"
/// for a file someone else will read is worse than saying nothing.
///
/// A blank team name (only a project file edited by hand has one) counts as no
/// scoreboard, rather than producing `" v Athletic"`.
fn match_name(project: &Project) -> Option<String> {
    let config = project.scoreboard.as_ref()?;
    let (home, away) = (config.home.name.trim(), config.away.name.trim());
    (!home.is_empty() && !away.is_empty()).then(|| format!("{home} v {away}"))
}

/// What the title calls this target, as a phrase that follows the match name:
/// `"whole match"`, `"all clips"`, one side's `"Rovers goals"`, a tag or a
/// clip name as the coach typed it.
///
/// The fixed labels lose their capital here and keep it in the sheet: the
/// words themselves are still declared once, so renaming a target renames it
/// in the file name, the sheet row and the title together.
fn target_phrase(project: &Project, target: &ExportTarget) -> String {
    match target {
        ExportTarget::WholeMatch => lower_case(WHOLE_MATCH_LABEL),
        ExportTarget::AllClips => lower_case(ALL_CLIPS_LABEL),
        ExportTarget::Reel(ReelSide::All) => lower_case(REEL_LABEL),
        ExportTarget::Reel(side) => reel_label(project, *side),
        ExportTarget::Tag(tag) => tag.clone(),
        ExportTarget::Clip(id) => project
            .clips
            .iter()
            .find(|clip| clip.id == *id)
            .map_or(UNTITLED, clip_label)
            .to_owned(),
    }
}

/// `"Rovers 2 - 1 Athletic"`, or `None` where there is no result to state.
fn final_score(project: &Project) -> Option<String> {
    let context = ScoreboardContext::for_project(project)?;
    let state = context.state_at_abs(project.total_source_duration())?;
    let config = context.config();
    let (home, away) = (config.home.name.trim(), config.away.name.trim());
    (!home.is_empty() && !away.is_empty())
        .then(|| format!("{home} {} - {} {away}", state.home_score, state.away_score))
}

/// Both team names, blanks dropped and a pair of identical names collapsed, so
/// the keyword list a library groups by is never `"Rovers, Rovers"` and never
/// has an empty member.
fn team_keywords(project: &Project) -> Vec<String> {
    let Some(config) = project.scoreboard.as_ref() else {
        return Vec::new();
    };
    let mut keywords: Vec<String> = Vec::with_capacity(2);
    for name in [config.home.name.trim(), config.away.name.trim()] {
        if !name.is_empty() && !keywords.iter().any(|seen| seen == name) {
            keywords.push(name.to_owned());
        }
    }
    keywords
}

/// The label with its first character lowered: `"Whole match"` reads as
/// `"whole match"` inside a title. Only the first character, so a tag or a
/// team name keeps whatever case the coach gave it.
fn lower_case(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The inverse, for a phrase that starts a sentence.
fn sentence_case(phrase: &str) -> String {
    let mut chars = phrase.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
