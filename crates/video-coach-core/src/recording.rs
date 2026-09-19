//! The commentary event log of one recording in progress.
//!
//! Ported from macOS `RecordingController`, renamed because it controls
//! nothing: it only turns caller-captured moments into `CommentaryEvent`s.
//! Deviations from the original (Phase 4 spec R8):
//!
//! - **Caller-supplied times.** Every append takes the `host_ns` the UI
//!   captured at the input event, on the same clock as `t0_ns` (the capture
//!   pipeline's base time). Swift read an injected clock inside some appends,
//!   which is queue delay by construction.
//! - **Construction writes the initial events.** `zoom @0` then `pause @0` is
//!   an invariant of the log, not two calls the caller must remember in order.
//! - **Record times never go backwards.** Each is clamped to at least the last
//!   event's. That keeps the log sorted when a `host_ns` captured just before
//!   `t0_ns` arrives, and when the zoom anchor would land before an earlier
//!   event — both of which Swift let through.

use uuid::Uuid;

use crate::event::{debug_assert_sorted, CommentaryEvent, EventKind};
use crate::stroke::Stroke;
use crate::zoom::Zoom;

/// A clip whose recording is running: everything `Project::add_recorded_clip`
/// needs that is known at the moment recording starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingClip {
    pub id: Uuid,
    pub source_index: usize,
    pub start_source_seconds: f64,
}

/// A zoom value this far (seconds) after the previous capture is preceded by
/// an anchor holding the previous value, so replay's lerp snaps at the change
/// instead of drifting through the quiet period.
const ZOOM_ANCHOR_GAP: f64 = 0.1;

/// How far before a change the zoom anchor sits.
const ZOOM_ANCHOR_LEAD: f64 = 0.001;

#[derive(Debug)]
pub struct RecordingLog {
    t0_ns: u64,
    /// Never empty: construction writes the initial zoom and pause.
    events: Vec<CommentaryEvent>,
    /// The last zoom logged, for dedupe and the anchor's value.
    last_zoom: Zoom,
    /// Record time of the last zoom logged (not of the last one offered: a
    /// deduped value does not count as a capture).
    last_zoom_time: f64,
}

impl RecordingLog {
    /// Starts a log at `t0_ns` with the inherited `zoom` and a pause at the
    /// source position the recording started from, both at record time 0.
    ///
    /// The pause is pinned to 0, not to "now": the camera's first frame lands
    /// well after time 0 (Phase 4 R5), and the pause covers that lead-in so
    /// replay holds the start frame rather than inventing a play segment.
    pub fn new(t0_ns: u64, zoom: Zoom, start_source_seconds: f64) -> Self {
        RecordingLog {
            t0_ns,
            events: vec![
                CommentaryEvent::new(0.0, EventKind::Zoom(zoom)),
                CommentaryEvent::new(
                    0.0,
                    EventKind::Pause {
                        source_time: start_source_seconds,
                    },
                ),
            ],
            last_zoom: zoom,
            last_zoom_time: 0.0,
        }
    }

    /// Playback started at `host_ns`, from `source_seconds` in the source.
    pub fn play(&mut self, host_ns: u64, source_seconds: f64) {
        let t = self.record_time(host_ns);
        self.push(
            t,
            EventKind::Play {
                source_time: source_seconds,
            },
        );
    }

    /// Playback paused at `host_ns`, holding `source_seconds`.
    pub fn pause(&mut self, host_ns: u64, source_seconds: f64) {
        let t = self.record_time(host_ns);
        self.push(
            t,
            EventKind::Pause {
                source_time: source_seconds,
            },
        );
    }

    /// The coach skipped by `delta`. The **requested** delta is logged, as
    /// macOS did; replay clamps it within the source.
    pub fn skip(&mut self, host_ns: u64, delta: f64) {
        let t = self.record_time(host_ns);
        self.push(t, EventKind::Skip { delta });
    }

    /// The zoom changed to `zoom` at `host_ns`.
    ///
    /// Every distinct value is kept. macOS once throttled these and replay
    /// then lagged a pan made while drawing, leaving the drawing off the ball;
    /// only a value equal to the last one is dropped.
    pub fn zoom(&mut self, host_ns: u64, zoom: Zoom) {
        if zoom == self.last_zoom {
            return;
        }
        let t = self.record_time(host_ns);
        if t - self.last_zoom_time > ZOOM_ANCHOR_GAP {
            // `record_time` has already clamped `t` to the last event, but the
            // lead can step back past it; clamp again so the log stays sorted.
            let anchor = (t - ZOOM_ANCHOR_LEAD).max(self.last_time());
            self.push(anchor, EventKind::Zoom(self.last_zoom));
        }
        self.push(t, EventKind::Zoom(zoom));
        self.last_zoom = zoom;
        self.last_zoom_time = t;
    }

    /// A stroke that finished at `host_ns`.
    ///
    /// `host_ns` is the moment of the stroke's **last** point, not its first:
    /// replay back-computes the start from the point times, and auto-clear
    /// counts from here (`stroke_replay`).
    ///
    /// So if the monotonic clamp in `record_time` ever fired for a
    /// stroke, the back-computed start would move with it and the whole
    /// drawing would shift. It can't today — the UI captures `host_ns` at the
    /// pen-up, after every event it could be clamped against — and a guard
    /// here would only hide that.
    pub fn stroke(&mut self, host_ns: u64, stroke: Stroke) {
        let t = self.record_time(host_ns);
        self.push(t, EventKind::Stroke(stroke));
    }

    /// The coach wiped every drawing at `host_ns`.
    pub fn clear_all(&mut self, host_ns: u64) {
        let t = self.record_time(host_ns);
        self.push(t, EventKind::ClearAll);
    }

    /// The finished log, sorted by record time.
    pub fn finish(self) -> Vec<CommentaryEvent> {
        debug_assert_sorted(&self.events);
        self.events
    }

    /// Seconds from `t0_ns` to `host_ns`, never before the last event.
    fn record_time(&self, host_ns: u64) -> f64 {
        let t = (host_ns as i128 - self.t0_ns as i128) as f64 / 1e9;
        t.max(self.last_time())
    }

    fn last_time(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.record_time)
    }

    fn push(&mut self, record_time: f64, kind: EventKind) {
        self.events.push(CommentaryEvent::new(record_time, kind));
    }
}
