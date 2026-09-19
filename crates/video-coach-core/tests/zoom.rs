//! Zoom clamping, snapping, the surviving transform, and keyframe replay.

use uuid::Uuid;

use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_core::zoom::{zoom_at, Zoom, SNAP_NOTCHES};

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

// ----------------------------------------------------------------- clamping

#[test]
fn scale_floors_at_one_and_caps_at_ten() {
    assert_eq!(Zoom::new(0.2, 0.0, 0.0).clamped().scale, 1.0);
    assert_eq!(Zoom::new(50.0, 0.0, 0.0).clamped().scale, 10.0);
}

#[test]
fn pan_is_forced_to_zero_at_scale_one() {
    let z = Zoom::new(1.0, 0.4, -0.3).clamped();
    assert_eq!((z.scale, z.pan_x, z.pan_y), (1.0, 0.0, 0.0));
}

/// The limit narrows as scale approaches 1. At the limit the visible window's
/// edge lands exactly on the source edge, which is what lets the compositor
/// treat zoom as a plain crop with no edge handling.
#[test]
fn pan_limit_narrows_as_scale_approaches_one() {
    for scale in [1.25, 2.0, 5.0, 10.0] {
        let limit = (scale - 1.0) / (2.0 * scale);
        let z = Zoom::new(scale, 10.0, -10.0).clamped();
        assert!(
            approx(z.pan_x, limit),
            "scale {}: pan_x {} != {}",
            scale,
            z.pan_x,
            limit
        );
        assert!(
            approx(z.pan_y, -limit),
            "scale {}: pan_y {} != {}",
            scale,
            z.pan_y,
            -limit
        );
    }
    // Narrower at lower scale.
    let low = Zoom::new(1.25, 10.0, 0.0).clamped().pan_x;
    let high = Zoom::new(10.0, 10.0, 0.0).clamped().pan_x;
    assert!(low < high);
}

/// At the pan limit the visible window sits exactly inside the source — the
/// property the crop-based compositor relies on.
#[test]
fn at_the_pan_limit_the_window_edge_lands_on_the_source_edge() {
    let z = Zoom::new(4.0, 10.0, 0.0).clamped();
    let (right, _) = z.source_point(1.0, 0.5);
    assert!(
        approx(right, 1.0),
        "window right edge at {right}, expected 1.0"
    );
}

// ----------------------------------------------------------------- snapping
// NOTE: these are NEW. `snapped()` has no test anywhere in the Swift tree.

#[test]
fn snaps_to_a_notch_inside_three_percent_tolerance() {
    assert_eq!(Zoom::new(2.01, 0.1, 0.2).snapped().scale, 2.0);
    assert_eq!(Zoom::new(2.94, 0.0, 0.0).snapped().scale, 3.0);
}

#[test]
fn does_not_snap_outside_tolerance() {
    // 2.5 is 25% from 2.0 and 16% from 3.0 — outside 3% of either.
    assert_eq!(Zoom::new(2.5, 0.0, 0.0).snapped().scale, 2.5);
}

#[test]
fn snapping_preserves_pan() {
    let z = Zoom::new(2.01, 0.11, -0.07).snapped();
    assert_eq!((z.pan_x, z.pan_y), (0.11, -0.07));
}

/// The table's spacing makes the tolerance windows disjoint, so "first match"
/// and "nearest" coincide. If a future notch breaks this, the semantics change
/// silently — so assert the property rather than trusting the table.
#[test]
fn notch_tolerance_windows_are_disjoint() {
    for pair in SNAP_NOTCHES.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        assert!(
            lo + lo * 0.03 < hi - hi * 0.03,
            "notches {lo} and {hi} have overlapping snap windows"
        );
    }
}

// -------------------------------------------------------- cursor anchoring

#[test]
fn zoomed_to_cursor_keeps_the_source_point_under_the_cursor() {
    // Well inside the pan limit at scale 4, so `.clamped()` does not bind —
    // otherwise the round-trip is flaky by construction.
    let start = Zoom::new(2.0, 0.05, -0.02);
    let (cx, cy) = (0.55, 0.48);
    let before = start.source_point(cx, cy);

    let after = start.zoomed_to_cursor(4.0, cx, cy);
    let now = after.source_point(cx, cy);

    assert!(
        approx(before.0, now.0),
        "x drifted: {} -> {}",
        before.0,
        now.0
    );
    assert!(
        approx(before.1, now.1),
        "y drifted: {} -> {}",
        before.1,
        now.1
    );
}

#[test]
fn chained_zooms_preserve_the_cursor_pivot() {
    let (cx, cy) = (0.52, 0.51);
    let mut z = Zoom::new(1.5, 0.0, 0.0);
    let pivot = z.source_point(cx, cy);
    for scale in [2.0, 3.0, 2.5, 4.0] {
        z = z.zoomed_to_cursor(scale, cx, cy);
        let p = z.source_point(cx, cy);
        assert!(
            approx(pivot.0, p.0) && approx(pivot.1, p.1),
            "pivot drifted at scale {scale}"
        );
    }
}

#[test]
fn zooming_out_to_one_returns_identity() {
    assert_eq!(
        Zoom::new(4.0, 0.2, 0.1).zoomed_to_cursor(1.0, 0.3, 0.3),
        Zoom::IDENTITY
    );
}

// ---------------------------------------------------------------- transform

#[test]
fn identity_transform_with_equal_sizes_is_the_identity() {
    let t = Zoom::IDENTITY.transform(1920.0, 1080.0, 1920.0, 1080.0);
    assert!(approx(t.a, 1.0) && approx(t.d, 1.0));
    assert!(approx(t.tx, 0.0) && approx(t.ty, 0.0));
}

/// Not "matching aspect" — a 640x360 source into 1920x1080 scales by 3.
#[test]
fn identity_transform_scales_a_smaller_same_aspect_source_up() {
    let t = Zoom::IDENTITY.transform(640.0, 360.0, 1920.0, 1080.0);
    assert!(approx(t.a, 3.0) && approx(t.d, 3.0));
    assert!(approx(t.tx, 0.0) && approx(t.ty, 0.0));
}

/// Letterbox, not stretch: a 4:3 source into 16:9 gets equal pillarbox bars
/// and an undistorted aspect ratio.
#[test]
fn a_four_three_source_pillarboxes_into_sixteen_nine() {
    let t = Zoom::IDENTITY.transform(1440.0, 1080.0, 1920.0, 1080.0);
    assert!(
        approx(t.a, 1.0) && approx(t.d, 1.0),
        "scale must be uniform (no stretch)"
    );
    let bar = (1920.0 - 1440.0) / 2.0;
    assert!(approx(t.tx, bar), "left bar {} != {bar}", t.tx);
    assert!(approx(t.ty, 0.0));
}

/// **The pan-convention test.** The only Swift test tying `pan` to a rendering
/// matrix covered `deltaTransform`, which this port deletes — so this replaces
/// it. Nothing else pins the `- pan_x * src_w * s` term, and dropping the `* s`
/// gives a pan that drifts toward centre as scale rises.
#[test]
fn transform_maps_the_pan_centre_to_the_output_centre() {
    let (src_w, src_h) = (1440.0, 1080.0); // non-square, non-matching aspect
    let (out_w, out_h) = (1920.0, 1080.0);
    let z = Zoom::new(3.0, 0.1, -0.05);

    let t = z.transform(src_w, src_h, out_w, out_h);
    // The source point at the centre of the visible window, in source pixels.
    let (px, py) = ((0.5 + z.pan_x) * src_w, (0.5 + z.pan_y) * src_h);
    let (ox, oy) = t.apply(px, py);

    assert!(
        approx(ox, out_w / 2.0),
        "pan centre x -> {ox}, expected {}",
        out_w / 2.0
    );
    assert!(
        approx(oy, out_h / 2.0),
        "pan centre y -> {oy}, expected {}",
        out_h / 2.0
    );
}

/// `transform` at identity is also the content rect strokes denormalize
/// against — one function, both needs.
#[test]
fn identity_transform_is_the_content_rect() {
    let t = Zoom::IDENTITY.transform(1440.0, 1080.0, 1920.0, 1080.0);
    let (x0, y0) = t.apply(0.0, 0.0);
    let (x1, y1) = t.apply(1440.0, 1080.0);
    assert!(approx(x0, 240.0) && approx(y0, 0.0));
    assert!(approx(x1, 1680.0) && approx(y1, 1080.0));
}

// ------------------------------------------------------------------ replay

fn zoom_ev(t: f64, scale: f64, pan_x: f64) -> CommentaryEvent {
    CommentaryEvent::new(t, EventKind::Zoom(Zoom::new(scale, pan_x, 0.0)))
}

#[test]
fn an_empty_track_is_identity() {
    assert_eq!(zoom_at(&[], 5.0), Zoom::IDENTITY);
}

#[test]
fn before_the_first_keyframe_holds_the_first_value() {
    let evs = [zoom_ev(10.0, 3.0, 0.1)];
    assert_eq!(zoom_at(&evs, 0.0), Zoom::new(3.0, 0.1, 0.0));
}

#[test]
fn after_the_last_keyframe_holds_the_last_value() {
    let evs = [zoom_ev(1.0, 2.0, 0.0), zoom_ev(2.0, 3.0, 0.2)];
    assert_eq!(zoom_at(&evs, 99.0), Zoom::new(3.0, 0.2, 0.0));
}

#[test]
fn an_exact_keyframe_hit_returns_that_value() {
    let evs = [zoom_ev(1.0, 2.0, 0.0), zoom_ev(3.0, 4.0, 0.2)];
    assert_eq!(zoom_at(&evs, 3.0), Zoom::new(4.0, 0.2, 0.0));
}

/// Replay interpolates. This is why the recorder's anchor keyframe exists —
/// without the lerp it would be inert.
#[test]
fn between_keyframes_the_value_is_interpolated() {
    let evs = [zoom_ev(0.0, 1.0, 0.0), zoom_ev(2.0, 3.0, 0.4)];
    let mid = zoom_at(&evs, 1.0);
    assert!(approx(mid.scale, 2.0), "scale {} != 2.0", mid.scale);
    assert!(approx(mid.pan_x, 0.2), "pan_x {} != 0.2", mid.pan_x);
}

/// The anchor-keyframe pattern: a keyframe at `t - 1ms` holding the previous
/// value turns a smooth ramp across a quiet gap into a hold-then-snap.
#[test]
fn an_anchor_keyframe_produces_a_hold_then_a_snap() {
    let without = [zoom_ev(0.0, 1.0, 0.0), zoom_ev(10.0, 5.0, 0.0)];
    assert!(
        approx(zoom_at(&without, 5.0).scale, 3.0),
        "no anchor: ramps across the gap"
    );

    let with = [
        zoom_ev(0.0, 1.0, 0.0),
        zoom_ev(9.999, 1.0, 0.0),
        zoom_ev(10.0, 5.0, 0.0),
    ];
    assert!(
        approx(zoom_at(&with, 5.0).scale, 1.0),
        "anchored: holds across the gap"
    );
    assert!(
        approx(zoom_at(&with, 10.0).scale, 5.0),
        "anchored: snaps at the keyframe"
    );
}

#[test]
fn non_zoom_events_are_ignored() {
    let stroke = Stroke {
        id: Uuid::nil(),
        color: Rgba::RED,
        line_width: 0.01,
        points: vec![StrokePoint {
            x: 0.5,
            y: 0.5,
            t: 0.0,
        }],
        auto_clear_after_seconds: None,
    };
    let evs = [
        CommentaryEvent::new(0.5, EventKind::Stroke(stroke)),
        CommentaryEvent::new(0.7, EventKind::ClearAll),
        CommentaryEvent::new(0.9, EventKind::Play { source_time: 3.0 }),
        zoom_ev(1.0, 2.0, 0.0),
    ];
    assert_eq!(zoom_at(&evs, 2.0), Zoom::new(2.0, 0.0, 0.0));
}

/// An unrecognized event kind must be inert in the lookup, not an error and
/// not a keyframe.
#[test]
fn unknown_kinds_do_not_appear_in_the_lookup() {
    let evs = [
        CommentaryEvent::new(
            0.5,
            EventKind::Unknown(serde_json::json!({"futureKind": {}})),
        ),
        zoom_ev(1.0, 2.0, 0.0),
    ];
    assert_eq!(zoom_at(&evs, 2.0), Zoom::new(2.0, 0.0, 0.0));
    assert_eq!(zoom_at(&evs[..1], 2.0), Zoom::IDENTITY);
}

/// `f64::clamp` asserts `min <= max`, so a NaN scale would panic: NaN survives
/// the scale clamp, the `<= 1.0` guard is then false, and the pan limit becomes
/// NaN. Swift's nested min/max degrades to 10x instead. Reachable from a live
/// pinch gesture in Phase 6.
#[test]
fn a_nan_zoom_does_not_panic() {
    let z = Zoom::new(f64::NAN, 0.0, 0.0).clamped();
    assert_eq!(z.scale, 10.0, "matches Swift's min/max degradation");

    let z = Zoom::new(2.0, 0.1, 0.0).zoomed_to_cursor(f64::NAN, 0.5, 0.5);
    assert!(
        z.scale.is_finite(),
        "scale must stay finite, got {}",
        z.scale
    );
}
