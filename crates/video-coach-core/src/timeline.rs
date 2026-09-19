//! Replaying the commentary event log into a playback timeline.
//!
//! Two functions answer closely-related questions and are **deliberately not
//! identical** at end-of-source:
//!
//! - [`playback_segments`] is authoritative for **which frame to pull**. It
//!   caps freeze anchors at `source_duration - 0.05` so a pull-based decoder is
//!   never asked for a frame at or past the end of the file.
//! - [`source_time`] is authoritative for **the clock** — it is the sole input
//!   to the scoreboard's match time on both the preview and the export path.
//!
//! The 50 ms cap is a decoder-safety pullback, not a semantic answer, so the
//! two agree to within 50 ms past EOF and exactly everywhere else. That gap is
//! pinned by a test on purpose; do not "fix" one side to match the other.
//!
//! For that claim to hold, **both functions clamp the play/pause anchor to
//! `[0, source_duration]` at assignment.** The Swift original assigns anchors
//! raw, which lets a negative anchor hand the decoder a `source_start` of -5 s
//! and put the two functions seconds apart — the same class of bug the freeze
//! cap prevents at the other end of the range. Clamping changes nothing for an
//! in-range anchor, and nothing for an anchor past the end (which the cap and
//! the available-source floor already handle).

use crate::event::EventKind;
use crate::project::Clip;

/// How the source behaves across one span of the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Source advances at 1x.
    Play,
    /// Source is held on one frame.
    Freeze,
}

/// One span of the recording timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackSegment {
    pub kind: SegmentKind,
    /// Source-video offset at the start of this segment.
    pub source_start: f64,
    /// Duration in the **recording** timeline.
    pub out_duration: f64,
}

/// Freeze anchors are pulled this far back from the end of the source.
///
/// A pull-based compositor asks the decoder for the frame *at* `source_start`,
/// and exactly `source_duration` is past the last frame. 50 ms lands safely on
/// a real sample at any realistic source frame rate (1–120 fps) and is
/// imperceptible against "the actual last frame".
const FREEZE_EOF_BACKOFF: f64 = 0.05;

#[inline]
fn clamp_source(t: f64, source_duration: f64) -> f64 {
    t.clamp(0.0, source_duration.max(0.0))
}

/// The source-video time the coach was looking at, `at_record_time` seconds
/// into the recording.
///
/// Walks the event log applying play/pause/skip. Stroke, zoom and clear-all do
/// not move source time.
///
/// `Play` and `Pause` **anchor** the cursor to the source position captured at
/// the keystroke, overriding the wall-clock computation: player latency and
/// frame-boundary rounding make a computed cursor drift by tens of
/// milliseconds, and the captured value pins the frame to what was on screen.
///
/// Clamping happens at **each mutation**, mirroring [`playback_segments`] — not
/// once on the way out. Placement changes the answer: with a 1000 s source,
/// `skip(+1e6)` then `skip(-10)` gives 990 from a per-mutation clamp and 1000
/// from a return-value clamp.
pub fn source_time(clip: &Clip, at_record_time: f64, source_duration: f64) -> f64 {
    crate::event::debug_assert_sorted(&clip.events);

    let mut source = clamp_source(clip.start_source_seconds, source_duration);
    let mut record_cursor = 0.0;
    let mut rate = 1.0;

    for ev in clip
        .events
        .iter()
        .filter(|e| e.record_time <= at_record_time)
    {
        source = clamp_source(
            source + (ev.record_time - record_cursor) * rate,
            source_duration,
        );
        record_cursor = ev.record_time;
        match ev.kind {
            EventKind::Play { source_time } => {
                rate = 1.0;
                source = clamp_source(source_time, source_duration);
            }
            EventKind::Pause { source_time } => {
                rate = 0.0;
                source = clamp_source(source_time, source_duration);
            }
            EventKind::Skip { delta } => source = clamp_source(source + delta, source_duration),
            EventKind::Stroke(_)
            | EventKind::ClearAll
            | EventKind::Zoom(_)
            | EventKind::Unknown(_) => {}
        }
    }

    clamp_source(
        source + (at_record_time - record_cursor) * rate,
        source_duration,
    )
}

/// Walk the event log into play/freeze segments covering the whole recording.
///
/// Only `Play`, `Pause` and `Skip` split the timeline — those are the events
/// that change `rate` or the source cursor. Zoom, stroke and clear-all must
/// **not** split it: a continuous pinch gesture emits up to ~60 zoom events per
/// second, and splitting on each would explode the segment count into the
/// hundreds for no change in output. Zoom still reaches the compositor through
/// the keyframe lookup, which is independent of segment boundaries.
/// Mutable state of one pass over the event log.
///
/// A struct rather than a closure with four `&mut` parameters: the loop and the
/// emitter both mutate the same cursors, which a closure cannot express without
/// threading every field through the call.
struct Walk {
    segments: Vec<PlaybackSegment>,
    source_cursor: f64,
    record_cursor: f64,
    rate: f64,
    source_duration: f64,
    /// Applied as a cap (`min`) on freeze anchors, with no lower bound needed
    /// now that anchors are clamped at assignment. `max(0.0)` matters for
    /// sub-50 ms sources, which is exactly what synthetic test fixtures are.
    freeze_max_source: f64,
}

impl Walk {
    fn emit(&mut self, record_end: f64) {
        let dur = record_end - self.record_cursor;
        // NOTE: the early return also skips the `record_cursor` update. Hoisting
        // that assignment out of the guard — the natural refactor — changes
        // behavior for two events sharing a `record_time`.
        if dur <= 0.0 {
            return;
        }

        if self.rate == 1.0 {
            // Source advances here. If it would read past the end, split into a
            // `Play` tail covering the available source plus a `Freeze` on the
            // last frame for whatever record time remains — mirroring a player,
            // which holds the last decoded frame rather than showing nothing.
            let available = (self.source_duration - self.source_cursor).max(0.0);
            let play_dur = dur.min(available);
            if play_dur > 0.0 {
                self.segments.push(PlaybackSegment {
                    kind: SegmentKind::Play,
                    source_start: self.source_cursor,
                    out_duration: play_dur,
                });
                self.source_cursor += play_dur;
            }
            let freeze_dur = dur - play_dur;
            if freeze_dur > 0.0 {
                self.segments.push(PlaybackSegment {
                    kind: SegmentKind::Freeze,
                    source_start: self.source_cursor.min(self.freeze_max_source),
                    out_duration: freeze_dur,
                });
            }
        } else {
            self.segments.push(PlaybackSegment {
                kind: SegmentKind::Freeze,
                source_start: self.source_cursor.min(self.freeze_max_source),
                out_duration: dur,
            });
        }
        self.record_cursor = record_end;
    }
}

pub fn playback_segments(clip: &Clip, source_duration: f64) -> Vec<PlaybackSegment> {
    crate::event::debug_assert_sorted(&clip.events);

    let mut w = Walk {
        segments: Vec::new(),
        source_cursor: clamp_source(clip.start_source_seconds, source_duration),
        record_cursor: 0.0,
        rate: 1.0,
        source_duration,
        freeze_max_source: (source_duration - FREEZE_EOF_BACKOFF).max(0.0),
    };

    for ev in &clip.events {
        match ev.kind {
            EventKind::Play { source_time } => {
                w.emit(ev.record_time);
                w.rate = 1.0;
                w.source_cursor = clamp_source(source_time, source_duration);
            }
            EventKind::Pause { source_time } => {
                w.emit(ev.record_time);
                w.rate = 0.0;
                w.source_cursor = clamp_source(source_time, source_duration);
            }
            EventKind::Skip { delta } => {
                w.emit(ev.record_time);
                w.source_cursor = clamp_source(w.source_cursor + delta, source_duration);
            }
            EventKind::Stroke(_)
            | EventKind::ClearAll
            | EventKind::Zoom(_)
            | EventKind::Unknown(_) => {}
        }
    }
    w.emit(clip.recording_duration);

    w.segments
}
