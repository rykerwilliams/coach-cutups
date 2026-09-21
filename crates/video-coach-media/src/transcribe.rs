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

use std::ffi::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::composite::audio::Reader;
use crate::composite::CompositeError;

/// `WHISPER_SAMPLE_RATE`: the only rate whisper.cpp takes — `whisper_full`
/// has no rate argument and does not resample, so the pipeline does.
pub const TRANSCRIBE_SAMPLE_RATE: u32 = 16_000;

/// Whisper takes one channel.
const TRANSCRIBE_CHANNELS: usize = 1;

/// How long the test transcriber sleeps between looks at the cancel flag.
const TEST_TICK: Duration = Duration::from_millis(5);

/// Where a model that isn't on disk is downloaded from, for the message that
/// says so. The model is **found, never fetched** (spec S3): fetching it is
/// Phase 11's, with the bundling decision.
const MODEL_URL_PREFIX: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

/// `best_of` for the greedy sampler — whisper.cpp's own default for greedy,
/// written down rather than inherited so the closeout's throughput number
/// describes a run somebody can reproduce. At temperature 0 the extra
/// decoders are never sampled; they exist for the temperature fallback.
const GREEDY_BEST_OF: c_int = 5;

/// How often the whisper run's percent and the cancel flag are looked at.
/// Also the worst case a cancel waits before whisper is told about it.
const WHISPER_POLL: Duration = Duration::from_millis(100);

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
    /// The test arm leaves the samples unread and answers from `text`: the
    /// sound it was handed only has to have been readable.
    fn run(
        &self,
        samples: &[f32],
        progress: &mut dyn FnMut(u8),
        cancel: &AtomicBool,
    ) -> Result<String, TranscribeError> {
        match self {
            TranscribeKind::Whisper { model } => whisper_run(model, samples, progress, cancel),
            TranscribeKind::Test { delay, text } => test_run(*delay, text, progress, cancel),
        }
    }
}

/// whisper.cpp recognising `samples` with the model at `model`, and the line
/// the closeout's spike is written from.
///
/// **The percent comes back through an atomic, and a second thread does the
/// recognising.** whisper's callbacks have to be `'static`, so neither of
/// them can hold `progress`; and [`WhisperState::full`](whisper_rs::WhisperState::full)
/// blocks for the whole job, so there is no moment afterwards worth reporting
/// in. The run goes beside this thread, which watches the two atomics.
fn whisper_run(
    model: &Path,
    samples: &[f32],
    progress: &mut dyn FnMut(u8),
    cancel: &AtomicBool,
) -> Result<String, TranscribeError> {
    let name = model.file_name().unwrap_or_default().to_string_lossy();
    if !model.is_file() {
        return Err(TranscribeError::Failed(format!(
            "no speech model at {}: download {MODEL_URL_PREFIX}{name} and save it there",
            model.display(),
        )));
    }
    // `full` refuses an empty buffer, and its refusal reads like a bug in us.
    // A recording with an audio track and no samples in it has no words in
    // it either, which is what an empty transcript and nothing else means.
    if samples.is_empty() {
        return Ok(String::new());
    }

    // whisper.cpp and ggml write their model-loading chatter straight to
    // stderr. `bus: loaded …` is the zero-copy diagnostic read off that same
    // stream, so the C logs go into whisper-rs's hooks instead — which, with
    // no `log` or `tracing` backend enabled, is nowhere. Safe to call every
    // job; only the first one does anything.
    whisper_rs::install_logging_hooks();

    let threads = std::thread::available_parallelism()
        .ok()
        .and_then(|n| c_int::try_from(n.get()).ok())
        // `whisper_full_default_params` would have used `min(4, cores)`,
        // which leaves half of an eight-thread laptop idle for minutes.
        .unwrap_or(4);
    let percent = Arc::new(AtomicU8::new(0));
    // The cancel the abort callback reads. It cannot be the caller's — the
    // callback must be `'static` and the caller's flag is a borrow — so this
    // thread mirrors one into the other, within [`WHISPER_POLL`].
    let abort = Arc::new(AtomicBool::new(false));

    let started = Instant::now();
    let text = std::thread::scope(|scope| {
        let run = scope.spawn(|| recognise(model, samples, threads, cancel, &percent, &abort));
        let mut reported = 0;
        while !run.is_finished() {
            if cancel.load(Ordering::SeqCst) {
                abort.store(true, Ordering::SeqCst);
            }
            let now = percent.load(Ordering::Relaxed);
            if now != reported {
                reported = now;
                progress(now);
            }
            std::thread::sleep(WHISPER_POLL);
        }
        run.join().expect("the speech recogniser thread")
    })?;

    let audio = samples.len() as f64 / f64::from(TRANSCRIBE_SAMPLE_RATE);
    let elapsed = started.elapsed().as_secs_f64();
    // **The model and the thread count are in the line on purpose** (spec
    // S0): a throughput number written down without them describes nothing.
    eprintln!(
        "transcribe: {audio:.1} s of audio in {elapsed:.1} s ({:.2}x), \
         {name}, {threads} threads",
        audio / elapsed,
    );
    Ok(text)
}

/// The whisper run itself, on its own thread: load the model, recognise, and
/// join what came back.
fn recognise(
    model: &Path,
    samples: &[f32],
    threads: c_int,
    cancel: &AtomicBool,
    percent: &Arc<AtomicU8>,
    abort: &Arc<AtomicBool>,
) -> Result<String, TranscribeError> {
    let context = WhisperContext::new_with_params(model, WhisperContextParameters::default())
        .map_err(|e| {
            TranscribeError::Failed(format!(
                "could not load the speech model at {}: {e}",
                model.display()
            ))
        })?;
    let mut state = context.create_state().map_err(|e| {
        TranscribeError::Failed(format!("the speech recogniser would not start: {e}"))
    })?;

    // **Whisper hears words in silence.** Ten seconds of nothing commonly
    // comes back as "Thank you." or "[BLANK_AUDIO]", and that is what a clip
    // recorded with the mic muted will say. `no_speech_thold` and
    // `suppress_nst` are the levers; this phase names the failure and accepts
    // it (spec S1), since a wrong transcript is one selection away from being
    // cleared and a threshold tuned by guesswork is not.
    let mut params = FullParams::new(SamplingStrategy::Greedy {
        best_of: GREEDY_BEST_OF,
    });
    params.set_n_threads(threads);
    // Both of these default to **true**, and both write to stderr.
    params.set_print_progress(false);
    params.set_print_timestamps(false);

    let reporter = percent.clone();
    params.set_progress_callback_safe(move |done: i32| {
        // A floor: whisper counts 30-second chunks, reports at the top of
        // each and so never reaches 100. The inspector shows a clock beside
        // this for exactly that reason.
        reporter.store(done.clamp(0, 100) as u8, Ordering::Relaxed);
    });
    // **Already boxed, and that is the whole point.** whisper-rs 0.16.0's
    // `set_abort_callback_safe` boxes its closure into a
    // `Box<Box<dyn FnMut() -> bool>>` and then installs `trampoline::<F>`
    // with `F` the *concrete closure type*, so the trampoline reinterprets
    // the fat pointer's data half — undefined behaviour for a bare closure.
    // Handing it a trait object makes `F` the boxed type and the cast right
    // (BACKLOG #60). Cancellation is the whole of `Drop`-cancels-and-joins,
    // so this is not a detail to get wrong.
    let stop = abort.clone();
    let abort_callback: Box<dyn FnMut() -> bool> = Box::new(move || stop.load(Ordering::SeqCst));
    params.set_abort_callback_safe(abort_callback);

    let outcome = state.full(params, samples);
    // **Our flag, not the return code.** An abort surfaces as -6, -8 or -9
    // depending on where it caught the run, all of them as
    // `WhisperError::GenericError(n)`; the code says where it stopped, never
    // why. We are the ones who asked it to stop, so we are the ones who know
    // (spec S1): a cancel is `Cancelled` and the clip goes back to idle.
    if cancel.load(Ordering::SeqCst) {
        return Err(TranscribeError::Cancelled);
    }
    outcome.map_err(|e| TranscribeError::Failed(format!("the speech recogniser stopped: {e}")))?;

    // **Concatenated, then trimmed once.** Whisper's BPE tokens carry their
    // own leading space, so every segment already reads " like this": joining
    // with a space would double every boundary. Lossy on purpose — one
    // invalid byte is not worth throwing a whole transcript away.
    let mut text = String::new();
    for segment in state.as_iter() {
        let words = segment.to_str_lossy().map_err(|e| {
            TranscribeError::Failed(format!("the speech recogniser returned no text: {e}"))
        })?;
        text.push_str(&words);
    }
    Ok(text.trim().to_owned())
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

    /// A missing model is a failure the coach can act on: it names the path
    /// it looked at and the URL of the file to put there. Needs no model, and
    /// so is not `#[ignore]`d.
    #[test]
    fn a_missing_model_names_the_path_and_the_url() {
        let dir = dir();
        let model = dir.path().join("ggml-small.en.bin");
        let error = TranscribeKind::Whisper {
            model: model.clone(),
        }
        .run(&[0.0], &mut |_| {}, &running())
        .expect_err("there is no model there");
        let TranscribeError::Failed(message) = error else {
            panic!("expected a failure, got {error:?}");
        };
        assert!(
            message.contains(&model.display().to_string()),
            "the message names no path: {message}"
        );
        assert!(
            message.contains(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"
            ),
            "the message names no URL: {message}"
        );
    }

    /// The model the whisper tests run against.
    ///
    /// They are **`#[ignore]`d, never skipped**: a test that reads an
    /// environment variable and passes when it is unset passes vacuously on
    /// CI forever, which is the failure mode [`fixtures`] exists to avoid.
    /// `CLAUDE.md` carries the command that runs them.
    fn model() -> PathBuf {
        PathBuf::from(
            // The same variable the app reads to find its model (spec S3).
            std::env::var_os("COACH_CUTS_WHISPER_MODEL")
                .expect("COACH_CUTS_WHISPER_MODEL names a ggml whisper model"),
        )
    }

    /// A whole run: the model loads, `full` recognises, the segments join.
    ///
    /// **Nothing is asserted about the words.** The fixture is a tone, and
    /// what whisper hears in a tone is the silence hallucination spec S1
    /// names and accepts. What this pins is that a real run comes back with
    /// text rather than an error — and, under `--nocapture`, it prints the
    /// throughput line the closeout's spike is written from.
    #[test]
    #[ignore = "needs a whisper model in COACH_CUTS_WHISPER_MODEL"]
    fn a_recording_transcribes() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "commentary.webm", 3, 160, 90, 25, 25);
        let samples = read_all(&path, &running()).expect("the recording has sound");
        let text = TranscribeKind::Whisper { model: model() }
            .run(&samples, &mut |_| {}, &running())
            .expect("the model recognises the recording");
        eprintln!("transcribed: {text:?}");
        assert_eq!(text, text.trim(), "the transcript is trimmed once");
    }

    /// A cancelled run is [`TranscribeError::Cancelled`] — **and the test
    /// asserts no return code.** An abort surfaces as -6, -8 or -9 depending
    /// on where it caught the run, so a test that pinned one would be
    /// testing whisper's internals rather than our answer to them.
    #[test]
    #[ignore = "needs a whisper model in COACH_CUTS_WHISPER_MODEL"]
    fn a_cancelled_run_says_cancelled() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "commentary.webm", 3, 160, 90, 25, 25);
        let samples = read_all(&path, &running()).expect("the recording has sound");
        // Set before the run rather than raced against it: the model still
        // loads and `full` still starts, so the abort callback still fires —
        // at its first graph node instead of somewhere unrepeatable.
        let cancel = AtomicBool::new(true);
        let error = TranscribeKind::Whisper { model: model() }
            .run(&samples, &mut |_| {}, &cancel)
            .expect_err("the run was cancelled");
        assert_eq!(error, TranscribeError::Cancelled);
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
