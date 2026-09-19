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
use super::{Bus, Event};

impl Bus {
    /// Play is refused (answered with `Playing(false)`) while any source is
    /// missing or nothing is loaded. Pausing is always allowed.
    pub(super) fn toggle_play(&mut self) {
        let play = !self.playing && self.loaded && !self.any_missing();
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
                PlayerEvent::SeekDone { origin } => {
                    // Before `request_ended`: the burst's next seek keeps a
                    // request outstanding, so no settled position is
                    // published between a burst's flights.
                    if origin == Origin::Skip {
                        let decision = self.skip.seek_completed();
                        self.apply_skip(decision);
                    }
                    self.request_ended();
                }
                PlayerEvent::SeekDisplaced { origin } | PlayerEvent::SeekFailed { origin } => {
                    if origin == Origin::Skip {
                        self.reset_skip();
                    }
                    self.request_ended();
                }
                PlayerEvent::Loaded { diagnostics } => eprintln!(
                    "bus: loaded source {}: decoder {:?}, glupload caps {:?}, GL platform {:?}",
                    self.current,
                    diagnostics.decoder,
                    diagnostics.glupload_caps,
                    diagnostics.gl_platform
                ),
                PlayerEvent::Eos => self.end_of_stream(),
                PlayerEvent::Error(msg) => {
                    eprintln!("bus: player error: {msg}");
                    // The player dropped its flight and its source; the next
                    // seek reloads.
                    self.reset_skip();
                    self.reset_slot();
                    self.loaded = false;
                    self.publish_position();
                    self.set_playing(false);
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

    /// Issues a seek request and publishes its target first — so a load's
    /// new source index is out before the pipeline leaves READY.
    pub(super) fn request(&mut self, uri: &str, secs: f64, accurate: bool, origin: Origin) {
        self.target_secs = Some(secs);
        self.outstanding += 1;
        self.publish_position();
        let events = self.player.seek_to(uri, secs, accurate, origin);
        self.player_events(events);
    }

    fn request_ended(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
        if self.outstanding == 0 {
            self.target_secs = None;
            self.publish_position();
        }
    }

    /// Drops the player's flight and pending request.
    pub(super) fn reset_slot(&mut self) {
        self.player.clear();
        self.outstanding = 0;
        self.target_secs = None;
    }

    /// Drops the loaded source entirely, so no stale frame stays up: for no
    /// sources, or a current source that is missing.
    pub(super) fn unload(&mut self) {
        self.reset_skip();
        self.reset_slot();
        self.player.unload();
        self.loaded = false;
        if self.playing {
            self.set_playing(false);
        }
    }

    pub(super) fn publish_position(&self) {
        let target_abs = self.open.as_ref().and_then(|open| {
            self.target_secs
                .map(|secs| open.project.abs_seconds(self.current, secs))
        });
        self.emit(Event::Position {
            source_index: self.current,
            target_abs,
        });
    }
}
