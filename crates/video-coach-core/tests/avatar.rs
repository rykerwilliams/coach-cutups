//! The pulse: dB to a level, the one-pole filter both estimators share, the
//! per-frame table the render reads, and where the picture lands.

use video_coach_core::avatar::{
    avatar_rect, level_from_db, pulse, smooth, PULSE_ATTACK, PULSE_CEILING_DB, PULSE_FLOOR_DB,
    PULSE_GROWTH, PULSE_RATE, PULSE_RELEASE,
};
use video_coach_core::layout::Rect;

const DT: f64 = 1.0 / 30.0;

fn pip() -> Rect {
    Rect {
        x: 1456.0,
        y: 719.0,
        w: 422.4,
        h: 316.8,
    }
}

/// A deterministic generator, so a property test is reproducible and core
/// stays at four dependencies. xorshift64*, seeded per test.
struct Rng(u64);

impl Rng {
    /// The next value in `0.0..1.0`.
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A mono sine at `hz`, `seconds` long, at [`PULSE_RATE`].
fn tone(hz: f64, seconds: f64, amplitude: f32) -> Vec<f32> {
    let n = (seconds * f64::from(PULSE_RATE)) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(PULSE_RATE);
            (amplitude as f64 * (std::f64::consts::TAU * hz * t).sin()) as f32
        })
        .collect()
}

// ------------------------------------------------------------ level_from_db

#[test]
fn level_from_db_maps_the_window_and_clamps_outside_it() {
    assert_eq!(level_from_db(PULSE_FLOOR_DB), 0.0);
    assert_eq!(level_from_db(PULSE_CEILING_DB), 1.0);
    let mid = level_from_db((PULSE_FLOOR_DB + PULSE_CEILING_DB) / 2.0);
    assert!((mid - 0.5).abs() < 1e-12, "{mid}");
    assert_eq!(level_from_db(PULSE_FLOOR_DB - 30.0), 0.0);
    assert_eq!(level_from_db(PULSE_CEILING_DB + 30.0), 1.0);
    assert_eq!(level_from_db(f64::NEG_INFINITY), 0.0, "digital silence");
    assert_eq!(level_from_db(f64::INFINITY), 1.0);
    assert_eq!(level_from_db(f64::NAN), 0.0, "never a NaN size");
}

// ------------------------------------------------------------------- smooth

/// The filter's whole contract: one time constant while rising, another while
/// falling, and `1 - exp(-dt/tau)` of the way there in one step.
#[test]
fn smooth_uses_the_attack_rising_and_the_release_falling() {
    let up = smooth(0.0, 1.0, PULSE_ATTACK);
    assert!((up - (1.0 - (-1.0f64).exp())).abs() < 1e-12, "{up}");

    let down = smooth(1.0, 0.0, PULSE_RELEASE);
    assert!((down - (-1.0f64).exp()).abs() < 1e-12, "{down}");

    // The asymmetry is the point: over the same dt, a fall moves less than a
    // rise, because the release is the longer constant.
    assert!(smooth(0.0, 1.0, DT) > 1.0 - smooth(1.0, 0.0, DT));
}

#[test]
fn smooth_at_either_rate_reaches_the_same_steady_state() {
    let run = |dt: f64, seconds: f64| {
        let mut s = 0.0;
        for _ in 0..(seconds / dt) as usize {
            s = smooth(s, 0.7, dt);
        }
        s
    };
    let slow = run(0.1, 2.0);
    let fast = run(DT, 2.0);
    assert!((slow - 0.7).abs() < 1e-3, "{slow}");
    assert!((fast - 0.7).abs() < 1e-3, "{fast}");
}

#[test]
fn smooth_over_no_time_changes_nothing() {
    assert_eq!(smooth(0.4, 1.0, 0.0), 0.4);
    assert_eq!(smooth(0.4, 0.0, 0.0), 0.4);
}

// -------------------------------------------------------------------- pulse

#[test]
fn silence_never_moves_the_avatar() {
    let quiet = vec![0.0f32; PULSE_RATE as usize];
    assert_eq!(pulse(&quiet, PULSE_RATE, 30), vec![0.0; 30]);
}

#[test]
fn a_full_scale_tone_saturates() {
    let loud = tone(200.0, 1.0, 1.0);
    let table = pulse(&loud, PULSE_RATE, 30);
    // The attack is 60 ms, so it is pinned well before the second is out.
    assert!(table[9] > 0.99, "{:?}", &table[..10]);
    assert!(table.iter().all(|&v| v <= 1.0));
}

#[test]
fn a_step_rises_in_two_frames_and_decays_over_about_seven() {
    let mut samples = vec![0.0f32; (0.5 * f64::from(PULSE_RATE)) as usize];
    samples.extend(tone(200.0, 0.5, 1.0));
    samples.extend(vec![0.0f32; (0.5 * f64::from(PULSE_RATE)) as usize]);
    let table = pulse(&samples, PULSE_RATE, 45);

    let loud_at = 15; // 0.5 s in
    assert!(table[loud_at - 1] < 0.01, "silent before the step");
    assert!(
        table[loud_at + 1] > 0.5,
        "two frames of a 60 ms attack: {:?}",
        &table[loud_at..loud_at + 3]
    );

    let quiet_at = 30; // 1.0 s in
    let peak = table[quiet_at - 1];
    // 220 ms is 6.6 frames, so seven frames is one time constant: ~37% left.
    let after = table[quiet_at + 7];
    assert!(after < 0.45 * peak, "peak {peak}, after {after}");
    assert!(after > 0.2 * peak, "a release, not a cut: {after}");
}

#[test]
fn a_window_past_the_end_of_the_audio_is_silence() {
    assert_eq!(pulse(&[], PULSE_RATE, 5), vec![0.0; 5]);

    // Half a second of tone, but a two-second entry: the tail decays to zero
    // and stays there.
    let table = pulse(&tone(200.0, 0.5, 1.0), PULSE_RATE, 60);
    assert_eq!(table.len(), 60);
    assert!(
        table[14] > 0.9,
        "the last frame that has audio: {}",
        table[14]
    );
    assert!(table[59] < 0.01, "long past the end: {}", table[59]);

    assert!(pulse(&tone(200.0, 1.0, 1.0), PULSE_RATE, 0).is_empty());
}

#[test]
fn every_level_is_in_range_whatever_the_audio() {
    let mut rng = Rng(0x2026_0922);
    let samples: Vec<f32> = (0..PULSE_RATE as usize * 2)
        .map(|_| (rng.unit() as f32 - 0.5) * 8.0) // deliberately past full scale
        .collect();
    for (n, v) in pulse(&samples, PULSE_RATE, 60).into_iter().enumerate() {
        assert!((0.0..=1.0).contains(&v), "frame {n}: {v}");
    }
}

// -------------------------------------------------------------- avatar_rect

#[test]
fn at_full_level_the_avatar_is_exactly_the_inset() {
    assert_eq!(avatar_rect(pip(), 1.0), pip());
    assert_eq!(avatar_rect(pip(), 2.0), pip(), "out of range clamps");
}

#[test]
fn at_rest_the_avatar_is_concentric_and_smaller_by_the_growth() {
    let pip = pip();
    let rest = avatar_rect(pip, 0.0);
    assert!((rest.w - pip.w / PULSE_GROWTH).abs() < 1e-12, "{rest:?}");
    assert!((rest.h - pip.h / PULSE_GROWTH).abs() < 1e-12, "{rest:?}");
    assert!((rest.x + rest.w / 2.0 - (pip.x + pip.w / 2.0)).abs() < 1e-12);
    assert!((rest.y + rest.h / 2.0 - (pip.y + pip.h / 2.0)).abs() < 1e-12);
    assert_eq!(avatar_rect(pip, -1.0), rest, "out of range clamps");
    assert_eq!(avatar_rect(pip, f64::NAN), rest, "never a NaN rect");
}

#[test]
fn the_avatar_grows_with_the_level_and_never_past_the_inset() {
    let pip = pip();
    let mut previous = avatar_rect(pip, 0.0).w;
    for i in 1..=20 {
        let r = avatar_rect(pip, f64::from(i) / 20.0);
        assert!(r.w > previous, "monotone at {i}");
        previous = r.w;
    }

    let mut rng = Rng(0xa7a7_1234);
    for _ in 0..500 {
        let level = (rng.unit() - 0.25) * 2.0; // some out of range, both ways
        let r = avatar_rect(pip, level);
        assert!(r.w <= pip.w && r.h <= pip.h, "{level}: {r:?}");
        assert!(r.x >= pip.x && r.y >= pip.y, "{level}: {r:?}");
        assert!(r.x + r.w <= pip.x + pip.w + 1e-12, "{level}: {r:?}");
        assert!(r.y + r.h <= pip.y + pip.h + 1e-12, "{level}: {r:?}");
    }
}
