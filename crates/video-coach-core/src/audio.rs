//! The audio edit: which span of which file is heard at each output sample,
//! how loud, and the fades at its edges.
//!
//! Core owns the splice; media owns the samples (spec E3). The two tracks are
//! the game video's own audio — heard **only while the source is playing**, so
//! a freeze is silent — and the commentary recording, heard throughout its
//! entry.
//!
//! # The emitted timeline
//!
//! Every position here is an **emitted sample**: an index into the stream
//! handed to the AAC encoder, which is the output timeline with its first
//! [`PRIMING_SAMPLES`] samples dropped (spec E3, measured). So emitted sample
//! `s` is output time `(s + PRIMING_SAMPLES) / AUDIO_SAMPLE_RATE`, and output
//! frame `n` is emitted sample `n · 1600 − 1024`.
//!
//! That shift is not cosmetic: the ramps have to live on the emitted timeline
//! or the first region's fade-in is inside the dropped head and the export
//! opens at full gain — the click the ramp rule exists to remove. Expressing
//! the regions themselves in emitted samples is what makes that true by
//! construction, and it leaves the caller one mapping instead of two: emitted
//! sample `s` of a region reads its file at
//! `source_offset + (s − out_samples.start) / AUDIO_SAMPLE_RATE`.

use std::ops::Range;

use crate::export::{frame_count, Compilation, OUTPUT_FPS};
use crate::plan::PlanEntry;
use crate::project::Preferences;
use crate::timeline::SegmentKind;

/// The mixing rate. Media decodes every audio file to F32LE at this rate, in
/// two channels; a sample position here counts **frames of audio**, not
/// interleaved floats.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// Audio samples per output video frame.
///
/// Exact by construction — the const assert below fails the build if a future
/// rate or fps makes it fractional, because a fractional value would put entry
/// boundaries between samples and let rounding drift across a long run.
const SAMPLES_PER_FRAME: u64 = (AUDIO_SAMPLE_RATE / OUTPUT_FPS) as u64;
const _: () = assert!(AUDIO_SAMPLE_RATE.is_multiple_of(OUTPUT_FPS));

/// Samples `avenc_aac` prepends as priming, and therefore the number media
/// drops from the head of the mixed stream so the sound lands where the picture
/// does (measured: 1024 samples, 21.3 ms — spec E3).
///
/// Core needs it because the ramps are computed on the post-drop timeline; the
/// drop itself happens in media.
pub const PRIMING_SAMPLES: u64 = 1024;

/// The fade's length: 5 ms at [`AUDIO_SAMPLE_RATE`].
pub const RAMP_SAMPLES: u64 = AUDIO_SAMPLE_RATE as u64 / 200;

/// Which file a region is heard from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// The entry's source video, at `source_offset` into it.
    Game,
    /// The entry's commentary recording, at `source_offset` into it.
    Commentary,
}

/// One contiguous stretch of one file, at one gain.
///
/// Regions of the two tracks overlap; regions of the same track never do.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// Index into `CompilationPlan::entries`, naming the source video or the
    /// recording this region reads — the same index [`crate::export::FrameSpec`]
    /// carries, rather than repeating a filename per region.
    pub entry: usize,
    pub track: Track,
    /// The emitted samples this region covers (see the module docs).
    pub out_samples: Range<u64>,
    /// Time into the file at `out_samples.start`.
    pub source_offset: f64,
    pub gain: f64,
}

/// The multiplier for emitted `sample`: the region's gain, shaped by the fades
/// at its edges. Zero outside the region.
///
/// Both edges ramp linearly over [`RAMP_SAMPLES`], from silence and back to it.
/// Taking the **smaller** of the two ramps handles a region shorter than two of
/// them without a special case: it stays quiet rather than jumping to full
/// gain, which is what a click-avoidance rule should do with a 3 ms fragment.
///
/// The ramp windows are clamped to the region, so nothing is evaluated before
/// the emitted timeline's t = 0: the first region fades in over the first 5 ms
/// the encoder actually receives instead of starting part-way up a ramp that
/// the priming drop swallowed.
pub fn envelope(region: &Region, sample: u64) -> f64 {
    if !region.out_samples.contains(&sample) {
        return 0.0;
    }
    let rise = sample - region.out_samples.start;
    let fall = region.out_samples.end - 1 - sample;
    region.gain * (rise.min(fall).min(RAMP_SAMPLES) as f64 / RAMP_SAMPLES as f64)
}

/// Every audio region of `compilation`, in output order.
///
/// Gains are the **preview** volumes: one pair of numbers decides how loud a
/// clip is in the app and in the file, so what the coach hears while recording
/// is what the export contains.
///
/// Adjacent play segments stay separate regions even when the source is
/// continuous across them. A `Skip` or `Play` event is a source discontinuity,
/// and media seeks its audio pipeline per region regardless, so the fade pair
/// at the join is honest rather than an artefact of the splice.
pub fn audio_regions(compilation: &Compilation, prefs: &Preferences) -> Vec<Region> {
    let mut regions = Vec::new();
    for (i, entry) in compilation.plan.entries.iter().enumerate() {
        let end = entry.start_frame + entry.frames;
        game_regions(entry, i, prefs.preview_source_volume, &mut regions);
        // One per entry: the recording runs from its own zero for the whole
        // entry, freezes included.
        regions.extend(region(
            i,
            Track::Commentary,
            entry.start_frame..end,
            0.0,
            prefs.preview_commentary_volume,
        ));
    }
    regions
}

/// Append `entry`'s play segments as game regions; freezes contribute nothing,
/// which is how they come out silent.
///
/// Segments are placed on the same whole frames the picture uses: a boundary at
/// `s` seconds into the entry belongs to the frame
/// [`frame_count`]`(s)` — the rule `export::walk` applies to the video, epsilon
/// and all, so sound and picture switch on the same frame. The last segment
/// runs to the entry's end, taking the up-to-one-frame of quantization with it
/// exactly as the picture does.
fn game_regions(entry: &PlanEntry, index: usize, gain: f64, out: &mut Vec<Region>) {
    let mut seconds = 0.0;
    let mut first = 0;
    for (n, seg) in entry.segments.iter().enumerate() {
        seconds += seg.out_duration;
        let last = if n + 1 == entry.segments.len() {
            entry.frames
        } else {
            frame_count(seconds).min(entry.frames)
        };
        if seg.kind == SegmentKind::Play {
            out.extend(region(
                index,
                Track::Game,
                entry.start_frame + first..entry.start_frame + last,
                seg.source_start,
                gain,
            ));
        }
        first = last;
    }
}

/// A region over output frames `frames`, moved onto the emitted timeline.
///
/// `None` when nothing survives: an entry or a segment that got no frame, or —
/// only ever for the very first region — a span living entirely inside the
/// dropped head. What the drop removes from the head of a region is added to
/// its `source_offset`, so the region still reads the right audio: the export
/// loses its first 21 ms, it does not play them late.
fn region(
    entry: usize,
    track: Track,
    frames: Range<usize>,
    source_start: f64,
    gain: f64,
) -> Option<Region> {
    let start = frames.start as u64 * SAMPLES_PER_FRAME;
    let end = frames.end as u64 * SAMPLES_PER_FRAME;
    let dropped = PRIMING_SAMPLES.min(end).saturating_sub(start);
    let out_samples = start.saturating_sub(PRIMING_SAMPLES)..end.saturating_sub(PRIMING_SAMPLES);
    if out_samples.is_empty() {
        return None;
    }
    Some(Region {
        entry,
        track,
        out_samples,
        source_offset: source_start + dropped as f64 / f64::from(AUDIO_SAMPLE_RATE),
        gain,
    })
}
