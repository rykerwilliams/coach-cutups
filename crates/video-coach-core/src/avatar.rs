//! How big the avatar is, moment to moment.
//!
//! The avatar's size is a function of how loud the coach is right now, on a
//! `0..=1` scale, smoothed. That quantity is estimated two ways — the render
//! reads the recording's own audio at 30 Hz, the corner reads the recorder's
//! `level` element at 10 Hz — and everything here is shared between them so
//! the two say the same thing. Both are estimators of one curve; neither is
//! persisted (spec D4), because the file it is derived from already is.
//!
//! **Everything outside this module carries a level in `0..=1`, never a
//! scale.** [`PULSE_GROWTH`] is applied in [`avatar_rect`] and nowhere else.

use crate::export::OUTPUT_FPS;
use crate::layout::Rect;

/// The rate the render decodes the commentary at, mono. The same rate
/// transcription uses.
pub const PULSE_RATE: u32 = 16_000;

/// The loudest inset is exactly the rect it is given — [`avatar_box`] for an
/// avatar, `layout::pip_rect` for a camera — and the resting one is this much
/// smaller. 1.10 means the avatar grows 10% from rest to full.
pub const PULSE_GROWTH: f64 = 1.10;

/// Quiet enough to be at rest. Below this the avatar does not move.
pub const PULSE_FLOOR_DB: f64 = -45.0;
/// Loud enough to be at full size. Speech RMS, not peak — see [`level_from_db`].
pub const PULSE_CEILING_DB: f64 = -12.0;

/// The filter's time constant while the level is rising, in seconds.
pub const PULSE_ATTACK: f64 = 0.06;
/// And while it is falling: longer, so a word ends by settling rather than
/// snapping shut.
pub const PULSE_RELEASE: f64 = 0.22;

/// `v` in `0..=1`, with a non-number reading as silence rather than
/// propagating into a `NaN` rect.
fn clamp01(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// `db` mapped into `0..=1` against the floor and the ceiling.
///
/// Whatever is fed in must be an **RMS** dBFS: speech has a 10–14 dB crest
/// factor, so a peak through these thresholds would sit near the ceiling
/// while the same breath's RMS was barely off the floor (spec D1).
pub fn level_from_db(db: f64) -> f64 {
    clamp01((db - PULSE_FLOOR_DB) / (PULSE_CEILING_DB - PULSE_FLOOR_DB))
}

/// One step of the one-pole filter: `a = 1 − exp(−dt / τ)`, with τ the attack
/// while rising and the release while falling.
///
/// It holds no state and takes `dt`, because both estimators call it at their
/// own rate — the render at `1/30`, the live corner at `0.1`.
pub fn smooth(previous: f64, target: f64, dt: f64) -> f64 {
    let tau = if target >= previous {
        PULSE_ATTACK
    } else {
        PULSE_RELEASE
    };
    previous + (1.0 - (-dt / tau).exp()) * (target - previous)
}

/// One level per output frame of an entry, each in `0.0..=1.0`, smoothed.
///
/// The window is the output frame itself — `[n / fps, (n + 1) / fps)` of the
/// recording, 533 samples at 16 kHz — so there is one sampling grid, not two.
/// That is already several pitch periods of a voice, so the RMS is stable
/// before any smoothing. A window past the end of `samples_mono` is silence,
/// which is what an entry longer than its recording gets.
pub fn pulse(samples_mono: &[f32], rate: u32, frames: usize) -> Vec<f64> {
    let fps = u64::from(OUTPUT_FPS);
    let rate = u64::from(rate);
    let mut levels = Vec::with_capacity(frames);
    let mut smoothed = 0.0;
    for n in 0..frames {
        let start = (n as u64 * rate / fps) as usize;
        let end = ((n as u64 + 1) * rate / fps) as usize;
        let window = samples_mono
            .get(start.min(samples_mono.len())..end.min(samples_mono.len()))
            .unwrap_or(&[]);
        let level = if window.is_empty() {
            0.0
        } else {
            let mean_square = window
                .iter()
                .map(|&x| f64::from(x) * f64::from(x))
                .sum::<f64>()
                / window.len() as f64;
            // The floor keeps digital silence off log10's asymptote; it is far
            // below `PULSE_FLOOR_DB` either way.
            level_from_db(20.0 * mean_square.sqrt().max(1e-6).log10())
        };
        smoothed = smooth(smoothed, level, 1.0 / f64::from(OUTPUT_FPS));
        levels.push(smoothed);
    }
    levels
}

/// How large the avatar's circle is against the webcam inset a camera take
/// would fill — the user asked for smaller, 2026-09-23; it is one line to
/// retune.
pub const AVATAR_BOX_RATIO: f64 = 0.75;

/// The box an avatar's circle is drawn in at its loudest: `pip` shrunk by
/// [`AVATAR_BOX_RATIO`] about its **bottom-right corner**, so the circle keeps
/// the webcam inset's own right and bottom margins and only gets smaller.
///
/// This is the whole of the difference between an avatar's footprint and a
/// camera's; the pulse still grows the circle concentrically *inside* this box
/// ([`avatar_rect`]), so nothing else moves.
pub fn avatar_box(pip: Rect) -> Rect {
    let (w, h) = (pip.w * AVATAR_BOX_RATIO, pip.h * AVATAR_BOX_RATIO);
    Rect {
        x: pip.x + pip.w - w,
        y: pip.y + pip.h - h,
        w,
        h,
    }
}

/// Where the avatar is drawn: `pip` at `level == 1.0`, and `pip` scaled about
/// its centre by `1.0 / PULSE_GROWTH` at `level == 0.0`, lerped between.
///
/// It never exceeds the rect it is given — [`avatar_box`] on the avatar paths,
/// the webcam inset itself on the camera's — so the footprint is that rect's,
/// and a level outside `0..=1` clamps.
pub fn avatar_rect(pip: Rect, level: f64) -> Rect {
    let level = clamp01(level);
    // The lerp is written so the endpoints are exact: at `level == 1.0` the
    // scale is 1.0 and the rect is `pip` itself, which is what lets the live
    // corner run a camera take through here unchanged (spec G2).
    let scale = (1.0 / PULSE_GROWTH) * (1.0 - level) + level;
    let (w, h) = (pip.w * scale, pip.h * scale);
    Rect {
        x: pip.x + (pip.w - w) / 2.0,
        y: pip.y + (pip.h - h) / 2.0,
        w,
        h,
    }
}
