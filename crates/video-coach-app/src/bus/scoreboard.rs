//! Match events and the scoreboard's setup (Phase 9 spec S5).
//!
//! A tag carries the position the readout already had, captured by the caller
//! at the keypress like every other logged command: the bus never asks the
//! player where it is, since queue delay would put the goal somewhere else.
//!
//! Each tag or delete is one undo step holding the **whole** event list. The
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
use video_coach_core::project::Project;
use video_coach_core::scoreboard::{MatchEventKind, ScoreboardConfig};
use video_coach_core::undo::UndoAction;

use super::{Bus, Event, UserError};

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
            return self.emit(Event::Error(UserError::Scoreboard(
                "every period of this match format is already tagged; \
                 change the format to tag more",
            )));
        }
        self.edit_match_events(|project| {
            project.append_match_event(kind, source_index, source_seconds);
        });
    }

    pub(super) fn delete_match_event(&mut self, id: Uuid) {
        self.edit_match_events(|project| {
            if project.delete_match_event(id).is_none() {
                eprintln!("bus: DeleteMatchEvent on an event that isn't there: {id}");
            }
        });
    }

    /// Replaces the scoreboard's setup: both teams, the format and the
    /// back-anchor flag, which is setup rather than a command of its own.
    ///
    /// Not an undo step — the history is the coach's edits, and the setup
    /// sheet has its own Cancel. An empty team name is refused here, so the
    /// render path never has to guard one (spec S5).
    pub(super) fn set_scoreboard(&mut self, config: ScoreboardConfig) {
        if config.home.name.trim().is_empty() || config.away.name.trim().is_empty() {
            return self.emit(Event::Error(UserError::Scoreboard(
                "both teams need a name",
            )));
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
