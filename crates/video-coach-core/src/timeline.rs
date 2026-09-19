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
    clip.debug_assert_sorted_events();

    let mut source = clip.start_source_seconds;
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
            // Anchors are assigned raw, exactly as the segment builder does.
            EventKind::Play { source_time } => {
                rate = 1.0;
                source = source_time;
            }
            EventKind::Pause { source_time } => {
                rate = 0.0;
                source = source_time;
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
pub fn playback_segments(clip: &Clip, source_duration: f64) -> Vec<PlaybackSegment> {
    clip.debug_assert_sorted_events();

    let mut segments: Vec<PlaybackSegment> = Vec::new();
    let mut source_cursor = clip.start_source_seconds;
    let mut record_cursor = 0.0_f64;
    let mut rate = 1.0_f64;

    // Applied as a cap (`min`) on freeze anchors only, with no lower bound —
    // a negative pause anchor yields a negative freeze anchor, matching the
    // original. `max(0.0)` matters for sub-50 ms sources, which is exactly
    // what synthetic test fixtures are.
    let freeze_max_source = (source_duration - FREEZE_EOF_BACKOFF).max(0.0);

    let emit = |record_end: f64,
                segments: &mut Vec<PlaybackSegment>,
                source_cursor: &mut f64,
                record_cursor: &mut f64,
                rate: f64| {
        let dur = record_end - *record_cursor;
        // NOTE: the early return also skips the `record_cursor` update. Hoisting
        // that assignment out of the guard — the natural refactor — changes
        // behavior for two events sharing a `record_time`.
        if dur <= 0.0 {
            return;
        }

        if rate == 1.0 {
            // Source advances here. If it would read past the end, split into a
            // `Play` tail covering the available source plus a `Freeze` on the
            // last frame for whatever record time remains — mirroring a player,
            // which holds the last decoded frame rather than showing nothing.
            let available = (source_duration - *source_cursor).max(0.0);
            let play_dur = dur.min(available);
            if play_dur > 0.0 {
                segments.push(PlaybackSegment {
                    kind: SegmentKind::Play,
                    source_start: *source_cursor,
                    out_duration: play_dur,
                });
                *source_cursor += play_dur;
            }
            let freeze_dur = dur - play_dur;
            if freeze_dur > 0.0 {
                segments.push(PlaybackSegment {
                    kind: SegmentKind::Freeze,
                    source_start: source_cursor.min(freeze_max_source),
                    out_duration: freeze_dur,
                });
            }
        } else {
            segments.push(PlaybackSegment {
                kind: SegmentKind::Freeze,
                source_start: source_cursor.min(freeze_max_source),
                out_duration: dur,
            });
        }
        *record_cursor = record_end;
    };

    for ev in &clip.events {
        match ev.kind {
            EventKind::Play { source_time } => {
                emit(
                    ev.record_time,
                    &mut segments,
                    &mut source_cursor,
                    &mut record_cursor,
                    rate,
                );
                rate = 1.0;
                source_cursor = source_time;
            }
            EventKind::Pause { source_time } => {
                emit(
                    ev.record_time,
                    &mut segments,
                    &mut source_cursor,
                    &mut record_cursor,
                    rate,
                );
                rate = 0.0;
                source_cursor = source_time;
            }
            EventKind::Skip { delta } => {
                emit(
                    ev.record_time,
                    &mut segments,
                    &mut source_cursor,
                    &mut record_cursor,
                    rate,
                );
                source_cursor = clamp_source(source_cursor + delta, source_duration);
            }
            EventKind::Stroke(_)
            | EventKind::ClearAll
            | EventKind::Zoom(_)
            | EventKind::Unknown(_) => {}
        }
    }
    emit(
        clip.recording_duration,
        &mut segments,
        &mut source_cursor,
        &mut record_cursor,
        rate,
    );

    segments
}
