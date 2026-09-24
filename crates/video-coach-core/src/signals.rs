//! What the crowd and the referee sound like (spec D2): whistles and cheers,
//! as pure functions over a slice of 16 kHz mono samples.
//!
//! Media decodes; core measures. Everything here is a number series over the
//! same frame grid — 32 ms windows on a 16 ms hop — and every threshold is
//! **relative to a 60 s rolling median of the signal's own band**. Nothing
//! absolute can work: the two venues in the design footage differ by about
//! 27 dB in the whistle band, so a level tuned on one is noise on the other
//! [measured].
//!
//! **Every constant here is an initial value that P3 replaces** with one
//! chosen on the tuning match (spec G2). They are `pub` so the ground-truth
//! run can print them on its `RUN` line and sweep around them.
//!
//! # Why a whistle needs a tonality term
//!
//! A level rule on the whistle band does not work. Measured on one whole half,
//! the two loudest 2–4.5 kHz events were the two loudest **cheers**: a shout
//! is broadband and leaks straight into the band, and the half's own final
//! whistle had no level excursion at any threshold tried. So a whistle is a
//! peak that is loud against the band's recent past ([`WHISTLE_SNR_DB`]) and
//! holds its pitch ([`WHISTLE_PITCH_HZ`]) **and** stands over the rest of its
//! own window ([`WHISTLE_TONALITY_DB`]).
//!
//! The last term is the one that is easy to think redundant, because most
//! broadband sound fails the pitch hold on its own: a noise burst's loudest
//! bin hops about, and a run breaks the moment it hops a bin too far. What it
//! catches is sound that is broadband **and** steady — a horn, a buzzer, a
//! plastic trumpet — whose loudest bin does not move for as long as it lasts.
//! Measured on the synthetic one in `tests/signals.rs`: 31 dB over the band's
//! median, one bin from first window to last, and 5.5 dB over the rest of its
//! own window. Level and pitch call that a whistle; tonality is what says no.
//!
//! # Why no FFT crate
//!
//! Core declares four dependencies and an audit fails a fifth. The bank is
//! forty hand-written Goertzel evaluations per window, which is the whole of
//! the arithmetic that an FFT would have saved.

use std::f64::consts::PI;

/// The rate every signal here is defined at: the 16 kHz mono media's analysis
/// audio pass decodes to, which is also what whisper takes.
pub const SIGNAL_SAMPLE_RATE: u32 = 16_000;

/// The analysis window: 32 ms.
pub const WINDOW_SAMPLES: usize = 512;

/// The step between windows: 16 ms.
pub const HOP_SAMPLES: usize = 256;

/// [`HOP_SAMPLES`] in seconds — the resolution of every time this module
/// reports.
pub const HOP_SECONDS: f64 = HOP_SAMPLES as f64 / SIGNAL_SAMPLE_RATE as f64;

/// How much recent past a level is judged against. Long enough that a goal's
/// cheer cannot raise its own baseline, short enough to follow a venue
/// getting louder as it fills.
pub const MEDIAN_SECONDS: f64 = 60.0;

/// The lowest bin of the whistle bank.
pub const WHISTLE_BAND_LOW_HZ: f32 = 2_000.0;

/// The spacing of the whistle bank's bins.
pub const WHISTLE_BIN_HZ: f32 = 75.0;

/// 2–5 kHz in [`WHISTLE_BIN_HZ`] steps: 2000 Hz … 4925 Hz.
pub const WHISTLE_BINS: usize = 40;

/// How far the peak bin must stand over the band's rolling median. **Initial
/// value** (spec D2).
pub const WHISTLE_SNR_DB: f32 = 15.0;

/// How far the peak bin must stand over the median of the *other* bins in its
/// own window. **Initial value**, and the term that separates a whistle from a
/// shout — see the module docs.
pub const WHISTLE_TONALITY_DB: f32 = 10.0;

/// How far the peak may wander and still be the same whistle. **Initial
/// value** (spec D2).
pub const WHISTLE_PITCH_HZ: f32 = 150.0;

/// The shortest run that counts as a whistle. **Initial value** (spec D2).
pub const WHISTLE_MIN_SECONDS: f64 = 0.15;

/// What [`Whistle::is_long`] means: the half-ending blast, not a play-on peep.
/// **Initial value** (spec D2).
pub const WHISTLE_LONG_SECONDS: f64 = 0.8;

/// The cheer band's lower edge.
pub const CHEER_BAND_LOW_HZ: f32 = 300.0;

/// The cheer band's upper edge.
pub const CHEER_BAND_HIGH_HZ: f32 = 3_000.0;

/// How far the cheer band must rise over its rolling median. **Initial value**
/// (spec D2).
pub const CHEER_SNR_DB: f32 = 8.0;

/// How long that rise must hold. **Initial value, and not the spec's 1.5 s:**
/// measured on one whole half, a real goal's burst ran 1.4 s, and a 1.5 s
/// floor dropped that goal while finding only three excursions in the half.
/// At 1.0 s the same half yields six, covering all three of its goals.
pub const CHEER_MIN_SECONDS: f64 = 1.0;

/// One tonal event in the whistle band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Whistle {
    /// Seconds into the samples, at the centre of the first window that held
    /// it.
    pub start: f64,
    /// How long it held, in whole hops — so a whistle that fills one window
    /// and no more is 0 s and never reaches [`WHISTLE_MIN_SECONDS`].
    pub duration: f64,
    /// The peak bin of its loudest window.
    pub freq: f32,
    /// That window's peak over the band's rolling median.
    pub snr_db: f32,
    /// That window's peak over the median of its own other bins.
    pub tonality_db: f32,
}

impl Whistle {
    /// Long enough to be a period's end rather than a stoppage (spec D4).
    pub fn is_long(&self) -> bool {
        self.duration >= WHISTLE_LONG_SECONDS
    }
}

/// One broadband rise in the crowd band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cheer {
    /// Seconds into the samples, at the centre of the first window that held
    /// it. Measured, a goal's cheer starts 0.4–1.4 s **after** the frame the
    /// coach tags.
    pub onset: f64,
    /// How long it held, in whole hops.
    pub duration: f64,
    /// The loudest window's rise over the rolling median. Measured, a goal is
    /// +25 to +46 dB.
    pub peak_db: f32,
}

/// Every whistle in `samples` (16 kHz mono), at this module's constants.
pub fn whistles(samples: &[f32]) -> Vec<Whistle> {
    let band = whistle_band(samples);
    let median = rolling_median(&band.peak_db, median_span());
    let qualifies = |i: usize| {
        band.peak_db[i] - median[i] >= WHISTLE_SNR_DB && band.tonality_db[i] >= WHISTLE_TONALITY_DB
    };

    let mut out = Vec::new();
    let mut i = 0;
    while i < band.peak_db.len() {
        if !qualifies(i) {
            i += 1;
            continue;
        }
        // The pitch is held against the run's **first** window, not its
        // neighbour: a slide that creeps a bin at a time would otherwise never
        // break, and a slide is exactly what a shout's formant does.
        let pitch = band.freq[i];
        let mut end = i + 1;
        while end < band.peak_db.len()
            && qualifies(end)
            && (band.freq[end] - pitch).abs() <= WHISTLE_PITCH_HZ
        {
            end += 1;
        }
        let duration = (end - i - 1) as f64 * HOP_SECONDS;
        if duration >= WHISTLE_MIN_SECONDS {
            let loudest = (i..end)
                .max_by(|&a, &b| {
                    (band.peak_db[a] - median[a]).total_cmp(&(band.peak_db[b] - median[b]))
                })
                .expect("a run holds at least one window");
            out.push(Whistle {
                start: frame_time(i),
                duration,
                freq: band.freq[loudest],
                snr_db: band.peak_db[loudest] - median[loudest],
                tonality_db: band.tonality_db[loudest],
            });
        }
        // A run that broke on pitch restarts here rather than a window later,
        // so the second half of a slide gets its own chance to be a whistle.
        i = end;
    }
    out
}

/// Every cheer in `samples` (16 kHz mono), at this module's constants.
pub fn cheers(samples: &[f32]) -> Vec<Cheer> {
    cheers_from(&cheer_excess(samples), CHEER_SNR_DB, CHEER_MIN_SECONDS)
}

/// The cheer band's level in dB over its own [`MEDIAN_SECONDS`] rolling
/// median, one value every [`HOP_SECONDS`].
///
/// Split out from [`cheers`] because it is the expensive half and the
/// thresholds are the half that gets swept: a whole grid of
/// `(snr_db, min_seconds)` costs one pass over the samples (spec G2).
pub fn cheer_excess(samples: &[f32]) -> Vec<f32> {
    let band = band_pass(samples, CHEER_BAND_LOW_HZ, CHEER_BAND_HIGH_HZ);
    let level: Vec<f32> = (0..frame_count(band.len()))
        .map(|i| {
            let frame = &band[i * HOP_SAMPLES..i * HOP_SAMPLES + WINDOW_SAMPLES];
            power_db(
                frame
                    .iter()
                    .map(|&s| f64::from(s) * f64::from(s))
                    .sum::<f64>(),
            )
        })
        .collect();
    let median = rolling_median(&level, median_span());
    level
        .iter()
        .zip(&median)
        .map(|(level, median)| level - median)
        .collect()
}

/// The cheers in a [`cheer_excess`] series at the given thresholds.
pub fn cheers_from(excess: &[f32], snr_db: f32, min_seconds: f64) -> Vec<Cheer> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < excess.len() {
        if excess[i] < snr_db {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < excess.len() && excess[end] >= snr_db {
            end += 1;
        }
        let duration = (end - i - 1) as f64 * HOP_SECONDS;
        if duration >= min_seconds {
            out.push(Cheer {
                onset: frame_time(i),
                duration,
                peak_db: excess[i..end].iter().copied().fold(f32::MIN, f32::max),
            });
        }
        i = end;
    }
    out
}

// ------------------------------------------------------------- the framing

/// How many windows fit in `samples` samples.
fn frame_count(samples: usize) -> usize {
    samples
        .checked_sub(WINDOW_SAMPLES)
        .map_or(0, |rest| rest / HOP_SAMPLES + 1)
}

/// Window `i`'s time: the **centre** of the window, because a run's first and
/// last windows are the ones half over the event.
fn frame_time(i: usize) -> f64 {
    (i as f64 * HOP_SAMPLES as f64 + WINDOW_SAMPLES as f64 / 2.0) / f64::from(SIGNAL_SAMPLE_RATE)
}

/// [`MEDIAN_SECONDS`] as a count of windows.
fn median_span() -> usize {
    (MEDIAN_SECONDS / HOP_SECONDS).round() as usize
}

/// A power sum as dB, with a floor so silence is a number rather than −∞.
fn power_db(power: f64) -> f32 {
    (10.0 * power.max(1e-20).log10()) as f32
}

// -------------------------------------------------------- the whistle bank

/// What the Goertzel bank says about each window.
struct Band {
    /// The strongest bin's magnitude, in dB.
    peak_db: Vec<f32>,
    /// That bin's frequency.
    freq: Vec<f32>,
    /// The peak over the median of the window's other bins.
    tonality_db: Vec<f32>,
}

fn whistle_band(samples: &[f32]) -> Band {
    let frames = frame_count(samples.len());
    let mut band = Band {
        peak_db: Vec::with_capacity(frames),
        freq: Vec::with_capacity(frames),
        tonality_db: Vec::with_capacity(frames),
    };
    // Hann, so a tone between two bins leaks into its neighbours rather than
    // across the whole band — which is what the tonality term measures.
    let window: Vec<f64> = (0..WINDOW_SAMPLES)
        .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / WINDOW_SAMPLES as f64).cos())
        .collect();
    let mut shaped = vec![0.0f64; WINDOW_SAMPLES];
    let mut bins = [0.0f32; WHISTLE_BINS];
    for i in 0..frames {
        let frame = &samples[i * HOP_SAMPLES..i * HOP_SAMPLES + WINDOW_SAMPLES];
        for ((out, &sample), &w) in shaped.iter_mut().zip(frame).zip(&window) {
            *out = f64::from(sample) * w;
        }
        for (k, bin) in bins.iter_mut().enumerate() {
            *bin = power_db(goertzel(&shaped, bin_hz(k)));
        }
        let mut sorted = bins;
        sorted.sort_unstable_by(f32::total_cmp);
        // The peak is the last of the sorted bins, so the median of the other
        // WHISTLE_BINS − 1 is the middle of what is left.
        let others = sorted[(WHISTLE_BINS - 1) / 2];
        let peak = sorted[WHISTLE_BINS - 1];
        let at = bins
            .iter()
            .position(|b| *b == peak)
            .expect("the peak is one of the bins");
        band.peak_db.push(peak);
        band.freq.push(bin_hz(at));
        band.tonality_db.push(peak - others);
    }
    band
}

fn bin_hz(k: usize) -> f32 {
    WHISTLE_BAND_LOW_HZ + k as f32 * WHISTLE_BIN_HZ
}

/// The power `frame` holds at `freq`.
///
/// Goertzel rather than a transform: the bank needs forty frequencies of a
/// 512-sample window, and each costs one multiply and two adds per sample.
fn goertzel(frame: &[f64], freq: f32) -> f64 {
    let w = 2.0 * PI * f64::from(freq) / f64::from(SIGNAL_SAMPLE_RATE);
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in frame {
        let s = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)
}

// ------------------------------------------------------------ the cheer band

/// `samples` through a one-pole high-pass at `low` and a one-pole low-pass at
/// `high`.
///
/// Two sections, not a design: the cheer rule asks whether the crowd band rose
/// against **its own** median minutes either side, and a gentle skirt shifts
/// both sides of that comparison equally.
fn band_pass(samples: &[f32], low: f32, high: f32) -> Vec<f32> {
    let (a_low, a_high) = (pole(low), pole(high));
    let (mut below, mut out) = (0.0f32, 0.0f32);
    samples
        .iter()
        .map(|&x| {
            below += a_low * (x - below);
            out += a_high * ((x - below) - out);
            out
        })
        .collect()
}

/// A one-pole section's coefficient for a −3 dB corner at `hz`.
fn pole(hz: f32) -> f32 {
    1.0 - (-2.0 * std::f32::consts::PI * hz / SIGNAL_SAMPLE_RATE as f32).exp()
}

// --------------------------------------------------------- the rolling median

/// The width of a histogram bucket: the median is reported to this precision,
/// which is a fortieth of the smallest threshold it is compared against.
const MEDIAN_BUCKET_DB: f32 = 0.5;

/// The lowest level the histogram distinguishes. Below it everything is
/// silence, and silence has no median worth having.
const MEDIAN_FLOOR_DB: f32 = -160.0;

/// −160 dB to +40 dB in [`MEDIAN_BUCKET_DB`] buckets.
const MEDIAN_BUCKETS: usize = 400;

/// The median of `values` over a centred window of `span`, one per value,
/// clamped at both ends of the series.
///
/// A histogram rather than a sort: a half is a hundred thousand windows and a
/// span is nearly four thousand of them, so sorting each one over costs
/// minutes. Buckets of [`MEDIAN_BUCKET_DB`] make every step O(1) at the price
/// of half a dB, and half a dB is nothing against an 8 dB threshold.
fn rolling_median(values: &[f32], span: usize) -> Vec<f32> {
    let half = span / 2;
    let bucket = |v: f32| {
        (((v - MEDIAN_FLOOR_DB) / MEDIAN_BUCKET_DB) as isize).clamp(0, MEDIAN_BUCKETS as isize - 1)
            as usize
    };
    let mut histogram = [0usize; MEDIAN_BUCKETS];
    let (mut lo, mut hi, mut count) = (0usize, 0usize, 0usize);
    let mut out = Vec::with_capacity(values.len());
    for i in 0..values.len() {
        while hi < (i + half + 1).min(values.len()) {
            histogram[bucket(values[hi])] += 1;
            count += 1;
            hi += 1;
        }
        while lo < i.saturating_sub(half) {
            histogram[bucket(values[lo])] -= 1;
            count -= 1;
            lo += 1;
        }
        let mut seen = 0;
        let mut median = MEDIAN_FLOOR_DB;
        for (b, n) in histogram.iter().enumerate() {
            seen += n;
            if seen > count / 2 {
                median = MEDIAN_FLOOR_DB + (b as f32 + 0.5) * MEDIAN_BUCKET_DB;
                break;
            }
        }
        out.push(median);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rolling_median_follows_a_ramp_rather_than_averaging_it() {
        // A 0.1 dB/step ramp: a centred median reads the value at its middle,
        // so the excess over it is nothing anywhere.
        let values: Vec<f32> = (0..1000).map(|i| -60.0 + i as f32 * 0.1).collect();
        let median = rolling_median(&values, 101);
        for i in 50..950 {
            assert!(
                (median[i] - values[i]).abs() <= MEDIAN_BUCKET_DB,
                "at {i}: median {} for value {}",
                median[i],
                values[i]
            );
        }
    }

    #[test]
    fn a_window_that_does_not_fit_is_not_a_frame() {
        assert_eq!(frame_count(0), 0);
        assert_eq!(frame_count(WINDOW_SAMPLES - 1), 0);
        assert_eq!(frame_count(WINDOW_SAMPLES), 1);
        assert_eq!(frame_count(WINDOW_SAMPLES + HOP_SAMPLES), 2);
    }
}
