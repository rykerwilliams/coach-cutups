//! Reading a hand-tagged match, **read-only** (spec G1, G3).
//!
//! The ground truth is the coach's own tagged matches, and those folders are
//! the only copy. Everything here goes through [`store::read`] and
//! [`std::fs::read_to_string`]: no [`store::write`], no `Bus`, and no file of
//! any kind written inside a project folder.
//!
//! **Nothing identifying leaves this module.** A match is called `A`, `B`,
//! `C` …; the folder path, the project's name and the team names are read but
//! never printed, because the repository is public and the footage shows
//! children.

use std::path::{Path, PathBuf};

use video_coach_core::project::Project;
use video_coach_core::scoreboard::{self, AbsoluteMatchEvent, PeriodRole};
use video_coach_core::store;

/// The notes file beside `project.json` (G1). Absent is not an error.
pub const KICKOFFS_FILENAME: &str = "kickoffs.txt";

/// What a tag is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruthKind {
    /// A goal, either side. **Which side is thrown away**: it is not detected
    /// and not scored (the user's decision, Q9).
    Goal,
    PeriodStart,
    PeriodEnd,
    /// A post-goal restart from `kickoffs.txt`. A diagnostic, never something
    /// to detect, so it is not scored.
    Restart,
}

/// One tag, positioned on one source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TruthEvent {
    pub source_index: usize,
    pub seconds: f64,
    pub kind: TruthKind,
}

/// One hand-tagged match, anonymised.
#[derive(Debug, Clone, PartialEq)]
pub struct Truth {
    /// `A`, `B`, `C` … — never the folder name (see the module docs).
    pub name: String,
    /// Absolute paths to the source videos, for existence checks and for the
    /// analysis passes later tasks add. Never printed.
    pub sources: Vec<PathBuf>,
    /// Per source, from the project's stored `duration_seconds`, which is the
    /// duration authority the app itself uses.
    pub durations: Vec<f64>,
    /// Sorted by source, then time.
    pub events: Vec<TruthEvent>,
}

/// One parsed `kickoffs.txt` line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Restart {
    /// Zero-based; the file writes it one-based.
    pub source_index: usize,
    pub seconds: f64,
    /// One-based line number, so a bound check can name the line too.
    pub line: usize,
}

#[derive(thiserror::Error, Debug)]
pub enum TruthError {
    #[error("{0}")]
    Store(#[from] store::StoreError),
    #[error("{KICKOFFS_FILENAME} is unreadable: {0}")]
    Io(#[from] std::io::Error),
    /// Never a silent skip: a mistyped restart would quietly distort V-3.
    #[error("{KICKOFFS_FILENAME} line {line}: {why}")]
    Kickoffs { line: usize, why: String },
}

impl Truth {
    /// Read the tagged project in `folder` and call it `name`.
    pub fn read(name: impl Into<String>, folder: &Path) -> Result<Truth, TruthError> {
        let project = store::read(folder)?;
        let mut events = Vec::new();
        goals(&project, &mut events);
        periods(&project, &mut events);

        // An absent notes file is not an error: it costs the restart
        // diagnostics and V-3, nothing else. Mapped off the read itself rather
        // than tested with `exists()` first, as `store::read` does — one
        // syscall, and no window in which it appears between the two.
        let notes = match std::fs::read_to_string(folder.join(KICKOFFS_FILENAME)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(TruthError::Io(e)),
        };
        for r in parse_kickoffs(&notes)? {
            if r.source_index >= project.source_videos.len() {
                return Err(TruthError::Kickoffs {
                    line: r.line,
                    why: format!(
                        "source {} but the project has {}",
                        r.source_index + 1,
                        project.source_videos.len()
                    ),
                });
            }
            events.push(TruthEvent {
                source_index: r.source_index,
                seconds: r.seconds,
                kind: TruthKind::Restart,
            });
        }

        events.sort_by(|a, b| {
            a.source_index
                .cmp(&b.source_index)
                .then_with(|| a.seconds.total_cmp(&b.seconds))
        });
        Ok(Truth {
            name: name.into(),
            sources: project
                .source_videos
                .iter()
                .map(|s| folder.join(&s.relative_path))
                .collect(),
            durations: project
                .source_videos
                .iter()
                .map(|s| s.duration_seconds)
                .collect(),
            events,
        })
    }

    /// Every event of one kind on one source, in time order.
    pub fn on(&self, source_index: usize, kind: TruthKind) -> impl Iterator<Item = &TruthEvent> {
        self.events
            .iter()
            .filter(move |e| e.source_index == source_index && e.kind == kind)
    }

    fn first(&self, source_index: usize, kind: TruthKind) -> Option<f64> {
        self.on(source_index, kind).next().map(|e| e.seconds)
    }

    /// The `TAGS` and `GAPS` census (Task 3.1): what the tags alone say, with
    /// no detector involved. One fact per line, stable `key=value`, so two runs
    /// diff.
    pub fn print_census(&self) {
        let goals = self
            .events
            .iter()
            .filter(|e| e.kind == TruthKind::Goal)
            .count();
        let periods = self
            .events
            .iter()
            .filter(|e| matches!(e.kind, TruthKind::PeriodStart | TruthKind::PeriodEnd))
            .count();
        println!(
            "TAGS   match={} files={} goals={goals} periods={periods}",
            self.name,
            self.sources.len()
        );

        for (i, dur) in self.durations.iter().enumerate() {
            let start = self.first(i, TruthKind::PeriodStart);
            let stop = self.first(i, TruthKind::PeriodEnd);
            let goals: Vec<f64> = self.on(i, TruthKind::Goal).map(|e| e.seconds).collect();
            // How much room the last goal leaves before the whistle: a goal
            // with no room for a restart is structurally undetectable by a
            // kick-off-anchored rule.
            let headroom = stop.and_then(|s| goals.iter().map(|g| s - g).min_by(f64::total_cmp));
            println!(
                "TAGS   match={} src={i} dur={dur:.0} start={} stop={} lead_in={} tail={} goals={} headroom_min={}",
                self.name,
                seconds(start),
                seconds(stop),
                seconds(start),
                seconds(stop.map(|s| dur - s)),
                goals.len(),
                seconds(headroom),
            );
        }

        let mut goal_to_goal = Vec::new();
        let mut goal_to_stop = Vec::new();
        let mut start_to_goal = Vec::new();
        for i in 0..self.durations.len() {
            let goals: Vec<f64> = self.on(i, TruthKind::Goal).map(|e| e.seconds).collect();
            goal_to_goal.extend(goals.windows(2).map(|w| w[1] - w[0]));
            if let Some(stop) = self.first(i, TruthKind::PeriodEnd) {
                goal_to_stop.extend(goals.iter().map(|g| stop - g));
            }
            if let (Some(start), Some(first)) =
                (self.first(i, TruthKind::PeriodStart), goals.first())
            {
                start_to_goal.push(first - start);
            }
        }
        println!(
            "GAPS   match={} goal_to_goal_min={} goal_to_stop_min={} start_to_first_goal_min={}",
            self.name,
            seconds(goal_to_goal.into_iter().min_by(f64::total_cmp)),
            seconds(goal_to_stop.into_iter().min_by(f64::total_cmp)),
            seconds(start_to_goal.into_iter().min_by(f64::total_cmp)),
        );
    }
}

/// A census number, or `none` when there is no tag to compute it from.
fn seconds(value: Option<f64>) -> String {
    value.map_or_else(|| "none".to_string(), |v| format!("{v:.0}"))
}

fn goals(project: &Project, out: &mut Vec<TruthEvent>) {
    out.extend(
        project
            .match_events
            .iter()
            .filter(|e| e.kind.is_goal())
            .map(|e| TruthEvent {
                source_index: e.source_index,
                seconds: e.source_seconds,
                kind: TruthKind::Goal,
            }),
    );
}

/// The start/stops, through [`scoreboard::interpret`] — the only correct way to
/// learn which tag starts a period and which ends one, since nothing is tagged
/// "half-time". It works on **absolute** time, so a match whose two halves
/// share one file still reads correctly; each role is mapped back to its source
/// with [`Project::locate`].
fn periods(project: &Project, out: &mut Vec<TruthEvent>) {
    let Some(config) = &project.scoreboard else {
        return;
    };
    let absolute: Vec<AbsoluteMatchEvent> = project
        .match_events
        .iter()
        .map(|e| AbsoluteMatchEvent {
            id: Some(e.id),
            kind: e.kind,
            abs_seconds: project.abs_seconds(e.source_index, e.source_seconds),
        })
        .collect();
    out.extend(
        scoreboard::interpret(&absolute, config)
            .into_iter()
            .map(|e| {
                let (source_index, seconds) = project.locate(e.abs_seconds);
                TruthEvent {
                    source_index,
                    seconds,
                    kind: match e.role {
                        PeriodRole::Start(_) => TruthKind::PeriodStart,
                        PeriodRole::End(_) => TruthKind::PeriodEnd,
                    },
                }
            }),
    );
}

// ------------------------------------------------------------- kickoffs.txt

/// Parse `kickoffs.txt`: one restart per line, `<1-based source> <mm:ss>`.
///
/// `#` starts a comment, so G1's `# missing` line — a goal whose restart is not
/// in the file — needs no special case. Blank lines are ignored, and so is a
/// line that names a source with no time after it: that is the template's own
/// half-filled shape, a restart not written down yet. Anything else is an
/// error naming its line, never a silent skip, because a *mistyped* restart
/// would quietly distort V-3.
pub fn parse_kickoffs(text: &str) -> Result<Vec<Restart>, TruthError> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let body = raw.split('#').next().unwrap_or("").trim();
        if body.is_empty() {
            continue;
        }
        let bad = |why: &str| TruthError::Kickoffs {
            line,
            why: why.to_string(),
        };
        let fields: Vec<&str> = body.split_whitespace().collect();
        let [index, rest @ ..] = fields.as_slice() else {
            unreachable!("a non-empty body has at least one field");
        };
        let index: usize = index
            .parse()
            .map_err(|_| bad("the source index is not a number"))?;
        let source_index = index
            .checked_sub(1)
            .ok_or_else(|| bad("the source index is 1-based, so 0 is not one"))?;
        let time = match rest {
            // The template the coach fills in writes each goal's video number
            // and leaves the time blank underneath it. A line still waiting
            // for its time is a restart not written yet, not a mistyped one:
            // the file is half filled in while the coach works through a
            // match, and refusing it would stop the whole measurement run over
            // a goal nobody has got to.
            [] => continue,
            [time] => time,
            _ => return Err(bad("expected `<1-based source index> <mm:ss>`")),
        };
        out.push(Restart {
            source_index,
            seconds: mm_ss(time).ok_or_else(|| bad("the time is not `mm:ss`"))?,
            line,
        });
    }
    Ok(out)
}

fn mm_ss(text: &str) -> Option<f64> {
    let (minutes, seconds) = text.split_once(':')?;
    let minutes: u32 = minutes.parse().ok()?;
    // Two digits, and under a minute: `3:75` is a typo, not 4:15.
    if seconds.len() != 2 {
        return None;
    }
    let seconds: u32 = seconds.parse().ok()?;
    (seconds < 60).then(|| f64::from(minutes) * 60.0 + f64::from(seconds))
}

// ------------------------------------------------------- COACH_GROUND_TRUTH

/// Split `COACH_GROUND_TRUTH` into `(name, folder)` pairs, **first is the
/// tuning match** (G2).
///
/// An entry is a folder, or `<LABEL>=<folder>` where the label is one or two
/// upper-case alphanumerics. Without a label the name is the entry's position
/// — `A`, `B`, `C` … — which is the rule that guarantees nothing identifying
/// reaches the terminal. The label exists because the tuning match goes first
/// while the letters have to stay pinned to the match they named in the plan's
/// measured tables; the shape of a label is what keeps it anonymous.
pub fn folders(var: &str) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (i, entry) in var.split(':').filter(|e| !e.is_empty()).enumerate() {
        let (name, folder) = match entry.split_once('=') {
            Some((label, rest)) if is_label(label) => (label.to_string(), rest),
            _ => (positional(i), entry),
        };
        if out.iter().any(|(n, _)| *n == name) {
            return Err(format!("two matches are both called {name}"));
        }
        out.push((name, PathBuf::from(folder)));
    }
    if out.is_empty() {
        return Err("COACH_GROUND_TRUTH names no folders".to_string());
    }
    Ok(out)
}

fn is_label(text: &str) -> bool {
    matches!(text.len(), 1 | 2)
        && text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && text.starts_with(|c: char| c.is_ascii_uppercase())
}

fn positional(index: usize) -> String {
    match u8::try_from(index) {
        Ok(i) if i < 26 => char::from(b'A' + i).to_string(),
        _ => format!("M{index}"),
    }
}
