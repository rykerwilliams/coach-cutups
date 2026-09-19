//! Transport (spec D8): play/pause, skips, scrubs and volume, and the
//! player's events.
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
    pub(super) fn toggle_play(&mut self) {
        let play = !self.playing
            && self.seekable()
            && (self.loaded()
                || self.load(self.current, self.current_secs(), true, Origin::System));
        self.set_playing(play);
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
    /// The coordinator clamps to `total − END_MARGIN` (spec D8).
    pub(super) fn skip(&mut self, delta: f64) {
        let Some(open) = &self.open else {
            return;
        };
        if !delta.is_finite() || !self.seekable() {
            return;
        }
        let clip_duration = (open.project.total_source_duration() - END_MARGIN).max(0.0);
        let now = open.project.abs_seconds(self.current, self.current_secs());
        let decision = self.skip.request_skip(delta, now, clip_duration);
        self.apply_skip(decision);
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
    pub(super) fn deadline_passed(&mut self) {
        let decision = self.skip.burst_ended();
        self.apply_skip(decision);
    }

    /// Forgets any skip burst and its debounce. For user context switches
    /// (scrub release, list mutation, open) and a skip flight that won't
    /// land — never for a load that fulfils a skip.
    pub(super) fn reset_skip(&mut self) {
        self.skip.reset();
        self.deadline = None;
    }

    fn apply_skip(&mut self, decision: SkipDecision) {
        if let Some(debounce) = decision.arm_debounce {
            self.deadline = Some(Instant::now() + debounce);
        }
        if let Some(seek) = decision.seek {
            // A seek that can't be issued would leave the coordinator waiting
            // for its landing.
            if !self.seek_abs(seek.target_seconds, seek.exact, Origin::Skip) {
                self.reset_skip();
            }
        }
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
    /// its final frame stays up.
    fn end_of_stream(&mut self) {
        if !self.playing {
            return;
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
