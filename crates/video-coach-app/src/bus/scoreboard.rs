//! Match events and the scoreboard's setup (Phase 9 spec S5).
//!
//! A tag carries the position the readout already had, captured by the caller
//! at the keypress like every other logged command: the bus never asks the
//! player where it is, since queue delay would put the goal somewhere else.
//!
//! A goal's reel trim (match vision spec R3) is set the same way, from the scan
//! position captured at the click, and lives on the goal's record.
//!
//! The editor's two commands are the exception that proves the rule: they carry
//! **typed** text, not a captured position, so there is nothing for queue delay
//! to stale. They hand that text to `core::match_entry` here rather than
//! trusting events parsed at the UI, so every rule of the grammar — the
//! duration bound, the duplicate window, the start/stop cap — is read once,
//! from core, against the project as it is when the command lands.
//!
//! Each tag, delete or trim is one undo step holding the **whole** event list. The
//! list is a handful of records, and a snapshot needs no per-event inverse —
//! but it does hold source indices, so a source move or removal purges it from
//! both stacks (`clips::purge_history_for_source_change`).
//!
//! **The refusal at the cap lives here, not in core's mutator.** A start/stop
//! past the format's last period gets no role from `interpret`, so it would be
//! a record the scoreboard ignores; the Match panel disables the action there
//! (on core's `Project::start_stops_at_cap`, the one rule) and so does the key,
//! and this refuses out loud if it is reached anyway — as a notice, since a
//! backstop for a disabled control has no business stopping the session with a
//! dialog. macOS's mutator silently did nothing instead, which is worse than a
//! refusal.

use uuid::Uuid;
use video_coach_core::match_entry::{
    self, BatchLine, BatchVerdict, LineVerdict, PendingMatchEvent,
};
use video_coach_core::project::Project;
use video_coach_core::scoreboard::{
    MatchEventKind, MatchEventRecord, ReelEnd, ScoreboardConfig, START_STOP_CAP_REFUSAL,
};
use video_coach_core::undo::UndoAction;

use super::{Bus, Event, UserError};

/// One line of the editor's row field, read against the record it names:
/// the whole verdict, **the cap included**.
///
/// Core owns the grammar ([`match_entry`]); the start/stop cap is this
/// module's rule rather than the grammar's, and it is folded in here so that
/// one call answers both the field's `✕` mark (`src/main.rs`) and
/// [`Bus::edit_match_event`] below. Written on both sides they would drift,
/// and the coach would see a line marked good and then refused.
pub fn editor_line(
    project: &Project,
    record: &MatchEventRecord,
    line: &str,
) -> Result<PendingMatchEvent, String> {
    // The seed is the field's own line, rebuilt from the record:
    // `edit_from_line` keeps the stored seconds when the time token is
    // untouched, so retyping only the kind can't re-round a stored 14.06 to
    // the 14.0 the line shows. A line with no video number of its own stays
    // on the event's own source.
    let seed = match_entry::format_line(record.kind, record.source_index, record.source_seconds);
    let event = match match_entry::edit_from_line(
        project,
        record.source_index,
        &seed,
        line,
        record.source_seconds,
    ) {
        LineVerdict::Event(event) => event,
        LineVerdict::Refused(reason) => return Err(reason),
        // An emptied field: deleting is the row's `×`, not a blank line.
        LineVerdict::Nothing => {
            return Err(format!("nothing on that line — a row reads \"{seed}\""))
        }
    };
    // Moving a start/stop can't break the cap; becoming one can.
    if event.kind == MatchEventKind::StartStop
        && record.kind != MatchEventKind::StartStop
        && project.start_stops_at_cap()
    {
        return Err(START_STOP_CAP_REFUSAL.into());
    }
    Ok(event)
}

impl Bus {
    /// Tags `kind` at `(source_index, source_seconds)`.
    pub(super) fn tag_match_event(
        &mut self,
        kind: MatchEventKind,
        source_index: usize,
        source_seconds: f64,
    ) {
        let Some(open) = &self.open else {
            return;
        };
        if source_index >= open.project.source_videos.len() {
            return eprintln!("bus: TagMatchEvent on source {source_index}, which isn't there");
        }
        // `Project::start_stops_at_cap` is the one cap rule, shared with the
        // Match panel, which disables the action on it.
        if kind == MatchEventKind::StartStop && open.project.start_stops_at_cap() {
            return self.refuse(START_STOP_CAP_REFUSAL.into());
        }
        self.edit_match_events(|project| {
            project.append_match_event(kind, source_index, source_seconds);
        });
    }

    /// Retypes the event with `id` from one line of the editor's grammar
    /// (spec C1), read by [`editor_line`] — the same call the field's mark
    /// makes, so a line marked good is never refused here.
    ///
    /// The field commits on Enter *and* on the focus that commit drops, so
    /// the same line arrives twice. The second lands on a record the first
    /// already moved, reads back to the same event, and `edit_match_events`
    /// drops it: no save, no undo step, no publish.
    pub(super) fn edit_match_event(&mut self, id: Uuid, line: &str) {
        let Some(open) = &self.open else {
            return;
        };
        let Some(record) = open.project.match_events.iter().find(|m| m.id == id) else {
            return eprintln!("bus: EditMatchEvent on an event that isn't there: {id}");
        };
        let event = match editor_line(&open.project, record, line) {
            Ok(event) => event,
            Err(reason) => return self.refuse(reason),
        };
        self.edit_match_events(|project| {
            if !project.edit_match_event(id, event.kind, event.source_index, event.source_seconds) {
                eprintln!("bus: EditMatchEvent on an event that isn't there: {id}");
            }
        });
    }

    /// Adds a pasted block as one undo step (spec C3): `edit_match_events`
    /// snapshots the whole list around the closure, so twenty appends inside
    /// one are one save, one step and one `ProjectChanged`.
    ///
    /// Best-effort, not all-or-nothing (spec V4) — the lines that can't land
    /// are named in one notice and the rest are added.
    ///
    /// **This parse is the one that decides what the box keeps** (spec B5),
    /// which is why the leftover travels back as an event rather than being
    /// stripped at the UI: the UI re-reads the box against a snapshot that
    /// may be a command behind, and a line this parse treated differently
    /// would then be in neither the box nor the project.
    pub(super) fn add_match_events(&mut self, text: &str, default_source: usize) {
        let Some(open) = &self.open else {
            return;
        };
        let batch = match_entry::parse_batch(&open.project, default_source, text);
        let events = batch.events;
        self.edit_match_events(|project| {
            for event in &events {
                project.append_match_event(event.kind, event.source_index, event.source_seconds);
            }
        });
        self.emit(Event::MatchPasteLeftover(batch.leftover));
        if let Some(notice) = batch_notice(&batch.lines) {
            self.refuse(notice);
        }
    }

    pub(super) fn delete_match_event(&mut self, id: Uuid) {
        self.edit_match_events(|project| {
            if project.delete_match_event(id).is_none() {
                eprintln!("bus: DeleteMatchEvent on an event that isn't there: {id}");
            }
        });
    }

    /// Sets or resets one end of `goal`'s reel entry at the position the
    /// caller captured (match vision spec R3). Core owns the rules; a refusal
    /// changes nothing and is said out loud, since the position is wherever
    /// the coach happened to be scanning.
    pub(super) fn set_reel_trim(&mut self, goal: Uuid, end: ReelEnd, at: Option<(usize, f64)>) {
        let mut refused = None;
        self.edit_match_events(|project| {
            refused = project.set_reel_trim(goal, end, at).err();
        });
        if let Some(e) = refused {
            self.refuse(e.to_string());
        }
    }

    /// Replaces the scoreboard's setup: both teams, the format and the
    /// back-anchor flag, which is setup rather than a command of its own.
    ///
    /// Not an undo step — the history is the coach's edits, and the setup
    /// sheet has its own Cancel. An empty team name is refused here, so the
    /// render path never has to guard one (spec S5).
    pub(super) fn set_scoreboard(&mut self, config: ScoreboardConfig) {
        if config.home.name.trim().is_empty() || config.away.name.trim().is_empty() {
            return self.refuse("both teams need a name".into());
        }
        let Some(open) = &mut self.open else {
            return;
        };
        if open.project.scoreboard.as_ref() == Some(&config) {
            return;
        }
        open.project.scoreboard = Some(config);
        self.project_changed();
    }

    /// Says a refused match command out loud, as a notice (spec C5): a modal
    /// could land over a live commentary take and swallow the transport keys.
    fn refuse(&mut self, reason: String) {
        self.emit(Event::Error(UserError::Scoreboard(reason)));
    }

    /// Applies `edit` to the event list as one undo step, unless it changed
    /// nothing.
    fn edit_match_events(&mut self, edit: impl FnOnce(&mut Project)) {
        let Some(open) = &mut self.open else {
            return;
        };
        let before = open.project.match_events.clone();
        edit(&mut open.project);
        let after = open.project.match_events.clone();
        if after == before {
            return;
        }
        self.save();
        self.record(UndoAction::EditMatchEvents { before, after });
        self.publish_project();
    }
}

/// One sentence for a paste that didn't land whole: how many of the block's
/// lines were added, then each reason with the number of lines that gave it,
/// in the order they appear. `None` when every line landed.
///
/// The count is of lines that said something — blanks and comments are not in
/// `lines` at all — and the per-line detail is the echo's job, before Add is
/// ever pressed. This is the backstop, and the only refusal the sheet can
/// show: the status bar's notice renders behind its scrim (spec C5).
///
/// **It earns its place twice over**, even beside the echo. The echo is drawn
/// against the UI's snapshot, which may be a command behind this parse, so it
/// is the only word on a verdict the echo could not predict — the cap, or a
/// duplicate of something added since. And the duplicates it counts are gone
/// from the box by the time it is read (spec B5), so nothing else says they
/// were skipped.
fn batch_notice(lines: &[BatchLine]) -> Option<String> {
    let mut reasons: Vec<(String, usize)> = Vec::new();
    let mut added = 0;
    for line in lines {
        let reason = match &line.verdict {
            BatchVerdict::Added(_) => {
                added += 1;
                continue;
            }
            BatchVerdict::AlreadyTagged(_) => "already tagged".to_string(),
            BatchVerdict::Refused(reason) => format!("refused: {reason}"),
        };
        match reasons.iter_mut().find(|(seen, _)| *seen == reason) {
            Some((_, count)) => *count += 1,
            None => reasons.push((reason, 1)),
        }
    }
    if reasons.is_empty() {
        return None;
    }
    let parts: Vec<String> = reasons
        .iter()
        .map(|(reason, count)| format!("{count} {reason}"))
        .collect();
    Some(format!(
        "{added} of {} added: {}",
        lines.len(),
        parts.join(", ")
    ))
}
