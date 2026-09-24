//! What the crowd and the referee sound like (spec D2): whistles, cheers and
//! applause, as pure functions over a slice of 16 kHz mono samples.
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
//! # Why a texture cue as well as a level one
//!
//! [`cheers`] asks one question: did the crowd band get louder than it has
//! recently been? Measured over six halves that found 7 of 16 tagged goals and
//! never more than 11 anywhere in a threshold sweep — a handful of parents on a
//! touchline is not loud from the halfway line, whatever the threshold.
//!
//! Applause is quiet but **textured**: a train of sharp broadband transients,
//! tens a second, each with a millisecond attack and almost no sustain. A shout
//! is one sustained broadband sound, wind is smooth, a whistle is a tone — none
//! of them is a train of transients. So [`clap_texture`] measures the *shape*
//! of the sound above 2 kHz rather than its level: **the onset rate**, how many
//! sharp rises of [`ONSET_RISE_DB`] a second the band's 2 ms envelope holds,
//! taken **in dB over its own 60 s rolling median** exactly as a level is,
//! because a venue's baseline clatter differs as much as its baseline level.
//!
//! ## What it measured, and why it is not the detector
//!
//! The cue is real and it is not enough. Over the same six halves and the same
//! sixteen tagged goals, compared against [`cheers`] **at matched firings a
//! half** on the two held-out matches:
//!
//! | firings a half | level (`cheers`) | texture (`claps`) |
//! |---|---|---|
//! | ~6 | 4/9 | 3/9 |
//! | ~13–19 | 6/9 at 13, 8/9 at 19 | 4/9 at 19 |
//! | ~57–67 | 9/9 at 57 | 6/9 at 67 |
//!
//! The texture never wins at a firing rate anyone would keep, and the union of
//! the two is worse than the level cue alone at the same total rate. It does
//! win on the one match whose crowd is inaudible — 4 of 7 goals against the
//! level cue's 1 — which is why it is kept here rather than deleted, and why
//! it is **the tuning match**, so that win is exactly the one a held-out split
//! exists to distrust.
//!
//! Two things measured and **not** kept, so they are not rebuilt: the
//! envelope's modulation depth (its 95th percentile over its own median across
//! a second) scored 0/9 at every firing rate under 30 a half and barely over
//! chance above that; and spectral flatness is [`WHISTLE_TONALITY_DB`] upside
//! down, which the whistle work already measured as no separator of a shout
//! from anything.
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

/// The clap band's lower edge: above a voice's fundamental and its first
/// formant, where a hand clap puts most of its energy and a shout does not.
///
/// There is no upper edge. The signals run at 16 kHz, so the band is 2 kHz to
/// Nyquist and a low-pass section would only shave the top of it.
pub const CLAP_BAND_LOW_HZ: f32 = 2_000.0;

/// The envelope's resolution: 2 ms, which is about a clap's attack and short
/// enough that two claps 20 ms apart are two.
pub const ENVELOPE_SAMPLES: usize = 32;

/// How far back an onset's rise is measured: three envelope blocks, 6 ms.
const ONSET_RISE_BLOCKS: usize = 3;

/// The shortest gap between two counted onsets: five blocks, 10 ms. One clap
/// has one attack however long its tail rings, and 10 ms caps the rate at 100
/// a second, far above any real clapping.
const ONSET_REFRACTORY_BLOCKS: usize = 5;

/// How far the band's envelope must rise in [`ONSET_RISE_BLOCKS`] to count as
/// an onset. **Initial value**, and the one threshold here that needs no
/// rolling median: it is already a ratio, so it means the same in both venues.
pub const ONSET_RISE_DB: f32 = 6.0;

/// The block the texture is measured over: a second, which holds tens of claps
/// and only one of anything else.
pub const CLAP_TEXTURE_SECONDS: f64 = 1.0;

/// The lowest onset rate the texture distinguishes, in onsets a second.
///
/// Applause is *tens* of transients a second, so anything under two of them is
/// read as none. Without a floor this high, a half whose background holds no
/// transients at all would call one stray knock a doubling of the rate — a
/// rolling median of zeros has nothing to be a ratio against.
const CLAP_RATE_FLOOR: f32 = 2.0;

/// How far the onset rate must rise over its rolling median, in dB — so 3 dB
/// is "twice as many transients a second as this half usually holds".
/// **Initial value** P3 replaces.
pub const CLAP_RATE_SNR_DB: f32 = 3.0;

/// How long either rise must hold to be a burst of applause. **Initial
/// value**, the same floor [`CHEER_MIN_SECONDS`] starts at, so the two cues are
/// compared on equal terms.
pub const CLAP_MIN_SECONDS: f64 = 1.0;

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

/// One burst of applause-shaped sound: a stretch whose texture stood over its
/// own recent past, whether or not its *level* did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clap {
    /// Seconds into the samples, at the centre of the first window that held
    /// it.
    pub onset: f64,
    /// How long it held, in whole hops.
    pub duration: f64,
    /// The loudest window's rise over the rolling median, in dB.
    pub peak_db: f32,
    /// The onset rate at that window, in onsets a second — the human-readable
    /// half of the number, because "+6 dB of texture" says nothing on its own
    /// about whether that is thirty claps or six.
    pub rate: f32,
}

/// What [`clap_texture`] measures, one value every [`HOP_SECONDS`] on the same
/// grid as [`cheer_excess`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClapTexture {
    /// Sharp rises a second in the clap band's 2 ms envelope, over a
    /// [`CLAP_TEXTURE_SECONDS`] block centred on the window.
    pub rate: Vec<f32>,
    /// [`ClapTexture::rate`] in dB over its own [`MEDIAN_SECONDS`] rolling
    /// median.
    pub rate_excess: Vec<f32>,
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
    excess_over_median(&level)
}

/// The cheers in a [`cheer_excess`] series at the given thresholds.
pub fn cheers_from(excess: &[f32], snr_db: f32, min_seconds: f64) -> Vec<Cheer> {
    runs(excess, snr_db, min_seconds)
        .map(|(i, end)| Cheer {
            onset: frame_time(i),
            duration: run_seconds(i, end),
            peak_db: peak(&excess[i..end]),
        })
        .collect()
}

/// Every burst of applause-shaped sound in `samples` (16 kHz mono), at this
/// module's constants, on the onset-rate series.
pub fn claps(samples: &[f32]) -> Vec<Clap> {
    let texture = clap_texture(samples);
    claps_from(
        &texture.rate_excess,
        &texture.rate,
        CLAP_RATE_SNR_DB,
        CLAP_MIN_SECONDS,
    )
}

/// The bursts in one of [`clap_texture`]'s excess series at the given
/// thresholds, annotated with the onset rate at each burst's peak.
///
/// Split from [`clap_texture`] for the reason [`cheer_excess`] is split from
/// [`cheers`]: the texture pass is what costs the minutes, and a whole grid of
/// `(snr_db, min_seconds)` over either series costs one pass (spec G2).
pub fn claps_from(excess: &[f32], rate: &[f32], snr_db: f32, min_seconds: f64) -> Vec<Clap> {
    runs(excess, snr_db, min_seconds)
        .map(|(i, end)| {
            let loudest = (i..end)
                .max_by(|&a, &b| excess[a].total_cmp(&excess[b]))
                .expect("a run holds at least one window");
            Clap {
                onset: frame_time(i),
                duration: run_seconds(i, end),
                peak_db: excess[loudest],
                rate: rate.get(loudest).copied().unwrap_or(0.0),
            }
        })
        .collect()
}

/// How textured `samples` (16 kHz mono) are, at [`ONSET_RISE_DB`].
pub fn clap_texture(samples: &[f32]) -> ClapTexture {
    clap_texture_at(samples, ONSET_RISE_DB)
}

/// How textured `samples` (16 kHz mono) are, window by window, counting a rise
/// of `rise_db` as an onset.
///
/// One pass: the band, its 2 ms envelope, the onsets in that envelope, and how
/// many of them a second. The series is not a level — a clap train ten dB
/// *under* a shout scores higher on it.
///
/// `rise_db` is a parameter rather than only a constant because it is the one
/// threshold the cheaper split cannot reach: [`claps_from`]'s two thresholds
/// sweep for free over a finished series, and this one changes the series
/// itself (spec G2).
pub fn clap_texture_at(samples: &[f32], rise_db: f32) -> ClapTexture {
    let frames = frame_count(samples.len());
    if frames == 0 {
        return ClapTexture::default();
    }
    let band = high_pass_2(samples, CLAP_BAND_LOW_HZ);
    let envelope = envelope_db(&band);
    let block = (CLAP_TEXTURE_SECONDS * f64::from(SIGNAL_SAMPLE_RATE) / ENVELOPE_SAMPLES as f64)
        .round() as usize;

    // The onset count up to each envelope block, so a centred block's rate is
    // one subtraction however wide the block is.
    let flags = onset_flags(&envelope, rise_db);
    let mut counted = Vec::with_capacity(flags.len() + 1);
    counted.push(0u32);
    for flag in &flags {
        counted.push(counted[counted.len() - 1] + u32::from(*flag));
    }

    let mut rate = Vec::with_capacity(frames);
    for i in 0..frames {
        // The envelope block holding this window's centre. `frame_time` is in
        // seconds; this is the same instant counted in 2 ms blocks.
        let centre = (i * HOP_SAMPLES + WINDOW_SAMPLES / 2) / ENVELOPE_SAMPLES;
        let lo = centre.saturating_sub(block / 2);
        let hi = (centre + block / 2 + 1).min(flags.len());
        let seconds = (hi.saturating_sub(lo)) as f64 * ENVELOPE_SAMPLES as f64
            / f64::from(SIGNAL_SAMPLE_RATE);
        let onsets = f64::from(counted[hi] - counted[lo]);
        rate.push(if seconds > 0.0 {
            (onsets / seconds) as f32
        } else {
            0.0
        });
    }

    let rate_db: Vec<f32> = rate
        .iter()
        .map(|r| 10.0 * r.max(CLAP_RATE_FLOOR).log10())
        .collect();
    ClapTexture {
        rate_excess: excess_over_median(&rate_db),
        rate,
    }
}

// ------------------------------------------------------------- the framing

/// Every maximal run of `excess` at or over `threshold` that lasts at least
/// `min_seconds`, as `[start, end)` window indices.
fn runs(excess: &[f32], threshold: f32, min_seconds: f64) -> impl Iterator<Item = (usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < excess.len() {
        if excess[i] < threshold {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < excess.len() && excess[end] >= threshold {
            end += 1;
        }
        if run_seconds(i, end) >= min_seconds {
            out.push((i, end));
        }
        i = end;
    }
    out.into_iter()
}

/// A `[start, end)` run's length in seconds, measured between the first and
/// last windows' centres — so a run of one window is 0 s and never clears a
/// floor.
fn run_seconds(start: usize, end: usize) -> f64 {
    (end - start - 1) as f64 * HOP_SECONDS
}

fn peak(values: &[f32]) -> f32 {
    values.iter().copied().fold(f32::MIN, f32::max)
}

/// A series in dB over its own [`MEDIAN_SECONDS`] rolling median.
fn excess_over_median(values: &[f32]) -> Vec<f32> {
    let median = rolling_median(values, median_span());
    values
        .iter()
        .zip(&median)
        .map(|(value, median)| value - median)
        .collect()
}

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

// ------------------------------------------------------------- the clap band

/// `samples` through two one-pole high-pass sections at `hz`, −12 dB an octave.
///
/// Two rather than the cheer band's one: the thing this band has to reject is a
/// shout, whose energy is an octave and a half below the corner, and a single
/// pole leaves 15 dB of it in the band. The skirt's exact shape does not matter
/// — every threshold downstream is a ratio against this same band's own past.
fn high_pass_2(samples: &[f32], hz: f32) -> Vec<f32> {
    let a = pole(hz);
    let (mut below_a, mut below_b) = (0.0f32, 0.0f32);
    samples
        .iter()
        .map(|&x| {
            below_a += a * (x - below_a);
            let once = x - below_a;
            below_b += a * (once - below_b);
            once - below_b
        })
        .collect()
}

/// The power of each [`ENVELOPE_SAMPLES`] block of `band`, in dB.
///
/// Blocks, not a smoothed rectifier: a clap's attack is a step in this series
/// and its tail is the decay, which is exactly what [`onset_flags`] reads.
fn envelope_db(band: &[f32]) -> Vec<f32> {
    band.as_chunks::<ENVELOPE_SAMPLES>()
        .0
        .iter()
        .map(|block| {
            power_db(
                block
                    .iter()
                    .map(|&s| f64::from(s) * f64::from(s))
                    .sum::<f64>(),
            )
        })
        .collect()
}

/// Which blocks of `envelope` begin a transient: a rise of `rise_db` across
/// [`ONSET_RISE_BLOCKS`], no nearer than [`ONSET_REFRACTORY_BLOCKS`] to the
/// last one.
///
/// The **first** block of the attack is the onset, not the peak. Only the count
/// is used, and taking the first is what makes the refractory gap mean "one
/// clap" rather than "one clap's loudest 2 ms".
fn onset_flags(envelope: &[f32], rise_db: f32) -> Vec<bool> {
    let mut out = vec![false; envelope.len()];
    let mut allowed = ONSET_RISE_BLOCKS;
    for n in ONSET_RISE_BLOCKS..envelope.len() {
        if n >= allowed && envelope[n] - envelope[n - ONSET_RISE_BLOCKS] >= rise_db {
            out[n] = true;
            allowed = n + ONSET_REFRACTORY_BLOCKS;
        }
    }
    out
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
