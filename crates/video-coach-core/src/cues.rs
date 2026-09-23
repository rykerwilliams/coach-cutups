//! The scoreboard as a cue list: the score and clock as lines of text against
//! output time, rather than pixels painted into the picture (spec U).
//!
//! One line per distinct reading, run-length-encoded over the output's frames,
//! and [`cues_to_srt`] to write it beside the file. Media writes the string;
//! nothing about SRT lives there.

use crate::export::{Compilation, OUTPUT_FPS};
use crate::scoreboard::{format_clock, ScoreboardConfig, ScoreboardContext, ScoreboardState};

/// One line of the scoreboard, and the stretch of **output** time it covers.
///
/// Times are the plan's — a frame index over [`OUTPUT_FPS`] — never a sum of
/// source durations: per-entry quantization would move every later cue.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The scoreboard's line for one instant (spec T2): one line, no markup.
///
/// The burned board fits its labels into fixed cells; a subtitle line is in no
/// cell, so a long club name wraps in the viewer's player and that is the
/// viewer's player's business.
fn cue_line(config: &ScoreboardConfig, state: &ScoreboardState) -> String {
    let clock = format_clock(state.clock);
    let mut line = format!(
        "{} {} - {} {} · {}",
        config.home.name, state.home_score, state.away_score, config.away.name, clock.main
    );
    // Empty unless the clock is in stoppage.
    if !clock.trailing.is_empty() {
        line.push(' ');
        line.push_str(&clock.trailing);
    }
    line
}

/// The scoreboard over the whole of one export, as cues.
///
/// **[`ScoreboardContext::state_at`] is called per frame** — the Phase 9 rule,
/// not an optimization to skip. A frozen entry reads the same state either side
/// of its pause, and the run-length encoding is what turns that into one long
/// cue instead of a clock that runs on while the picture is held.
///
/// A frame whose state is `None` ends the run it was in and starts none: a gap,
/// which is what "no match yet" means.
pub fn scoreboard_cues(compilation: &Compilation, scoreboard: &ScoreboardContext) -> Vec<Cue> {
    let at = |frame: usize| frame as f64 / f64::from(OUTPUT_FPS);
    let mut cues = Vec::new();
    // The run in progress: the frame it started on and the line it reads.
    let mut run: Option<(usize, String)> = None;

    for (n, frame) in compilation.frames.iter().enumerate() {
        let source_index = compilation.plan.entries[frame.entry].source_index;
        let line = scoreboard
            .state_at(source_index, frame.source_time)
            .map(|state| cue_line(scoreboard.config(), &state));
        if matches!((&run, &line), (Some((_, running)), Some(line)) if running == line) {
            continue;
        }
        if let Some((start, text)) = run.take() {
            cues.push(Cue {
                start: at(start),
                end: at(n),
                text,
            });
        }
        run = line.map(|text| (n, text));
    }
    if let Some((start, text)) = run {
        cues.push(Cue {
            start: at(start),
            end: at(compilation.frames.len()),
            text,
        });
    }
    cues
}

fn timecode(seconds: f64) -> String {
    // `as` truncates and saturates, so nothing here can panic or read as a
    // negative time.
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1_000) % 60,
        ms % 1_000
    )
}

/// The cue list as SubRip: numbered from 1, `\n` endings, one blank line
/// between cues. Empty in, empty out.
pub fn cues_to_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            timecode(cue.start),
            timecode(cue.end),
            cue.text
        ));
    }
    out
}
