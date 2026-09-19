//! The commentary event log.
//!
//! One `CommentaryEvent` per thing the coach did while recording, timestamped
//! in **record time** — seconds since the first frame of the commentary
//! recording. The log is the only record of what the source video was doing;
//! `timeline::playback_segments` replays it to reconstruct the edit.
//!
//! The log is assumed sorted by `record_time`. An unsorted log is an upstream
//! bug, not something readers defend against.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::stroke::Stroke;
use crate::zoom::Zoom;

/// One recorded action, at `record_time` seconds into the recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentaryEvent {
    pub record_time: f64,
    pub kind: EventKind,
}

impl CommentaryEvent {
    pub fn new(record_time: f64, kind: EventKind) -> Self {
        // A non-finite value here is silent corruption, not a loud failure:
        // serde_json writes it as `null`, which no f64 field accepts on the way
        // back, so the event re-decodes as `Unknown` and disappears from replay.
        // Catch it at the producer, where the value is still traceable.
        debug_assert!(
            record_time.is_finite(),
            "record_time must be finite, got {record_time}"
        );
        CommentaryEvent { record_time, kind }
    }
}

/// Debug-time guard: every reader of an event log assumes it is sorted by
/// `record_time`.
///
/// An unsorted log is an upstream bug. `playback_segments` silently loses a
/// segment for an out-of-order event, and `zoom_at` stops its scan at the first
/// keyframe past the target — both fail quietly, which is why this is asserted
/// rather than defended against.
///
/// Takes a slice rather than a `Clip` so `zoom_at`, which works on the event
/// list alone, can call it too.
#[inline]
pub(crate) fn debug_assert_sorted(events: &[CommentaryEvent]) {
    debug_assert!(
        events
            .windows(2)
            .all(|w| w[0].record_time <= w[1].record_time),
        "commentary event log must be sorted by record_time"
    );
}

/// What happened at a given record time.
///
/// `Play` and `Pause` carry the source playhead captured at the keystroke
/// moment. That anchor **overrides** the wall-clock computation in
/// `timeline`: player latency and frame-boundary rounding make a computed
/// cursor drift by tens of milliseconds, and the captured value pins the
/// freeze frame to what was actually on screen.
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    Play {
        source_time: f64,
    },
    Pause {
        source_time: f64,
    },
    Skip {
        delta: f64,
    },
    Stroke(Stroke),
    ClearAll,
    Zoom(Zoom),
    /// An event kind this build does not recognize.
    ///
    /// The original JSON is preserved verbatim and re-emitted on save, so a
    /// newer build's events survive a round-trip through an older one. The
    /// macOS original wrote `{}` here, keeping the event but destroying its
    /// payload; this does not.
    Unknown(serde_json::Value),
}

/// The kinds this build understands.
///
/// Split out so `EventKind`'s codec can try this first and fall back to
/// `Unknown` — `#[serde(other)]` cannot carry a payload, which is the only
/// reason `EventKind` needs a hand-written codec at all.
///
/// `ClearAll {}` is an **empty struct variant**, not a unit variant: serde
/// emits `{"clearAll":{}}` for the former and the bare string `"clearAll"`
/// for the latter, and the former is the shape the format uses.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
enum KnownKind {
    Play { source_time: f64 },
    Pause { source_time: f64 },
    Skip { delta: f64 },
    Stroke(Stroke),
    ClearAll {},
    Zoom(Zoom),
}

impl From<KnownKind> for EventKind {
    fn from(k: KnownKind) -> Self {
        match k {
            KnownKind::Play { source_time } => EventKind::Play { source_time },
            KnownKind::Pause { source_time } => EventKind::Pause { source_time },
            KnownKind::Skip { delta } => EventKind::Skip { delta },
            KnownKind::Stroke(s) => EventKind::Stroke(s),
            KnownKind::ClearAll {} => EventKind::ClearAll,
            KnownKind::Zoom(z) => EventKind::Zoom(z),
        }
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        // One match, with the `Unknown` arm inline. Routing through an
        // `Option<KnownKind>` would throw away the information the second match
        // needs and force an `unreachable!()` to put it back.
        match self {
            EventKind::Play { source_time } => KnownKind::Play {
                source_time: *source_time,
            }
            .serialize(ser),
            EventKind::Pause { source_time } => KnownKind::Pause {
                source_time: *source_time,
            }
            .serialize(ser),
            EventKind::Skip { delta } => KnownKind::Skip { delta: *delta }.serialize(ser),
            // Borrowed so the point vector is not cloned on the export hot path.
            EventKind::Stroke(s) => BorrowedStroke { stroke: s }.serialize(ser),
            EventKind::ClearAll => KnownKind::ClearAll {}.serialize(ser),
            EventKind::Zoom(z) => KnownKind::Zoom(*z).serialize(ser),
            EventKind::Unknown(v) => v.serialize(ser),
        }
    }
}

/// Serializes as `{"stroke": {...}}` without cloning the stroke.
#[derive(Serialize)]
struct BorrowedStroke<'a> {
    stroke: &'a Stroke,
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(de)?;
        // Try the known shapes first. A *malformed* known key — `{"play":{}}`
        // with no `sourceTime` — must also fall through to Unknown rather than
        // erroring, or the forward-compatibility the variant exists for is lost
        // the moment a future build changes a payload.
        match KnownKind::deserialize(&value) {
            Ok(known) => Ok(known.into()),
            Err(_) => Ok(EventKind::Unknown(value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stroke::{Rgba, StrokePoint};
    use serde_json::json;
    use uuid::Uuid;

    fn rt(kind: EventKind) -> EventKind {
        let s = serde_json::to_string(&kind).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    /// The literal wire shape. Enum-level `rename_all` renames variants only —
    /// the inner `source_time` needs `rename_all_fields`, and getting that
    /// wrong silently emits snake_case.
    #[test]
    fn play_has_the_expected_wire_shape() {
        let s = serde_json::to_string(&EventKind::Play { source_time: 1.5 }).unwrap();
        assert_eq!(s, r#"{"play":{"sourceTime":1.5}}"#);
    }

    /// `ClearAll` must emit an empty object, not the bare string a unit
    /// variant would produce.
    #[test]
    fn clear_all_emits_an_empty_object() {
        let s = serde_json::to_string(&EventKind::ClearAll).unwrap();
        assert_eq!(s, r#"{"clearAll":{}}"#);
        assert_eq!(rt(EventKind::ClearAll), EventKind::ClearAll);
    }

    #[test]
    fn skip_and_pause_round_trip() {
        assert_eq!(
            rt(EventKind::Skip { delta: -3.0 }),
            EventKind::Skip { delta: -3.0 }
        );
        assert_eq!(
            rt(EventKind::Pause { source_time: 9.25 }),
            EventKind::Pause { source_time: 9.25 }
        );
    }

    /// A round-trip test cannot catch a wrong field name — it serializes
    /// whatever was constructed and reads it back. `Zoom` shipped as snake_case
    /// for exactly that reason, so both payload shapes are pinned literally.
    #[test]
    fn zoom_and_stroke_have_the_expected_wire_shapes() {
        let z = serde_json::to_string(&EventKind::Zoom(Zoom::new(2.0, 0.1, -0.05))).unwrap();
        assert_eq!(z, r#"{"zoom":{"scale":2.0,"panX":0.1,"panY":-0.05}}"#);

        let s = serde_json::to_string(&EventKind::Stroke(Stroke {
            id: Uuid::nil(),
            color: Rgba {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            line_width: 0.006,
            points: vec![StrokePoint {
                x: 0.25,
                y: 0.5,
                t: 0.0,
            }],
            auto_clear_after_seconds: Some(3.0),
        }))
        .unwrap();
        assert!(s.contains(r#""lineWidth":0.006"#), "got {s}");
        assert!(s.contains(r#""autoClearAfterSeconds":3.0"#), "got {s}");
    }

    /// Absent and explicit null both decode to `None`. Recorded because it is
    /// the reason no `skip_serializing_if` is used.
    #[test]
    fn absent_and_null_auto_clear_both_decode_to_none() {
        let absent: Stroke = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000000","color":{"r":1,"g":0,"b":0,"a":1},"lineWidth":0.1,"points":[]}"#,
        )
        .unwrap();
        let null: Stroke = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000000","color":{"r":1,"g":0,"b":0,"a":1},"lineWidth":0.1,"points":[],"autoClearAfterSeconds":null}"#,
        )
        .unwrap();
        assert_eq!(absent.auto_clear_after_seconds, None);
        assert_eq!(null.auto_clear_after_seconds, None);
    }

    /// A camelCase zoom must decode as a real zoom, not fall through to
    /// `Unknown` — where it would vanish from replay and be re-emitted verbatim
    /// forever with no error.
    #[test]
    fn a_camel_case_zoom_does_not_decode_as_unknown() {
        let v = json!({"zoom": {"scale": 2.0, "panX": 0.1, "panY": -0.05}});
        let decoded: EventKind = serde_json::from_value(v).unwrap();
        assert_eq!(decoded, EventKind::Zoom(Zoom::new(2.0, 0.1, -0.05)));
    }

    #[test]
    fn zoom_and_stroke_round_trip() {
        let z = EventKind::Zoom(Zoom::new(2.0, 0.1, -0.05));
        assert_eq!(rt(z.clone()), z);

        let s = EventKind::Stroke(Stroke {
            id: Uuid::nil(),
            color: Rgba::RED,
            line_width: 0.006,
            points: vec![StrokePoint {
                x: 0.1,
                y: 0.2,
                t: 0.0,
            }],
            auto_clear_after_seconds: Some(3.0),
        });
        assert_eq!(rt(s.clone()), s);
    }

    /// A future build's event kind survives a round-trip through this one.
    /// macOS kept the event but wrote `{}`, destroying the payload.
    #[test]
    fn unknown_kind_round_trips_verbatim() {
        let raw = json!({"futureKind": {"someField": 42}});
        let decoded: EventKind = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(decoded, EventKind::Unknown(raw.clone()));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), raw);
    }

    /// A malformed *known* key falls through to Unknown rather than erroring,
    /// matching the Swift `try?`-per-branch decoder. Without this, a future
    /// build changing a payload shape breaks older builds outright.
    #[test]
    fn malformed_known_kind_decodes_as_unknown_not_error() {
        let raw = json!({"play": {}});
        let decoded: EventKind = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(decoded, EventKind::Unknown(raw));
    }

    #[test]
    fn event_wraps_kind_with_record_time() {
        let e = CommentaryEvent::new(2.5, EventKind::ClearAll);
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"recordTime":2.5,"kind":{"clearAll":{}}}"#);
        assert_eq!(serde_json::from_str::<CommentaryEvent>(&s).unwrap(), e);
    }
}
