//! The analysis pass's sound (spec D1): one source video, decoded whole to the
//! 16 kHz mono core's signals are defined at.
//!
//! ```text
//! filesrc ! decodebin3 (the audio stream only)
//!         ! audioconvert ! audioresample ! appsink F32LE/16k/1ch
//! ```
//!
//! **No new decode path.** That is the export's own
//! [`Reader`](crate::composite::audio), asked for the caps transcription
//! already asks it for; this module is its third caller.
//!
//! **The whole half at once, in memory.** 16 kHz of `f32` is 64 KB a second,
//! so the longest file in the design footage is 27 MB — less than one decoded
//! 1080p frame costs twice over, and cheaper than any arrangement that streams
//! it. It is also what lets every rule in
//! [`signals`](video_coach_core::signals) stay a pure function of a slice.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use video_coach_core::signals::SIGNAL_SAMPLE_RATE;

use super::AnalyzeError;
use crate::composite::audio;

/// One channel: the signals are band levels, and a stereo pair of a single
/// camera microphone says nothing a mix-down loses.
const ANALYZE_CHANNELS: usize = 1;

/// All of `path`'s sound at [`SIGNAL_SAMPLE_RATE`], mono.
///
/// One pass, no seeking, and the cancel flag is looked at before every pull —
/// a source whose analysis is cancelled stops within a buffer rather than at
/// the end of the file.
///
/// A file with no audio track is an **error**, not silence: a half with no
/// sound has no whistles and no cheers, and reporting none of either would
/// read as a detector that found nothing rather than a file that could say
/// nothing.
pub fn samples(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>, AnalyzeError> {
    audio::read_all(path, SIGNAL_SAMPLE_RATE, ANALYZE_CHANNELS, cancel)
}
