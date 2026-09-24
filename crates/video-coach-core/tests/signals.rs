//! The whistle and cheer signals (spec D2), on synthetic sound.
//!
//! Every fixture here is generated from a fixed seed, so these run on CI with
//! no footage, no GStreamer and no model. The real footage is measured by the
//! `#[ignore]`d ground-truth run in the harness; what is pinned here is the
//! rules themselves — the pitch hold, the duration floors, and the tonality
//! term that separates a whistle from a shout.

use video_coach_core::signals::{
    cheers, claps, whistles, Cheer, Clap, Whistle, CHEER_MIN_SECONDS, HOP_SAMPLES, HOP_SECONDS,
    SIGNAL_SAMPLE_RATE, WHISTLE_BIN_HZ, WHISTLE_LONG_SECONDS, WHISTLE_MIN_SECONDS,
};

const RATE: f64 = SIGNAL_SAMPLE_RATE as f64;

/// A fixed-seed xorshift, so a failure reproduces exactly. Core has four
/// dependencies and a random-number crate is not one of them.
struct Noise(u64);

impl Noise {
    fn new() -> Noise {
        Noise(0x5eed_1234_9abc_def1)
    }

    /// Uniform in [−1, 1).
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 40) as f32 / 8_388_608.0 - 1.0
    }
}

fn at(seconds: f64) -> usize {
    (seconds * RATE) as usize
}

/// `seconds` of pink-ish noise at roughly `rms`: white through one one-pole
/// section at 1 kHz, which is close enough to a room's own floor that the
/// whistle band is the quiet end of it.
fn floor(seconds: f64, rms: f32) -> Vec<f32> {
    let mut noise = Noise::new();
    let mut low = 0.0f32;
    // A one-pole at 1 kHz takes about 5 dB out of the white level, and the
    // scale below is what puts it back; nothing here needs the level exact.
    let a = 1.0 - (-2.0 * std::f32::consts::PI * 1_000.0 / RATE as f32).exp();
    (0..at(seconds))
        .map(|_| {
            low += a * (noise.next() - low);
            low * rms * 3.0
        })
        .collect()
}

/// Adds a tone sweeping from `from` to `to` over `[start, start + duration)`.
fn tone(samples: &mut [f32], start: f64, duration: f64, from: f32, to: f32, amplitude: f32) {
    let mut phase = 0.0f64;
    let span = at(start + duration).min(samples.len()) - at(start);
    for (i, s) in samples[at(start)..at(start) + span].iter_mut().enumerate() {
        let hz = f64::from(from) + f64::from(to - from) * i as f64 / span as f64;
        phase += 2.0 * std::f64::consts::PI * hz / RATE;
        *s += amplitude * phase.sin() as f32;
    }
}

/// Adds a horn over `[start, start + duration)`: every harmonic of 62.5 Hz
/// inside the whistle band, at equal amplitude and scattered phase.
///
/// A horn, a buzzer, a plastic trumpet in the stand. Its harmonics are 62.5 Hz
/// apart and the bank's bins are 75 Hz apart, so **every bin is full** — and
/// because the signal repeats once per hop, its loudest bin is the same bin
/// from the first window to the last. That is what makes it the one fixture
/// that a loud-and-holds-its-pitch rule cannot reject.
fn horn(samples: &mut [f32], start: f64, duration: f64, amplitude: f32) {
    let mut noise = Noise::new();
    let f0 = RATE / HOP_SAMPLES as f64;
    let harmonics: Vec<(f64, f64)> = (1..)
        .map(|k| k as f64 * f0)
        .take_while(|hz| *hz < 5_100.0)
        .filter(|hz| *hz > 1_900.0)
        .map(|hz| (hz, f64::from(noise.next()) * std::f64::consts::PI))
        .collect();
    let end = at(start + duration).min(samples.len());
    for (i, s) in samples[at(start)..end].iter_mut().enumerate() {
        let t = i as f64 / RATE;
        *s += amplitude
            * harmonics
                .iter()
                .map(|(hz, phase)| (2.0 * std::f64::consts::PI * hz * t + phase).sin())
                .sum::<f64>() as f32;
    }
}

fn only<T: Copy + std::fmt::Debug>(found: &[T], what: &str) -> T {
    let [one] = found else {
        panic!("expected one {what}, found {}: {found:?}", found.len());
    };
    *one
}

// ------------------------------------------------------------- whistles

#[test]
fn a_tone_in_the_band_is_one_whistle() {
    let mut sound = floor(3.0, 0.005);
    tone(&mut sound, 1.0, 0.3, 3_200.0, 3_200.0, 0.1);
    let found: Vec<Whistle> = whistles(&sound);
    let whistle = only(&found, "whistle");
    assert!(
        (whistle.freq - 3_200.0).abs() <= WHISTLE_BIN_HZ,
        "{whistle:?} is more than a bin off 3200 Hz"
    );
    assert!(
        (whistle.duration - 0.3).abs() <= HOP_SECONDS,
        "{whistle:?} is more than a hop off 0.3 s"
    );
    assert!(
        (whistle.start - 1.0).abs() <= 2.0 * HOP_SECONDS,
        "{whistle:?} does not start at 1.0 s"
    );
    assert!(!whistle.is_long(), "{whistle:?} is not a long blast");
}

#[test]
fn a_blast_held_past_the_long_floor_is_long() {
    let mut sound = floor(4.0, 0.005);
    tone(
        &mut sound,
        1.0,
        WHISTLE_LONG_SECONDS + 0.2,
        3_200.0,
        3_200.0,
        0.1,
    );
    assert!(only(&whistles(&sound), "whistle").is_long());
}

#[test]
fn a_tone_shorter_than_the_floor_is_not_a_whistle() {
    let mut sound = floor(3.0, 0.005);
    // 140 ms, just under WHISTLE_MIN_SECONDS: the windows either side of it
    // hold some of the tone, so this is the case that says the duration is
    // measured from the windows' centres and not from their edges.
    tone(
        &mut sound,
        1.0,
        WHISTLE_MIN_SECONDS - 0.01,
        3_200.0,
        3_200.0,
        0.1,
    );
    assert_eq!(whistles(&sound), Vec::new());
}

#[test]
fn a_tone_that_slides_is_not_one_whistle() {
    let mut sound = floor(3.0, 0.005);
    tone(&mut sound, 1.0, 0.4, 3_000.0, 3_600.0, 0.1);
    let found = whistles(&sound);
    // Either two — the run breaks and the rest of the slide holds long enough
    // to be its own — or none. Never one whistle spanning the slide, which is
    // what a rule with no pitch hold would report.
    assert_ne!(
        found.len(),
        1,
        "the slide was taken for one whistle: {found:?}"
    );
    for whistle in &found {
        assert!(
            whistle.duration < 0.3,
            "{whistle:?} spans most of a 0.4 s slide"
        );
    }
}

#[test]
fn a_horn_is_not_a_whistle() {
    // 300 ms, a shade louder than the tone above and spread over the whole
    // band instead of standing in one bin. It is 31 dB over the band's median
    // and its loudest bin never moves, so the level and the pitch hold both
    // pass it; its peak stands 5.5 dB over the rest of its own window, and
    // that is the only thing that says no. Without the tonality term this is
    // a whistle.
    let mut sound = floor(3.0, 0.005);
    horn(&mut sound, 1.0, 0.3, 0.02);
    assert_eq!(whistles(&sound), Vec::new());
}

// ---------------------------------------------------------------- cheers

/// `seconds` of white noise at `rms`, scaled by `gain` as a function of time —
/// the venue getting louder as it fills.
fn crowd(seconds: f64, rms: f32, gain: impl Fn(f64) -> f32) -> Vec<f32> {
    let mut noise = Noise::new();
    (0..at(seconds))
        .map(|i| noise.next() * rms * 1.73 * gain(i as f64 / RATE))
        .collect()
}

/// Multiplies `[start, start + duration)` by `db` decibels.
fn louder(samples: &mut [f32], start: f64, duration: f64, db: f32) {
    let gain = 10.0f32.powf(db / 20.0);
    let end = at(start + duration).min(samples.len());
    for s in samples[at(start)..end].iter_mut() {
        *s *= gain;
    }
}

#[test]
fn a_held_rise_in_the_crowd_band_is_one_cheer() {
    let mut sound = crowd(60.0, 0.01, |_| 1.0);
    louder(&mut sound, 30.0, 1.2, 10.0);
    let found: Vec<Cheer> = cheers(&sound);
    let cheer = only(&found, "cheer");
    assert!(
        (cheer.onset - 30.0).abs() <= 2.0 * HOP_SECONDS,
        "{cheer:?} does not start at 30 s"
    );
    assert!(
        cheer.duration >= CHEER_MIN_SECONDS,
        "{cheer:?} is under the floor it was detected at"
    );
    assert!(cheer.peak_db >= 8.0, "{cheer:?} understates a 10 dB rise");
}

#[test]
fn a_rise_that_does_not_hold_is_not_a_cheer() {
    let mut sound = crowd(60.0, 0.01, |_| 1.0);
    louder(&mut sound, 30.0, 0.6, 10.0);
    assert_eq!(cheers(&sound), Vec::new());
}

// ----------------------------------------------------------------- claps
//
// The cue these pin is the one the level cue cannot reach: applause is quiet
// and textured, so every fixture here is built so that the *level* rule and the
// *texture* rule disagree about it.

/// Adds a train of `per_second` hand claps over `[start, start + duration)`.
///
/// Each clap is a burst of white noise with a 2 ms decay and no sustain, which
/// is what one looks like on a 2 ms envelope: the attack lands inside one block
/// and the tail is gone three blocks later. The gaps are jittered by ±20 %,
/// because a crowd clapping in lockstep is a fixture artefact and a periodic
/// train is the one thing a rate counter could get right by accident.
fn clapping(samples: &mut [f32], start: f64, duration: f64, per_second: f64, amplitude: f32) {
    let mut noise = Noise::new();
    let gap = RATE / per_second;
    let tail = at(0.010);
    let end = at(start + duration).min(samples.len());
    let mut n = at(start);
    while n < end {
        for k in 0..tail.min(end - n) {
            let decay = (-(k as f32) / (RATE as f32 * 0.002)).exp();
            samples[n + k] += amplitude * decay * noise.next();
        }
        n += (gap * (1.0 + 0.2 * f64::from(noise.next()))) as usize;
    }
}

#[test]
fn a_train_of_transients_is_one_clap() {
    let mut sound = crowd(90.0, 0.01, |_| 1.0);
    clapping(&mut sound, 45.0, 3.0, 20.0, 0.06);
    let found: Vec<Clap> = claps(&sound);
    let clap = only(&found, "clap");
    assert!(
        (clap.onset - 45.0).abs() <= 1.0,
        "{clap:?} does not start at 45 s"
    );
    assert!(
        clap.duration >= 2.0,
        "{clap:?} covers less than the 3 s train"
    );
    assert!(
        clap.rate >= 10.0,
        "{clap:?} counts under half of a 20 a second train"
    );
}

#[test]
fn a_shout_the_level_rule_fires_on_is_not_a_clap() {
    // The same background, raised 10 dB for three seconds: one sustained
    // broadband sound, which is what a shout is. It is **louder** than the clap
    // train above and the level rule takes it — so this is the fixture that
    // says the texture rule is not the level rule wearing a different name.
    let mut sound = crowd(90.0, 0.01, |_| 1.0);
    louder(&mut sound, 45.0, 3.0, 10.0);
    assert_eq!(cheers(&sound).len(), 1, "the level rule should fire on it");
    assert_eq!(claps(&sound), Vec::new());
}

#[test]
fn a_whistle_is_not_a_clap() {
    let mut sound = crowd(90.0, 0.01, |_| 1.0);
    tone(&mut sound, 45.0, 3.0, 3_200.0, 3_200.0, 0.1);
    assert_eq!(claps(&sound), Vec::new());
}

#[test]
fn clapping_rides_a_venue_that_is_getting_louder() {
    // The same 20 dB ramp the cheer rule is pinned against. Nothing absolute
    // survives it, and the texture's rolling median is what makes a quiet
    // venue's applause and a loud one's comparable.
    let mut sound = crowd(180.0, 0.01, |t| {
        10.0f32.powf((t as f32 / 180.0) * 20.0 / 20.0)
    });
    clapping(
        &mut sound,
        150.0,
        3.0,
        20.0,
        0.06 * 10.0f32.powf(20.0 / 20.0),
    );
    let clap = only(&claps(&sound), "clap");
    assert!(
        (clap.onset - 150.0).abs() <= 1.0,
        "{clap:?} does not start at 150 s"
    );
}

#[test]
fn a_cheer_rides_a_venue_that_is_getting_louder() {
    // 20 dB across three minutes. Nothing absolute survives that — the last
    // minute's floor is louder than the first minute's cheer would have been —
    // and the rolling median is what makes the two ends comparable.
    let mut sound = crowd(180.0, 0.01, |t| {
        10.0f32.powf((t as f32 / 180.0) * 20.0 / 20.0)
    });
    louder(&mut sound, 150.0, 1.2, 10.0);
    let cheer = only(&cheers(&sound), "cheer");
    assert!(
        (cheer.onset - 150.0).abs() <= 2.0 * HOP_SECONDS,
        "{cheer:?} does not start at 150 s"
    );
}
