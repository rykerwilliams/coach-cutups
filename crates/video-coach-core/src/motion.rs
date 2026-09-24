//! What the picture does (spec D1, D3): where it holds still, and where it
//! looks like a kick-off — as pure functions over the number series and the
//! thumbnail grid [`analyze::motion`] decodes.
//!
//! Media decodes; core measures, exactly as for [`signals`](crate::signals).
//! Two series come out of the one decode pass, five frames a second of
//! [`MOTION_HZ`] motion and one thumbnail a second, and this module is
//! everything that reads them.
//!
//! # Two ways to find a kick-off
//!
//! **Stillness** is the spec's own (D3): after a goal the players walk back,
//! the ball sits on the centre spot, the virtual camera holds, and then the
//! restart is sudden. It asks nothing of the picture but that it stop
//! changing, which is why it also fires on an injury, a water break and every
//! long throw-in.
//!
//! **Picture similarity** is the coach's: "kick offs are a kind of known
//! picture". On this footage they are right and it is worth measuring
//! separately — the camera is a fixed tripod with a virtual pan, so the
//! halfway line, the centre circle and the two teams split either side of it
//! frame almost identically at every kick-off of every match. A kick-off is
//! then a frame that correlates with a **known** one.
//!
//! The comparison is a zero-mean, unit-norm correlation ([`Thumbnail`]), so
//! two kick-offs an hour and a venue apart are the same picture through
//! different light. What it cannot survive is a different camera position,
//! which is the question of whether one match's [`Template`] finds another's
//! kick-offs at all, and so of whether a shipped detector needs per-match
//! calibration. P3 measures it; nothing here assumes the answer.
//!
//! **Every constant here is an initial value that P3 replaces** with one
//! chosen on the tuning match (spec G2), and they are `pub` so the
//! ground-truth run can print them and sweep around them.

use std::ops::Range;

/// The rate the motion series is sampled at: media's pass drops the decode to
/// five frames a second before it uploads anything.
pub const MOTION_HZ: f64 = 5.0;

/// The rate the thumbnail grid is sampled at. One a second is enough for a
/// picture rule — a kick-off's framing holds for many seconds — and it is
/// what keeps a whole half's thumbnails at about a megabyte.
pub const THUMBNAIL_HZ: f64 = 1.0;

/// The thumbnail grid's width. Small on purpose: at this size a player is
/// under a pixel, so what correlates is the *framing* — the line, the circle,
/// the crowd's edge — and not who is standing where.
pub const THUMBNAIL_WIDTH: usize = 32;

/// The thumbnail grid's height, at 16:9.
pub const THUMBNAIL_HEIGHT: usize = 18;

/// Below this the picture counts as still. **Initial value** (spec D2), in
/// the units of the mean absolute luma difference media computes: 0–255 per
/// pixel.
pub const STILL_THETA: f32 = 3.0;

/// The shortest hold that counts as a walk-back. **Initial value** (spec D3).
pub const STILL_MIN_SECONDS: f64 = 10.0;

/// How alike a frame and a known kick-off must be to be one. **Initial
/// value**, and the one P3's sweep has the least prior information about: a
/// correlation of 1.0 is the same picture and 0.0 is an unrelated one.
pub const KICKOFF_SIMILARITY: f32 = 0.8;

/// How far apart two picture peaks must be to be two kick-offs. A kick-off's
/// framing holds for several seconds either side of the whistle, so without a
/// gap one kick-off is a dozen of them. **Initial value**; the closest two
/// goals in the design footage are 192 s apart, so nothing realistic is lost
/// at this width.
pub const KICKOFF_MIN_GAP_SECONDS: f64 = 30.0;

/// Every still interval in `motion`, at this module's constants.
///
/// `hz` is the series' sample rate ([`MOTION_HZ`] for media's pass).
pub fn still_intervals(motion: &[f32], hz: f64) -> Vec<Range<f64>> {
    still_intervals_at(motion, hz, STILL_THETA, STILL_MIN_SECONDS)
}

/// The still intervals of `motion` at the given threshold and floor.
///
/// Split out from [`still_intervals`] so a whole sweep of θ costs one decode
/// (spec G2).
///
/// **A single frame over θ ends an interval.** There is no tolerance, and it
/// is a choice rather than an oversight: at 5 fps one frame is 200 ms, and on
/// this footage a lone spike inside a hold is the virtual camera correcting
/// itself, which is movement. A hold broken by one flicker is two holds, and
/// [`kickoffs`](crate) reads the end of either as a restart — so the
/// tolerant rule would only ever merge a candidate away, never add one.
pub fn still_intervals_at(
    motion: &[f32],
    hz: f64,
    theta: f32,
    min_seconds: f64,
) -> Vec<Range<f64>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < motion.len() {
        if motion[i] >= theta {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < motion.len() && motion[end] < theta {
            end += 1;
        }
        // Frame `i` covers `[i/hz, (i+1)/hz)`, so a run of `n` frames is
        // `n/hz` long — the floor is inclusive, and a hold of exactly
        // [`STILL_MIN_SECONDS`] counts.
        if (end - i) as f64 / hz >= min_seconds {
            out.push(i as f64 / hz..end as f64 / hz);
        }
        i = end;
    }
    out
}

/// One greyscale frame on the [`THUMBNAIL_WIDTH`] × [`THUMBNAIL_HEIGHT`]
/// grid, with its mean taken out and its length normalised.
///
/// That normalisation is the whole of what makes two frames comparable: the
/// dot product of two of these is their **zero-mean normalised
/// cross-correlation**, 1.0 for the same picture at any exposure and 0.0 for
/// unrelated ones, so a cloudy second half and a sunlit first one score the
/// same against one template.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    /// Zero-mean and unit-norm, row-major, or all zeros for a frame with no
    /// detail at all.
    pixels: Vec<f32>,
}

impl Thumbnail {
    /// `luma` (row-major, `width` × `height`, any scale) on this module's
    /// grid.
    ///
    /// The source is box-averaged rather than sampled, so the grid carries
    /// what a region looks like on average rather than what one pixel of it
    /// happened to be — at 32 × 18 a sampled thumbnail of moving grass is
    /// mostly noise.
    pub fn from_luma(luma: &[f32], width: usize, height: usize) -> Thumbnail {
        assert_eq!(
            luma.len(),
            width * height,
            "a {width}x{height} frame is {} samples, not {}",
            width * height,
            luma.len()
        );
        assert!(
            width >= THUMBNAIL_WIDTH && height >= THUMBNAIL_HEIGHT,
            "a {width}x{height} frame is smaller than the {THUMBNAIL_WIDTH}x{THUMBNAIL_HEIGHT} grid"
        );
        let mut pixels = Vec::with_capacity(THUMBNAIL_WIDTH * THUMBNAIL_HEIGHT);
        for cy in 0..THUMBNAIL_HEIGHT {
            let rows = cy * height / THUMBNAIL_HEIGHT..(cy + 1) * height / THUMBNAIL_HEIGHT;
            for cx in 0..THUMBNAIL_WIDTH {
                let columns = cx * width / THUMBNAIL_WIDTH..(cx + 1) * width / THUMBNAIL_WIDTH;
                let mut sum = 0.0f64;
                let mut n = 0u32;
                for y in rows.clone() {
                    for x in columns.clone() {
                        sum += f64::from(luma[y * width + x]);
                        n += 1;
                    }
                }
                pixels.push((sum / f64::from(n.max(1))) as f32);
            }
        }
        let mean = pixels.iter().map(|&p| f64::from(p)).sum::<f64>() / pixels.len() as f64;
        for p in &mut pixels {
            *p -= mean as f32;
        }
        let norm = pixels
            .iter()
            .map(|&p| f64::from(p) * f64::from(p))
            .sum::<f64>()
            .sqrt();
        // A frame with no detail — a fade to black, a blank cover — correlates
        // with nothing rather than with everything, which is what a division
        // by its own noise would give.
        if norm > 1e-6 {
            for p in &mut pixels {
                *p = (f64::from(*p) / norm) as f32;
            }
        } else {
            pixels.fill(0.0);
        }
        Thumbnail { pixels }
    }

    /// How alike two frames are: −1 to 1, and 1.0 is the same picture.
    pub fn similarity(&self, other: &Thumbnail) -> f32 {
        self.pixels
            .iter()
            .zip(&other.pixels)
            .map(|(&a, &b)| f64::from(a) * f64::from(b))
            .sum::<f64>() as f32
    }
}

/// Known kick-off frames, to measure a half against.
///
/// **Which frames are in it is the caller's business, and the measurement's
/// whole point.** A template built from a half's own kick-offs would score
/// them perfectly and say nothing; the ground-truth run holds a match out of
/// its own template and reports which frames it kept.
#[derive(Debug, Clone, Default)]
pub struct Template {
    frames: Vec<Thumbnail>,
}

impl Template {
    pub fn new(frames: Vec<Thumbnail>) -> Template {
        Template { frames }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// How much `frame` looks like the **closest** known kick-off.
    ///
    /// The best member rather than the mean of them: two kick-offs at the two
    /// ends of a match are framed a little differently, and averaging them
    /// blurs away the detail each one is recognised by. An empty template
    /// scores −1, below any threshold.
    pub fn score(&self, frame: &Thumbnail) -> f32 {
        self.frames
            .iter()
            .map(|known| known.similarity(frame))
            .fold(-1.0, f32::max)
    }

    /// [`Template::score`] over a whole half's thumbnails.
    pub fn scores(&self, frames: &[Thumbnail]) -> Vec<f32> {
        frames.iter().map(|frame| self.score(frame)).collect()
    }
}

/// One local best of a similarity series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    /// Seconds into the source.
    pub seconds: f64,
    pub score: f32,
}

/// The peaks of `scores` (sampled at `hz`) over `threshold`, no two within
/// `min_gap_seconds`, in time order.
///
/// Greedy from the top: the highest score takes its neighbourhood, then the
/// highest of what is left. A kick-off's framing holds either side of the
/// restart, so without the gap one kick-off is a run of them; taking the best
/// of each neighbourhood rather than the first keeps the frame that looks
/// most like one.
pub fn peaks(scores: &[f32], hz: f64, threshold: f32, min_gap_seconds: f64) -> Vec<Peak> {
    let gap = (min_gap_seconds * hz).round() as usize;
    let mut taken = vec![false; scores.len()];
    let mut out: Vec<Peak> = Vec::new();
    loop {
        let best = scores
            .iter()
            .enumerate()
            .filter(|&(i, &score)| !taken[i] && score >= threshold)
            .max_by(|(_, a), (_, b)| a.total_cmp(b));
        let Some((i, &score)) = best else { break };
        out.push(Peak {
            seconds: i as f64 / hz,
            score,
        });
        for t in taken[i.saturating_sub(gap)..(i + gap + 1).min(scores.len())].iter_mut() {
            *t = true;
        }
    }
    out.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
    out
}
