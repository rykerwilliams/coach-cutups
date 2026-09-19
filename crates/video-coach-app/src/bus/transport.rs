//! Transport (spec D8): play/pause, skips, scrubs and volume, and the
//! player's events. While recording (Phase 4 R10), play, pause and skips are
//! logged, and skips and EOS stay inside the clip's source.
//!
//! Every seek goes through the player's single-flight slot. Skips go through
//! the [`SkipCoordinator`](video_coach_core::skip::SkipCoordinator) first, over
//! concat time, and every outcome of a skip's flight reaches it: a completion
//! drives the burst on, while a displacement or failure resets it, so it can
//! never be left waiting for a landing that won't come.

use std::time::Instant;

use video_coach_core::skip::SkipDecision;
use video_coach_media::{Origin, PlayerEvent};

use super::sources::END_MARGIN;
use super::{Bus, Event, UserError};

impl Bus {
    /// Play is refused (answered with `Playing(false)`) with no sources or
    /// while any is missing. If the player dropped the current source (after
    /// an error), play reloads it where it was first. Pausing is always
    /// allowed.
    ///
    /// While recording, a change is logged at `host_ns`, anchored where the
    /// player is heading, with `ui_secs` as the position the UI saw.
    pub(super) fn toggle_play(&mut self, host_ns: u64, ui_secs: Option<f64>) {
        let was_playing = self.playing;
        let play = !self.playing
            && self.seekable()
            && (self.loaded()
                || self.load(self.current, self.current_secs(), true, Origin::System));
        self.set_playing(play);
        if self.playing != was_playing {
            self.log_playing(host_ns, ui_secs);
        }
    }

    pub(super) fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.player.set_playing(playing);
        self.emit(Event::Playing(playing));
    }

    /// Applies at once; persists to `scan_volume` only on `commit`.
    pub(super) fn set_volume(&mut self, value: f64, commit: bool) {
        if !value.is_finite() {
            return;
        }
        let value = value.clamp(0.0, 1.0);
        self.player.set_volume(value);
        if !commit {
            return;
        }
        if let Some(open) = &mut self.open {
            open.project.preferences.scan_volume = value;
            self.project_changed();
        }
    }

    /// Skips by `delta` concat seconds from the burst's accumulated target,
    /// or from where the player is (or is heading) if no burst is running.
    /// The coordinator clamps to `total − END_MARGIN` (spec D8), or while
    /// recording to the clip's source, short of its end by the same margin
    /// (R10), and the requested delta is logged at `host_ns`.
    pub(super) fn skip(&mut self, delta: f64, host_ns: u64) {
        let Some(open) = &self.open else {
            return;
        };
        if !delta.is_finite() || !self.seekable() {
            return;
        }
        let range = match &self.recording {
            Some(active) => {
                let src = active.pending.source_index;
                let start = open.project.cumulative_offset(src);
                let duration = open
                    .project
                    .source_videos
                    .get(src)
                    .map_or(0.0, |s| s.duration_seconds);
                start..=start + (duration - END_MARGIN).max(0.0)
            }
            None => 0.0..=(open.project.total_source_duration() - END_MARGIN).max(0.0),
        };
        let now = open.project.abs_seconds(self.current, self.current_secs());
        let decision = self.skip.request_skip(delta, now, range);
        self.apply_skip(decision);
        if let Some(active) = &mut self.recording {
            active.log.skip(host_ns, delta);
        }
    }

    /// Scrub moves are keyframe seeks, latest wins. A release is a new user
    /// context: it abandons any skip burst, then lands frame-accurate.
    pub(super) fn scrub(&mut self, abs: f64, release: bool) {
        if release {
            self.reset_skip();
        }
        self.seek_abs(abs, release, Origin::Scrub);
    }

    /// The skip debounce fired: the burst is over.
    pub(super) fn skip_debounce_passed(&mut self) {
        let decision = self.skip.burst_ended();
        self.apply_skip(decision);
    }

    /// Forgets any skip burst and its debounce. For user context switches
    /// (scrub release, list mutation, open) and a skip flight that won't
    /// land — never for a load that fulfils a skip.
    pub(super) fn reset_skip(&mut self) {
        self.skip.reset();
        self.skip_deadline = None;
    }

    fn apply_skip(&mut self, decision: SkipDecision) {
        if let Some(debounce) = decision.arm_debounce {
            self.skip_deadline = Some(Instant::now() + debounce);
        }
        if let Some(seek) = decision.seek {
            // A seek that can't be issued would leave the coordinator waiting
            // for its landing.
            if !self.seek_abs(seek.target_seconds, seek.exact, Origin::Skip) {
                self.reset_skip();
            }
        }
    }

    /// Where the player is heading, as source index and source seconds (R6,
    /// R10): a skip burst's target, else the seek in flight, else `ui_secs`
    /// (the position the UI read at the keypress), else the pipeline's
    /// position. Both a recording's start and its play and pause anchors.
    pub(super) fn heading(&self, ui_secs: Option<f64>) -> (usize, f64) {
        if let (Some(abs), Some(open)) = (self.skip.target(), &self.open) {
            return open.project.locate(abs);
        }
        let secs = self
            .player
            .target_secs()
            .or(ui_secs)
            .unwrap_or_else(|| self.position.query_position().unwrap_or(0.0));
        (self.current, secs)
    }

    /// Whether seeks are allowed: some sources, none missing.
    fn seekable(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| !open.project.source_videos.is_empty())
            && !self.any_missing()
    }

    /// Seeks to concat time `abs`. `locate` clamps it to the timeline and
    /// `load` short of its source's end, which for the last source is the
    /// spec's `total − END_MARGIN`. Returns whether a request was issued.
    fn seek_abs(&mut self, abs: f64, accurate: bool, origin: Origin) -> bool {
        let Some(open) = &self.open else {
            return false;
        };
        if !abs.is_finite() || !self.seekable() {
            return false;
        }
        let (index, secs) = open.project.locate(abs);
        self.load(index, secs, accurate, origin)
    }

    pub(super) fn player_events(&mut self, events: Vec<PlayerEvent>) {
        for event in events {
            match event {
                // The burst's next seek is issued in this same batch, so the
                // player is never idle between a burst's flights and no
                // settled position is published there.
                PlayerEvent::SeekDone {
                    origin: Origin::Skip,
                } => {
                    let decision = self.skip.seek_completed();
                    self.apply_skip(decision);
                }
                PlayerEvent::SeekDisplaced {
                    origin: Origin::Skip,
                }
                | PlayerEvent::SeekFailed {
                    origin: Origin::Skip,
                } => self.reset_skip(),
                PlayerEvent::SeekDone { .. }
                | PlayerEvent::SeekDisplaced { .. }
                | PlayerEvent::SeekFailed { .. } => {}
                PlayerEvent::Loaded { diagnostics } => eprintln!(
                    "bus: loaded {}: decoder {:?}, glupload caps {:?}, GL platform {:?}",
                    self.player.loaded_uri().unwrap_or("?"),
                    diagnostics.decoder,
                    diagnostics.glupload_caps,
                    diagnostics.gl_platform
                ),
                PlayerEvent::Eos => self.end_of_stream(),
                PlayerEvent::Error(msg) => {
                    eprintln!("bus: player error: {msg}");
                    // The player dropped its flight and its source; play or
                    // the next seek reloads it. One failure often posts
                    // several errors.
                    self.reset_skip();
                    // While recording this pause isn't logged, as at EOS: it
                    // would need a bus-side time. Replay keeps playing until
                    // the next anchor. Rare, and accepted.
                    if self.playing {
                        self.set_playing(false);
                    }
                    self.emit(Event::Error(UserError::Playback(msg)));
                    // A file deleted mid-session gets its Relink card.
                    if self.refresh_missing() {
                        self.publish_project();
                    }
                }
            }
        }
    }

    /// Playback reached the end of `current` (spec D4): continue into the
    /// next source from its start, or stop at the end of the last one, where
    /// its final frame stays up. While recording it stops without advancing,
    /// since a clip points into one source, and logs nothing: replay's play
    /// tail freezes on the last frame the same way, and a pause here would
    /// carry a bus-side time (R10).
    fn end_of_stream(&mut self) {
        if !self.playing {
            return;
        }
        if self.recording.is_some() {
            return self.set_playing(false);
        }
        let sources = self
            .open
            .as_ref()
            .map_or(0, |open| open.project.source_videos.len());
        let next = self.current + 1;
        if next >= sources || !self.load(next, 0.0, true, Origin::System) {
            self.set_playing(false);
        }
    }

    /// Drops the loaded source entirely, so no stale frame stays up: for no
    /// sources, or a current source that is missing.
    pub(super) fn unload(&mut self) {
        self.reset_skip();
        self.player.unload();
        if self.playing {
            self.set_playing(false);
        }
    }

    /// Publishes where the player is heading, recomputed from the player
    /// after every input (so a list change that moves the concat offsets
    /// moves the target too), or the settled position once it's idle. A
    /// pause settling with nothing requested publishes nothing.
    pub(super) fn publish_position(&mut self) {
        let target = self.player.target_secs();
        if target.is_some() || self.player.is_idle() {
            self.publish_position_at(target);
        }
    }

    /// Publishes `current` with `target` (source seconds), unless unchanged.
    pub(super) fn publish_position_at(&mut self, target: Option<f64>) {
        let target_abs = target.and_then(|secs| {
            let open = self.open.as_ref()?;
            Some(open.project.abs_seconds(self.current, secs))
        });
        let position = (self.current, target_abs);
        if position == self.last_position {
            return;
        }
        self.last_position = position;
        self.emit(Event::Position {
            source_index: self.current,
            target_abs,
        });
    }
}
