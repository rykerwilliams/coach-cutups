//! What the picture does (spec D1, D3), on synthetic series and synthetic
//! pictures.
//!
//! Nothing here decodes anything: media hands core a number series and a
//! greyscale thumbnail grid, and these pin the rules that read them — the
//! stillness floor and its intolerance of a single moving frame, and the
//! normalisation that lets one match's kick-off frame be compared with
//! another's. The real footage is measured by the `#[ignore]`d ground-truth
//! run in the harness.

use video_coach_core::motion::{
    peaks, still_intervals, still_intervals_at, Template, Thumbnail, MOTION_HZ, STILL_MIN_SECONDS,
    STILL_THETA, THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH,
};

/// A motion series at [`MOTION_HZ`] from `(seconds, value)` stretches.
fn series(stretches: &[(f64, f32)]) -> Vec<f32> {
    let mut out = Vec::new();
    for &(seconds, value) in stretches {
        out.extend(std::iter::repeat_n(
            value,
            (seconds * MOTION_HZ).round() as usize,
        ));
    }
    out
}

/// A picture `f(x, y)` on the thumbnail grid, at whatever size media hands
/// core it in.
fn picture(width: usize, height: usize, f: impl Fn(f64, f64) -> f32) -> Thumbnail {
    let luma: Vec<f32> = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            f(x as f64 / width as f64, y as f64 / height as f64)
        })
        .collect();
    Thumbnail::from_luma(&luma, width, height)
}

/// A stand-in for a kick-off frame: a bright band down the middle of the
/// picture (the halfway line) over a dark field.
fn halfway(shift: f64) -> Thumbnail {
    picture(THUMBNAIL_WIDTH * 5, THUMBNAIL_HEIGHT * 5, |x, y| {
        let line = if (x - 0.5 - shift).abs() < 0.04 {
            200.0
        } else {
            40.0
        };
        line + 20.0 * y as f32
    })
}

#[test]
fn a_long_still_stretch_is_an_interval_and_a_short_one_is_not() {
    let motion = series(&[
        (12.0, STILL_THETA - 1.0),
        (3.0, STILL_THETA + 10.0),
        (6.0, STILL_THETA - 1.0),
    ]);
    let still = still_intervals(&motion, MOTION_HZ);
    assert_eq!(still.len(), 1, "{still:?}");
    assert!((still[0].start - 0.0).abs() < 1e-9, "{still:?}");
    assert!((still[0].end - 12.0).abs() < 1e-9, "{still:?}");
}

#[test]
fn the_floor_is_inclusive() {
    let exactly = series(&[(STILL_MIN_SECONDS, 0.0), (5.0, STILL_THETA + 1.0)]);
    assert_eq!(still_intervals(&exactly, MOTION_HZ).len(), 1);
    let one_frame_short = series(&[
        (STILL_MIN_SECONDS - 1.0 / MOTION_HZ, 0.0),
        (5.0, STILL_THETA + 1.0),
    ]);
    assert!(still_intervals(&one_frame_short, MOTION_HZ).is_empty());
}

#[test]
fn one_moving_frame_splits_a_still_stretch() {
    // The rule has no tolerance, and this is where that is decided: a single
    // frame over the threshold ends the interval. A hold broken by one
    // flicker is two holds.
    let mut motion = series(&[(30.0, 0.0)]);
    motion[(15.0 * MOTION_HZ) as usize] = STILL_THETA + 5.0;
    let still = still_intervals(&motion, MOTION_HZ);
    assert_eq!(still.len(), 2, "{still:?}");
    assert!((still[0].end - 15.0).abs() < 1e-9, "{still:?}");
    assert!((still[1].start - 15.2).abs() < 1e-9, "{still:?}");
}

#[test]
fn a_higher_threshold_keeps_a_noisier_hold() {
    let motion = series(&[(12.0, 3.5), (5.0, 20.0)]);
    assert!(still_intervals_at(&motion, MOTION_HZ, 3.0, STILL_MIN_SECONDS).is_empty());
    assert_eq!(
        still_intervals_at(&motion, MOTION_HZ, 4.0, STILL_MIN_SECONDS).len(),
        1
    );
}

#[test]
fn a_thumbnail_matches_itself_and_survives_exposure() {
    let frame = halfway(0.0);
    assert!((frame.similarity(&frame) - 1.0).abs() < 1e-5);

    // The same picture through a brighter lens: every level scaled and
    // lifted. A normalised correlation is what makes two kick-offs in
    // different light the same picture.
    let brighter = picture(THUMBNAIL_WIDTH * 5, THUMBNAIL_HEIGHT * 5, |x, y| {
        let line = if (x - 0.5).abs() < 0.04 { 200.0 } else { 40.0 };
        30.0 + 1.4 * (line + 20.0 * y as f32)
    });
    assert!(
        frame.similarity(&brighter) > 0.99,
        "{}",
        frame.similarity(&brighter)
    );
}

#[test]
fn a_different_picture_scores_far_lower() {
    let kickoff = halfway(0.0);
    let elsewhere = halfway(0.35);
    assert!(
        kickoff.similarity(&elsewhere) < 0.5,
        "{}",
        kickoff.similarity(&elsewhere)
    );
    // A flat picture has no detail to correlate, so it matches nothing rather
    // than everything.
    let flat = picture(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, |_, _| 128.0);
    assert!(kickoff.similarity(&flat).abs() < 1e-6);
    assert!(flat.similarity(&flat).abs() < 1e-6);
}

#[test]
fn a_template_scores_by_its_best_member() {
    let template = Template::new(vec![halfway(0.0), halfway(0.35)]);
    let frame = halfway(0.35);
    assert!((template.score(&frame) - 1.0).abs() < 1e-5);
    assert_eq!(template.scores(&[frame, halfway(0.0)]).len(), 2);
    // An empty template is the leave-one-out case with nothing left over. It
    // must score below any threshold rather than above every one.
    assert!(Template::new(Vec::new()).score(&halfway(0.0)) < -0.99);
}

#[test]
fn peaks_are_the_best_of_each_neighbourhood() {
    // Two rises 4 s apart and one 40 s later: at a 30 s gap the first two are
    // one peak, at the higher of them.
    let mut scores = vec![0.1f32; 60];
    scores[10] = 0.7;
    scores[14] = 0.8;
    scores[54] = 0.75;
    let found = peaks(&scores, 1.0, 0.5, 30.0);
    let times: Vec<f64> = found.iter().map(|p| p.seconds).collect();
    assert_eq!(times, vec![14.0, 54.0], "{found:?}");
    assert!((found[0].score - 0.8).abs() < 1e-6);
    // Raising the threshold over a peak drops it, and nothing else moves.
    let found = peaks(&scores, 1.0, 0.78, 30.0);
    assert_eq!(found.len(), 1);
    assert!((found[0].seconds - 14.0).abs() < 1e-9);
}
