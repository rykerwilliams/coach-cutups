//! The match-vision analysis passes (spec D1): one source video read for what
//! it sounds like and what moves in it, so core's pure rules have a number
//! series to work on.
//!
//! One job, **two passes, no seeking**. The [`audio`] pass streams the file
//! at 16 kHz mono through the export's own [`Reader`](crate::composite::audio)
//! and hands core the samples; the motion pass reads it again for a 5 fps
//! thumbnail difference. Two passes rather than one pipeline with two sinks:
//! a sink nobody drains stalls the graph, and the audio pass costs seconds
//! against the motion pass's minutes.
//!
//! **This stores nothing and suggests nothing.** P3 is the measurement phase:
//! the passes exist so the `#[ignore]`d ground-truth run can grade core's
//! rules against the coach's own tags. The commands, the suggestions and the
//! project-format change are P4's.

pub mod audio;
pub mod motion;

/// Why an analysis pass produced nothing. The composite's error under
/// analysis's name, as [`TranscribeError`](crate::TranscribeError) is under
/// transcription's.
pub type AnalyzeError = crate::composite::CompositeError;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use video_coach_core::motion::Thumbnail;
use video_coach_core::signals::{cheer_excess, whistles, Whistle};

use crate::job::{self, JobMessage};

/// How much of an [`Analyzer`]'s progress the sound is: the percentage the
/// job reports once the audio pass is done and the picture has yet to start.
///
/// Measured on the design footage, the sound of a half costs about 27 s
/// against the picture's minutes, so a tenth is roughly where it lands and
/// exactly where it does not matter — this is the shape of a progress bar,
/// not a result.
const AUDIO_SHARE: u8 = 10;

/// Everything one source's analysis found, for a rule to read.
///
/// **The series, not the events.** `cheers`, `still_intervals` and everything
/// built on them are pure functions of these at a threshold, and P3's whole
/// job is to sweep those thresholds: keeping the series means a grid of
/// constants costs one analysis rather than one each (spec G2).
#[derive(Debug, Clone, Default)]
pub struct Signals {
    /// Every tonal event in the whistle band.
    pub whistles: Vec<Whistle>,
    /// The cheer band over its own rolling median, in dB — what
    /// [`cheers_from`](video_coach_core::signals::cheers_from) reads.
    pub cheer_excess: Vec<f32>,
    /// The picture's mean absolute luma difference, at
    /// [`MOTION_HZ`](video_coach_core::motion::MOTION_HZ) — what
    /// [`still_intervals`](video_coach_core::motion::still_intervals) reads.
    pub motion: Vec<f32>,
    /// One thumbnail a second, for the picture rules.
    pub thumbnails: Vec<Thumbnail>,
    /// How much sound there was, in seconds.
    pub audio_seconds: f64,
    /// How much picture there was, in seconds.
    pub video_seconds: f64,
}

/// What an analysis says while it runs.
#[derive(Debug, Clone)]
pub enum AnalyzeMessage {
    /// How far through the source it is, 0–100.
    Progress(u8),
    /// Sent exactly once, last.
    Finished(Result<Signals, AnalyzeError>),
}

impl JobMessage for AnalyzeMessage {
    fn panicked(message: String) -> AnalyzeMessage {
        AnalyzeMessage::Finished(Err(AnalyzeError::Failed(format!(
            "the analysis stopped unexpectedly: {message}"
        ))))
    }
}

/// A running analysis: one source read for its sound, then for its picture.
/// Dropping it cancels the run and **does not wait for it**, as
/// [`Transcriber`](crate::Transcriber) doesn't.
///
/// Unlike a whisper cancel, this one is cheap: both passes look at the flag
/// between buffers, so a cancelled analysis is gone within about a frame
/// (spec B1) and a recording never waits on it.
pub struct Analyzer {
    cancel: Arc<AtomicBool>,
}

impl Analyzer {
    /// Analyses `source`. `on_message` is called on the analysis thread:
    /// [`AnalyzeMessage::Progress`] as it goes, then exactly one
    /// [`AnalyzeMessage::Finished`].
    ///
    /// **Two passes in one job, sound first.** The sound costs seconds
    /// against the picture's minutes, and a rule that reads both wants them
    /// from the same source anyway; one pipeline with two sinks stalls as
    /// soon as one of them isn't drained (spec D1).
    pub fn start(
        source: PathBuf,
        on_message: impl FnMut(AnalyzeMessage) + Send + 'static,
    ) -> Analyzer {
        let cancel = Arc::new(AtomicBool::new(false));
        job::spawn("analyze", on_message, {
            let cancel = cancel.clone();
            move |send| AnalyzeMessage::Finished(analyse(&source, &cancel, send))
        });
        Analyzer { cancel }
    }

    /// Asks the analysis to stop, and returns at once.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Analyzer {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn analyse(
    source: &Path,
    cancel: &AtomicBool,
    on_message: &mut dyn FnMut(AnalyzeMessage),
) -> Result<Signals, AnalyzeError> {
    on_message(AnalyzeMessage::Progress(0));
    let samples = audio::samples(source, cancel)?;
    on_message(AnalyzeMessage::Progress(AUDIO_SHARE));
    let whistles = whistles(&samples);
    let cheer_excess = cheer_excess(&samples);
    let audio_seconds =
        samples.len() as f64 / f64::from(video_coach_core::signals::SIGNAL_SAMPLE_RATE);
    drop(samples);

    let picture = motion::series_reporting(source, cancel, &mut |percent| {
        let share = u16::from(percent) * u16::from(100 - AUDIO_SHARE) / 100;
        on_message(AnalyzeMessage::Progress(AUDIO_SHARE + share as u8));
    })?;
    Ok(Signals {
        whistles,
        cheer_excess,
        motion: picture.motion,
        thumbnails: picture.thumbnails,
        audio_seconds,
        video_seconds: picture.seconds,
    })
}
