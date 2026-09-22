//! Player highlights: the stored shape, the mutators, what shows at an
//! instant, and the drawing geometry (spec H1, H2, H4).
//!
//! All tests here are **new**: the macOS app has no highlights.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::compilation_schedule;
use video_coach_core::highlight::{
    highlight_shapes, highlights_at, HighlightEdit, HighlightError, HighlightKey, NormRect,
    PlayerHighlight, SINGLE_KEY_SPAN,
};
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Clip, Project, SourceRef};
use video_coach_core::stroke::Rgba;
use video_coach_core::zoom::Zoom;

const GREEN: Rgba = Rgba {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

fn rect(x: f64, y: f64, w: f64, h: f64) -> NormRect {
    NormRect { x, y, w, h }
}

fn key(source_seconds: f64, rect: NormRect) -> HighlightKey {
    HighlightKey {
        source_seconds,
        rect,
        tracked: false,
    }
}

/// A highlight on source 0 with `keys`, built the way the bus will: one
/// `set_highlight_key` per key.
fn project_with_keys(keys: &[HighlightKey]) -> (Project, Uuid) {
    let mut p = Project::new("p");
    p.source_videos.push(SourceRef {
        relative_path: "a.mp4".into(),
        display_name: "a".into(),
        duration_seconds: 1000.0,
        display_aspect: 16.0 / 9.0,
    });
    let id = Uuid::from_u128(7);
    for k in keys {
        p.set_highlight_key(id, 0, GREEN, *k).unwrap();
    }
    (p, id)
}

// ----------------------------------------------------------- the stored shape

/// The on-disk shape. A round trip can't catch a wrong field name; this can.
/// Nothing here is defaulted: every field of a new struct is required (spec
/// F2).
#[test]
fn a_highlight_has_the_expected_wire_shape() {
    let h = PlayerHighlight {
        id: Uuid::nil(),
        source_index: 1,
        color: GREEN,
        label: "#7".into(),
        keys: vec![key(12.5, rect(0.1, 0.2, 0.3, 0.4))],
    };
    let text = serde_json::to_string(&h).unwrap();
    assert_eq!(
        text,
        r##"{"id":"00000000-0000-0000-0000-000000000000","sourceIndex":1,"color":{"r":0.0,"g":1.0,"b":0.0,"a":1.0},"label":"#7","keys":[{"sourceSeconds":12.5,"rect":{"x":0.1,"y":0.2,"w":0.3,"h":0.4},"tracked":false}]}"##
    );
    assert_eq!(serde_json::from_str::<PlayerHighlight>(&text).unwrap(), h);
}

/// `tracked` ships with the struct in P2, where it is always `false`, so P6
/// needs no format change. It is required, like every field of a new struct.
#[test]
fn a_key_without_tracked_is_malformed() {
    let json = r#"{"sourceSeconds":1.0,"rect":{"x":0.0,"y":0.0,"w":0.1,"h":0.1}}"#;
    assert!(serde_json::from_str::<HighlightKey>(json).is_err());
}

// --------------------------------------------------------------- the mutators

#[test]
fn the_first_key_creates_the_highlight() {
    let (p, id) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
    assert_eq!(p.player_highlights.len(), 1);
    let h = &p.player_highlights[0];
    assert_eq!((h.id, h.source_index, h.color), (id, 0, GREEN));
    assert_eq!(h.label, "");
    assert_eq!(h.keys.len(), 1);
}

#[test]
fn keys_are_kept_sorted() {
    let (p, _) = project_with_keys(&[
        key(30.0, rect(0.3, 0.0, 0.1, 0.1)),
        key(10.0, rect(0.1, 0.0, 0.1, 0.1)),
        key(20.0, rect(0.2, 0.0, 0.1, 0.1)),
    ]);
    let times: Vec<f64> = p.player_highlights[0]
        .keys
        .iter()
        .map(|k| k.source_seconds)
        .collect();
    assert_eq!(times, [10.0, 20.0, 30.0]);
}

/// Keys are placed at the displayed frame's stream time, so the same frame
/// gives the same number and no tolerance is needed.
#[test]
fn a_second_key_at_the_same_time_replaces_the_first() {
    let (p, _) = project_with_keys(&[
        key(10.0, rect(0.1, 0.1, 0.2, 0.2)),
        key(10.0, rect(0.5, 0.5, 0.1, 0.1)),
    ]);
    let keys = &p.player_highlights[0].keys;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].rect, rect(0.5, 0.5, 0.1, 0.1));
}

/// A highlight is on one video: a key placed while another source is on screen
/// belongs to a new highlight, not this one.
#[test]
fn a_key_on_another_source_is_refused() {
    let (mut p, id) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
    assert_eq!(
        p.set_highlight_key(id, 1, GREEN, key(11.0, rect(0.1, 0.1, 0.2, 0.2))),
        Err(HighlightError::OtherSource)
    );
    assert_eq!(p.player_highlights[0].keys.len(), 1);
}

#[test]
fn deleting_the_last_key_deletes_the_highlight() {
    let (mut p, id) = project_with_keys(&[
        key(10.0, rect(0.1, 0.1, 0.2, 0.2)),
        key(20.0, rect(0.3, 0.1, 0.2, 0.2)),
    ]);
    p.delete_highlight_key(id, 20.0);
    assert_eq!(p.player_highlights[0].keys.len(), 1);
    p.delete_highlight_key(id, 10.0);
    assert!(p.player_highlights.is_empty());
}

/// By exact equality, like the replace: a key at any other time is left alone.
#[test]
fn deleting_a_key_that_is_not_there_changes_nothing() {
    let (mut p, id) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
    p.delete_highlight_key(id, 10.5);
    p.delete_highlight_key(Uuid::from_u128(99), 10.0);
    assert_eq!(p.player_highlights[0].keys.len(), 1);
}

#[test]
fn deleting_a_highlight_removes_it() {
    let (mut p, id) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
    p.delete_highlight(id);
    assert!(p.player_highlights.is_empty());
}

/// A shirt number is shown as one: "7" is stored as "#7". Anything else is
/// just trimmed, so a name stays a name.
#[test]
fn a_label_of_digits_gets_a_hash() {
    let cases = [
        ("7", "#7"),
        ("  7 ", "#7"),
        ("10", "#10"),
        ("#7", "#7"),
        ("Keeper", "Keeper"),
        ("  left back ", "left back"),
        ("7a", "7a"),
        ("", ""),
        ("   ", ""),
    ];
    for (typed, shown) in cases {
        let (mut p, id) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
        p.edit_highlight(id, HighlightEdit::Label(typed.into()));
        assert_eq!(p.player_highlights[0].label, shown, "typed {typed:?}");
    }
}

/// The stored value is the colour, not the pen, as for a stroke.
#[test]
fn a_highlight_can_be_recoloured() {
    let (mut p, id) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
    p.edit_highlight(id, HighlightEdit::Color(Rgba::RED));
    assert_eq!(p.player_highlights[0].color, Rgba::RED);
}

// -------------------------------------------------------------- highlights_at

#[test]
fn nothing_shows_on_another_source_or_outside_the_range() {
    let (p, _) = project_with_keys(&[
        key(10.0, rect(0.1, 0.1, 0.2, 0.2)),
        key(20.0, rect(0.3, 0.1, 0.2, 0.2)),
    ]);
    assert!(highlights_at(&p.player_highlights, 1, 15.0).is_empty());
    assert!(highlights_at(&p.player_highlights, 0, 9.999).is_empty());
    assert!(highlights_at(&p.player_highlights, 0, 20.001).is_empty());
}

/// With two or more keys the range is `[first, last]`, and between two keys
/// the rect is linearly interpolated. Every number here is dyadic, so the
/// interpolation is exact rather than approximately equal.
#[test]
fn the_rect_is_interpolated_between_the_keys() {
    let (p, id) = project_with_keys(&[
        key(10.0, rect(0.0, 0.0, 0.25, 0.5)),
        key(20.0, rect(0.5, 0.25, 0.5, 1.0)),
        key(30.0, rect(0.5, 0.25, 0.5, 1.0)),
    ]);
    let at = |t| highlights_at(&p.player_highlights, 0, t);

    let ends = at(10.0);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].id, id);
    assert_eq!(ends[0].color, GREEN);
    assert_eq!(ends[0].rect, rect(0.0, 0.0, 0.25, 0.5));
    assert_eq!(at(30.0)[0].rect, rect(0.5, 0.25, 0.5, 1.0));

    // Midway between the first two keys: every component halfway.
    assert_eq!(at(15.0)[0].rect, rect(0.25, 0.125, 0.375, 0.75));
    // A quarter of the way, and inside the second span, which holds.
    assert_eq!(at(12.5)[0].rect, rect(0.125, 0.0625, 0.3125, 0.625));
    assert_eq!(at(25.0)[0].rect, rect(0.5, 0.25, 0.5, 1.0));
}

/// A highlight with a single key is "at a timestamp": it holds its box for
/// [`SINGLE_KEY_SPAN`], centred on the key, so it shows for the whole of a
/// commentary pause on that frame and for a readable second on playthrough.
#[test]
fn a_single_key_holds_for_its_span_inclusive() {
    let (p, _) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
    let half = SINGLE_KEY_SPAN / 2.0;
    let at = |t| highlights_at(&p.player_highlights, 0, t);
    assert_eq!(at(10.0)[0].rect, rect(0.1, 0.1, 0.2, 0.2));
    // The edges are included, and nothing past them.
    assert_eq!(at(10.0 - half).len(), 1);
    assert_eq!(at(10.0 + half).len(), 1);
    assert!(at(10.0 - half - 1e-9).is_empty());
    assert!(at(10.0 + half + 1e-9).is_empty());
}

#[test]
fn the_label_comes_along() {
    let (mut p, id) = project_with_keys(&[key(10.0, rect(0.1, 0.1, 0.2, 0.2))]);
    p.edit_highlight(id, HighlightEdit::Label("7".into()));
    assert_eq!(highlights_at(&p.player_highlights, 0, 10.0)[0].label, "#7");
}

// ---------------------------------------------------------- the freeze test

fn clip_with_events(events: Vec<CommentaryEvent>, recording_duration: f64) -> Clip {
    Clip {
        id: Uuid::new_v4(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: 100.0,
        recording_duration,
        recording_filename: "c.mkv".into(),
        events,
        show_pip: true,
        sort_index: 0,
        created_at: "2026-09-22T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// **Holds through a freeze.** A highlight is keyed by the *displayed frame's*
/// source time, so a clip that pauses for 20 s shows the same ring on every
/// frame of the pause — the drift BACKLOG #27 describes for the match clock,
/// avoided here by construction rather than by a per-clip constant.
#[test]
fn a_ring_holds_through_a_commentary_pause() {
    let (mut p, _) = project_with_keys(&[
        key(100.0, rect(0.0, 0.0, 0.2, 0.2)),
        key(140.0, rect(0.8, 0.0, 0.2, 0.2)),
    ]);
    p.clips.push(clip_with_events(
        vec![
            CommentaryEvent::new(2.0, EventKind::Pause { source_time: 102.0 }),
            CommentaryEvent::new(22.0, EventKind::Play { source_time: 102.0 }),
        ],
        30.0,
    ));

    let compilation = compilation_schedule(&p, &ExportTarget::AllClips);
    let entry = &compilation.plan.entries[0];
    let shape_at = |record_time: f64| {
        let frame = compilation.frames[entry.start_frame + (record_time * 30.0).round() as usize];
        highlight_shapes(
            &p.player_highlights,
            entry.source_index,
            frame.source_time,
            frame.zoom,
            1920.0,
            1080.0,
        )
    };

    let held = shape_at(2.0);
    assert_eq!(held.len(), 1);
    // Every frame of the 20 s pause: the same shape.
    for n in 0..=(20 * 30) {
        assert_eq!(shape_at(2.0 + f64::from(n) / 30.0), held, "frame {n}");
    }
    // Once it plays on, the ring moves with the footage again.
    assert_ne!(shape_at(27.0), held);
}

// ------------------------------------------------------------ the geometry

/// The picture *is* the fitted source, so a centred box maps to the picture's
/// centre and the ring sits on its bottom edge, 1.4× as wide as the box.
#[test]
fn a_centred_box_maps_to_the_centre_of_the_picture() {
    let (p, id) = project_with_keys(&[key(10.0, rect(0.4, 0.4, 0.2, 0.2))]);
    let shapes = highlight_shapes(&p.player_highlights, 0, 10.0, Zoom::IDENTITY, 1000.0, 500.0);
    assert_eq!(shapes.len(), 1);
    let s = &shapes[0];
    assert_eq!(s.id, id);
    assert_eq!(
        (s.rect.x, s.rect.y, s.rect.w, s.rect.h),
        (400.0, 200.0, 200.0, 100.0)
    );
    // Centred on the box's bottom edge; 1.4 × the box's width, 0.35 × that tall.
    assert_eq!(s.ellipse, (500.0, 300.0, 140.0, 49.0));
    // Scales with the picture's height only, like a pen.
    assert_eq!(s.width, 0.005 * 500.0);
    assert_eq!(
        highlight_shapes(
            &p.player_highlights,
            0,
            10.0,
            Zoom::IDENTITY,
            2000.0,
            1000.0
        )[0]
        .width,
        0.005 * 1000.0
    );
}

/// Under a zoom the ring moves with the picture: `highlight_shapes` maps
/// through the same `Zoom::transform` the picture itself is drawn with, so no
/// second mapping can drift from it.
#[test]
fn a_box_moves_with_the_zoom() {
    let (p, _) = project_with_keys(&[key(10.0, rect(0.4, 0.4, 0.2, 0.2))]);
    let zoom = Zoom::new(2.0, 0.0, 0.0);
    let s = &highlight_shapes(&p.player_highlights, 0, 10.0, zoom, 1000.0, 500.0)[0];
    // A centred box stays centred, at twice the size.
    assert_eq!(
        (s.rect.x, s.rect.y, s.rect.w, s.rect.h),
        (300.0, 150.0, 400.0, 200.0)
    );

    // And off centre it follows `transform` exactly.
    let zoom = Zoom::new(2.0, 0.1, -0.05);
    let t = zoom.transform(1000.0, 500.0, 1000.0, 500.0);
    let s = &highlight_shapes(&p.player_highlights, 0, 10.0, zoom, 1000.0, 500.0)[0];
    assert_eq!((s.rect.x, s.rect.y), t.apply(0.4 * 1000.0, 0.4 * 500.0));
}

/// **The zoom round trip.** A box dragged at picture pixels goes to source
/// space through `Zoom::source_point` (what the H tool does, Task 2.5) and
/// comes back from `highlight_shapes` at the same pixels.
#[test]
fn a_box_drawn_while_zoomed_comes_back_where_it_was_drawn() {
    let (pw, ph) = (1600.0, 900.0);
    let zoom = Zoom::new(2.5, 0.12, -0.08).clamped();
    // The drag, in picture pixels.
    let (x0, y0, x1, y1) = (520.0, 300.0, 680.0, 660.0);
    let corner = |x: f64, y: f64| zoom.source_point(x / pw, y / ph);
    let (sx0, sy0) = corner(x0, y0);
    let (sx1, sy1) = corner(x1, y1);

    let mut p = Project::new("p");
    p.set_highlight_key(
        Uuid::from_u128(1),
        0,
        GREEN,
        key(10.0, rect(sx0, sy0, sx1 - sx0, sy1 - sy0)),
    )
    .unwrap();

    let s = &highlight_shapes(&p.player_highlights, 0, 10.0, zoom, pw, ph)[0];
    for (got, want) in [
        (s.rect.x, x0),
        (s.rect.y, y0),
        (s.rect.w, x1 - x0),
        (s.rect.h, y1 - y0),
    ] {
        assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
    }
}

#[test]
fn shapes_come_out_in_stored_order_and_skip_other_sources() {
    let mut p = Project::new("p");
    p.set_highlight_key(
        Uuid::from_u128(1),
        0,
        GREEN,
        key(10.0, rect(0.0, 0.0, 0.1, 0.1)),
    )
    .unwrap();
    p.set_highlight_key(
        Uuid::from_u128(2),
        1,
        GREEN,
        key(10.0, rect(0.0, 0.0, 0.1, 0.1)),
    )
    .unwrap();
    p.set_highlight_key(
        Uuid::from_u128(3),
        0,
        Rgba::RED,
        key(10.0, rect(0.5, 0.5, 0.1, 0.1)),
    )
    .unwrap();
    let ids: Vec<Uuid> =
        highlight_shapes(&p.player_highlights, 0, 10.0, Zoom::IDENTITY, 100.0, 100.0)
            .iter()
            .map(|s| s.id)
            .collect();
    assert_eq!(ids, [Uuid::from_u128(1), Uuid::from_u128(3)]);
}
