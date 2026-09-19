//! Transport: play/pause, seeks and volume, and the player's events.
//!
//! Task 4a handles these minimally: a skip is a single accurate seek and EOS
//! just pauses. Task 4b adds the skip coordinator, the debounce deadline and
//! EOS advance.

use video_coach_media::{Origin, PlayerEvent};

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

    /// Task 4a: one accurate seek per press. Task 4b routes this through the
    /// skip coordinator.
    pub(super) fn skip(&mut self, delta: f64) {
        let Some(open) = &self.open else {
            return;
        };
        let abs = open.project.abs_seconds(self.current, self.current_secs());
        self.seek_abs(abs + delta, true, Origin::Skip);
    }

    pub(super) fn scrub(&mut self, abs: f64, release: bool) {
        self.seek_abs(abs, release, Origin::Scrub);
    }

    /// Seeks to concat time `abs`, clamped to the timeline. Refused while any
    /// source is missing.
    fn seek_abs(&mut self, abs: f64, accurate: bool, origin: Origin) {
        let Some(open) = &self.open else {
            return;
        };
        if !abs.is_finite() || open.project.source_videos.is_empty() || self.any_missing() {
            return;
        }
        let (index, secs) = open.project.locate(abs);
        // `load` clamps short of the source's end, which for the last source
        // is the end of the timeline.
        self.load(index, secs, accurate, origin);
    }

    /// The skip debounce fired (Task 4b).
    pub(super) fn deadline_passed(&mut self) {}

    pub(super) fn player_events(&mut self, events: Vec<PlayerEvent>) {
        for event in events {
            match event {
                PlayerEvent::SeekDone { .. }
                | PlayerEvent::SeekDisplaced { .. }
                | PlayerEvent::SeekFailed { .. } => self.request_ended(),
                PlayerEvent::Loaded { diagnostics } => eprintln!(
                    "bus: loaded source {}: decoder {:?}, glupload caps {:?}, GL platform {:?}",
                    self.current,
                    diagnostics.decoder,
                    diagnostics.glupload_caps,
                    diagnostics.gl_platform
                ),
                // Task 4b advances to the next source.
                PlayerEvent::Eos => self.set_playing(false),
                PlayerEvent::Error(msg) => {
                    eprintln!("bus: player error: {msg}");
                    self.reset_slot();
                    self.publish_position();
                    self.set_playing(false);
                }
            }
        }
    }

    /// Issues a seek request and publishes its target first.
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
