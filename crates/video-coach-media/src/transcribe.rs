//! The sound a transcript is made of (spec S2): one commentary recording,
//! decoded whole to the 16 kHz mono whisper.cpp takes.
//!
//! ```text
//! filesrc ! decodebin3 (the audio stream only)
//!         ! audioconvert ! audioresample ! appsink F32LE/16k/1ch
//! ```
//!
//! That is the export's own [`Reader`], asked for different caps. The reuse is
//! what makes a recording with no audio track *fail* rather than hang:
//! `decodebin3` never posts `no-more-pads` here, so the missing track is
//! recognised from its stream collection and nowhere else.
//!
//! **The recording only,** never the source video: the coach's words are the
//! whole of what is transcribed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::composite::audio::Reader;
use crate::composite::CompositeError;

/// `WHISPER_SAMPLE_RATE`: the only rate whisper.cpp takes — `whisper_full`
/// has no rate argument and does not resample, so the pipeline does.
pub const TRANSCRIBE_SAMPLE_RATE: u32 = 16_000;

/// Whisper takes one channel.
const TRANSCRIBE_CHANNELS: usize = 1;

/// How long the test transcriber sleeps between looks at the cancel flag.
const TEST_TICK: Duration = Duration::from_millis(5);

/// Why a transcription produced no words. The composite's error under
/// transcription's name, as [`ExportError`](crate::ExportError) is under
/// export's.
pub type TranscribeError = CompositeError;

/// Where a transcript's words come from (spec S8).
///
/// The same seam [`CaptureSources`](crate::CaptureSources) draws for the
/// recorder: one always-compiled enum, resolved at `Bus::spawn`, so the queue
/// is tested on CI with no model and no whisper build.
///
/// It takes **PCM**, not a path: reading the sound and recognising the words
/// are different failures the coach is told apart, and the shape is the one
/// GStreamer 1.28's `whispertranscriber` takes if this ever moves onto it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscribeKind {
    /// whisper.cpp reading the model at this path.
    Whisper { model: PathBuf },
    /// `text` after `delay`, with the cancel flag polled throughout. For
    /// tests: no model, no whisper build, no GPU.
    Test { delay: Duration, text: String },
}

impl TranscribeKind {
    /// The words in `samples` (16 kHz mono, as [`read_all`] returns them).
    ///
    /// Called on the [`Transcriber`]'s thread. `progress` reports whole
    /// percents, and `cancel` is polled throughout: a cancel is
    /// [`TranscribeError::Cancelled`] and never a failure, so the clip goes
    /// back to idle rather than wearing a message about a return code.
    ///
    /// The samples go unread until the whisper arm is built: the test one
    /// answers from `text`, and the sound it was handed only has to have been
    /// readable.
    fn run(
        &self,
        _samples: &[f32],
        progress: &mut dyn FnMut(u8),
        cancel: &AtomicBool,
    ) -> Result<String, TranscribeError> {
        match self {
            // Task 4's: the model path is already carried here, and the whole
            // of what is missing is this arm.
            TranscribeKind::Whisper { model } => Err(TranscribeError::Failed(format!(
                "this build has no speech recognition in it yet, so the model at {} went unread",
                model.display()
            ))),
            TranscribeKind::Test { delay, text } => test_run(*delay, text, progress, cancel),
        }
    }
}

/// The test transcriber: `delay` spent watching the cancel flag, one progress
/// report half-way through it, then the canned text.
///
/// The delay is the whole point — it is what lets a test queue a clip behind
/// a running job, or preempt one with a recording, without a sleep in the
/// test itself.
fn test_run(
    delay: Duration,
    text: &str,
    progress: &mut dyn FnMut(u8),
    cancel: &AtomicBool,
) -> Result<String, TranscribeError> {
    let started = Instant::now();
    let mut reported = false;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(TranscribeError::Cancelled);
        }
        let Some(left) = delay.checked_sub(started.elapsed()) else {
            return Ok(text.to_owned());
        };
        if !reported && left * 2 <= delay {
            reported = true;
            progress(50);
        }
        std::thread::sleep(TEST_TICK.min(left));
    }
}

/// What a running transcription reports, on its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscribeMessage {
    /// How far along, in whole percent.
    ///
    /// **A floor, not the whole story:** whisper counts its 30-second chunks
    /// and never reaches 100, so a clip shorter than one chunk reports 0 for
    /// its whole run. The UI shows an elapsed clock beside this.
    Progress(u8),
    /// Sent exactly once, last.
    Finished(Result<String, TranscribeError>),
}

/// A running transcription: the sound of one recording read, then recognised.
/// It owns its thread, and dropping it cancels and joins, as
/// [`Exporter`](crate::Exporter) does.
///
/// **One thread per job, not one worker for the queue.** A worker holding its
/// whisper context between jobs would save re-reading the model, and keep its
/// hundreds of megabytes resident through a whole recording session; the
/// queue lives on the bus, where it is a `VecDeque` and nothing else.
pub struct Transcriber {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Transcriber {
    /// Transcribes `recording` with `kind`.
    ///
    /// `on_message` is called on the transcription thread:
    /// [`TranscribeMessage::Progress`] as the percent moves, then exactly one
    /// [`TranscribeMessage::Finished`]. The sound is read **here**, not on
    /// the bus: a minute of commentary decodes in about a second, and the
    /// event loop has frames to deliver.
    pub fn start(
        recording: PathBuf,
        kind: TranscribeKind,
        mut on_message: impl FnMut(TranscribeMessage) + Send + 'static,
    ) -> Transcriber {
        let cancel = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("transcribe".into())
            .spawn({
                let cancel = cancel.clone();
                move || {
                    let result = transcribe(&recording, &kind, &cancel, &mut on_message);
                    on_message(TranscribeMessage::Finished(result));
                }
            })
            .expect("spawn the transcription thread");
        Transcriber {
            cancel,
            thread: Some(thread),
        }
    }

    /// Asks the transcription to stop. It finishes with
    /// [`TranscribeError::Cancelled`] — unless it had already finished, in
    /// which case its own result stands and the words are kept.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read the sound, then recognise it.
fn transcribe(
    recording: &Path,
    kind: &TranscribeKind,
    cancel: &AtomicBool,
    on_message: &mut impl FnMut(TranscribeMessage),
) -> Result<String, TranscribeError> {
    let samples = read_all(recording, cancel)?;
    kind.run(
        &samples,
        &mut |percent| on_message(TranscribeMessage::Progress(percent)),
        cancel,
    )
}

/// All of `path`'s sound as 16 kHz mono samples in [-1, 1].
///
/// **On a worker thread, never the bus:** a minute of commentary decodes in
/// about a second, and the event loop has frames to deliver.
///
/// A file with no audio track and a file that cannot be read are two different
/// errors, and both are errors: export folds them into silence and runs on,
/// but an empty transcript is how a clip says it has never been transcribed
/// (spec S4), so a swallowed failure here would be invisible.
fn read_all(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>, CompositeError> {
    match Reader::start(path, TRANSCRIBE_SAMPLE_RATE, TRANSCRIBE_CHANNELS, cancel) {
        Ok(Some(mut reader)) => reader.rest(cancel),
        Ok(None) => Err(CompositeError::Failed(format!(
            "{} has no sound to transcribe",
            path.display()
        ))),
        Err(CompositeError::Failed(e)) => Err(CompositeError::Failed(format!(
            "could not read the sound of {}: {e}",
            path.display()
        ))),
        Err(cancelled) => Err(cancelled),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use gstreamer as gst;

    use super::*;
    use crate::fixtures;

    /// A flag nothing sets: the uncancelled case.
    fn running() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn dir() -> tempfile::TempDir {
        gst::init().unwrap();
        tempfile::tempdir().unwrap()
    }

    /// Samples `seconds` of audio should come back as, and how far off the
    /// count may be.
    ///
    /// **A tolerance, not a figure:** `audioresample`'s filter has a latency
    /// and a tail, so the exact count is a property of the plugin version. The
    /// assertion is about the rate and the channel count — stereo would be
    /// twice this and 48 kHz three times — not about sample accounting.
    fn expected(seconds: f64) -> (usize, usize) {
        (
            (seconds * f64::from(TRANSCRIBE_SAMPLE_RATE)) as usize,
            1_600,
        )
    }

    #[test]
    fn a_recording_reads_back_as_sixteen_kilohertz_mono() {
        let dir = dir();
        // 2 s of 44.1 kHz mono, which is neither the rate nor the channel
        // count whisper takes.
        let path = fixtures::webm(dir.path(), "commentary.webm", 2, 160, 90, 25, 25);
        let samples = read_all(&path, &running()).expect("the recording has sound");
        let (want, tolerance) = expected(2.0);
        assert!(
            samples.len().abs_diff(want) <= tolerance,
            "{} samples for 2 s, wanted {want} +/- {tolerance}",
            samples.len()
        );
        assert!(
            samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
            "whisper takes samples in [-1, 1]"
        );
    }

    /// A recording with no audio track is a failure the coach must see, not
    /// silence and not a hang: `decodebin3` posts no `no-more-pads`, so only
    /// the stream collection says the track is missing.
    #[test]
    fn a_file_with_no_audio_track_is_a_failure() {
        let dir = dir();
        let path = fixtures::solid_video(&dir.path().join("mute.webm"), 160, 90, 25, 25, 0, false);
        let error = read_all(&path, &running()).expect_err("no sound to transcribe");
        assert!(
            error.to_string().contains("no sound"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_is_a_different_failure() {
        let dir = dir();
        let path = dir.path().join("damaged.webm");
        std::fs::write(&path, b"this is not a recording").unwrap();
        let error = read_all(&path, &running()).expect_err("nothing to decode");
        assert!(
            error.to_string().contains("could not read the sound"),
            "unexpected message: {error}"
        );
    }

    /// A cancel is an error, never a short buffer: a truncated clip would
    /// transcribe as a whole one, and the coach would be told those were all
    /// the words there were.
    #[test]
    fn a_cancelled_read_is_an_error_not_a_short_buffer() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "commentary.webm", 2, 160, 90, 25, 25);
        // Cancelled after the pipeline is up, so the cancel lands where the
        // samples are read rather than where the file is opened.
        let cancel = AtomicBool::new(false);
        let mut reader = Reader::start(&path, TRANSCRIBE_SAMPLE_RATE, TRANSCRIBE_CHANNELS, &cancel)
            .expect("the recording opens")
            .expect("the recording has sound");
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(reader.rest(&cancel), Err(CompositeError::Cancelled));

        // And where it lands while the file is being opened.
        assert_eq!(read_all(&path, &cancel), Err(CompositeError::Cancelled));
    }

    /// Everything the [`Transcriber`] sent, in order. The channel ends when
    /// the thread does, so this needs no cancel and no sleep.
    fn collect(recording: PathBuf, kind: TranscribeKind) -> Vec<TranscribeMessage> {
        let (tx, rx) = mpsc::channel();
        let transcriber = Transcriber::start(recording, kind, move |msg| {
            let _ = tx.send(msg);
        });
        let messages = rx.iter().collect();
        drop(transcriber);
        messages
    }

    #[test]
    fn a_test_transcription_reports_progress_and_then_its_words() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "commentary.webm", 1, 160, 90, 25, 25);
        let messages = collect(
            path,
            TranscribeKind::Test {
                delay: Duration::from_millis(60),
                text: "nice ball".into(),
            },
        );
        assert_eq!(
            messages,
            [
                TranscribeMessage::Progress(50),
                TranscribeMessage::Finished(Ok("nice ball".into())),
            ]
        );
    }

    /// The sound is read on the transcriber's thread, so a recording that
    /// can't be decoded fails the job rather than the extraction of it.
    #[test]
    fn a_recording_that_cannot_be_read_fails_the_job() {
        let dir = dir();
        let path = dir.path().join("damaged.mkv");
        std::fs::write(&path, b"this is not a recording").unwrap();
        let last = collect(
            path,
            TranscribeKind::Test {
                delay: Duration::ZERO,
                text: "never reached".into(),
            },
        )
        .pop();
        let Some(TranscribeMessage::Finished(Err(TranscribeError::Failed(e)))) = last else {
            panic!("expected a failure, got {last:?}");
        };
        assert!(e.contains("could not read the sound"), "{e}");
    }

    /// A cancel is [`TranscribeError::Cancelled`], never a failure: the clip
    /// goes back to idle, with no message about it (spec S5).
    #[test]
    fn a_cancelled_transcription_says_so() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "commentary.webm", 1, 160, 90, 25, 25);
        let (tx, rx) = mpsc::channel();
        let transcriber = Transcriber::start(
            path,
            TranscribeKind::Test {
                delay: Duration::from_secs(60),
                text: "never said".into(),
            },
            move |msg| {
                let _ = tx.send(msg);
            },
        );
        transcriber.cancel();
        drop(transcriber);
        assert_eq!(
            rx.iter().last(),
            Some(TranscribeMessage::Finished(Err(TranscribeError::Cancelled)))
        );
    }
}
