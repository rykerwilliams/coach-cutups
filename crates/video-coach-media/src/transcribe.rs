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
use std::time::{Duration, Instant};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::composite::audio::Reader;
use crate::composite::CompositeError;
use crate::download::{download, Fetch};

/// `WHISPER_SAMPLE_RATE`: the only rate whisper.cpp takes — `whisper_full`
/// has no rate argument and does not resample, so the pipeline does.
pub const TRANSCRIBE_SAMPLE_RATE: u32 = 16_000;

/// Whisper takes one channel.
const TRANSCRIBE_CHANNELS: usize = 1;

/// How long the test transcriber sleeps between looks at the cancel flag.
const TEST_TICK: Duration = Duration::from_millis(5);

/// Where a model that isn't on disk is downloaded from: by the job itself
/// when the app may fetch it (Phase 11 spec S3), and otherwise by the coach,
/// following the message that names it.
///
/// **Pinned to a commit, not `resolve/main/`.** `main` is mutable: an
/// upstream re-upload would fail every download's hash against
/// [`WhisperModel::sha256`], with no recovery short of a release. At this
/// commit the redirect's `x-linked-etag` is the measured sha256.
const MODEL_URL_PREFIX: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/";

/// Which model whisper runs, and so the speed the coach waits at.
///
/// **Two, both English-only, and that is the whole choice.** `small.en` was
/// measured at **0.73× realtime** on the reference laptop (65 s of audio in
/// 89.2 s, 8 threads, on AC), which is slow enough that the trade is real;
/// `base.en` is the faster, less accurate half of it. Nothing above `medium`
/// has an `.en` variant, and none of them would be worth the wait here.
///
/// The names live **beside [`MODEL_URL_PREFIX`]**, rather than beside the
/// path built from them: a name suggested for download and a name that isn't
/// must not drift apart, and [`WhisperModel::from_file_name`] is the one
/// place that answers "is this file ours".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WhisperModel {
    /// 148 MB.
    Base,
    /// 488 MB.
    #[default]
    Small,
}

impl WhisperModel {
    /// Both, in the order the picker offers them: fastest first.
    pub const ALL: [WhisperModel; 2] = [WhisperModel::Base, WhisperModel::Small];

    /// The file name under the models directory — which is also the name
    /// [`MODEL_URL_PREFIX`] resolves.
    ///
    /// **Never a quantization suffix:** tiny/base/small ship `q5_1` and
    /// medium/large `q5_0`, so a suffix written here would be wrong the first
    /// time the list grows.
    pub const fn file_name(self) -> &'static str {
        match self {
            WhisperModel::Base => "ggml-base.en.bin",
            WhisperModel::Small => "ggml-small.en.bin",
        }
    }

    /// What the picker shows, and what `state.json` remembers.
    pub const fn label(self) -> &'static str {
        match self {
            WhisperModel::Base => "base.en",
            WhisperModel::Small => "small.en",
        }
    }

    /// The sha256 of [`WhisperModel::file_name`] as published, **measured on
    /// a downloaded copy, not read off a web page.**
    ///
    /// What a download is checked against before it is renamed into place
    /// (Phase 11 spec S3) — one **per model**, since the coach picks which.
    pub const fn sha256(self) -> &'static str {
        match self {
            WhisperModel::Base => {
                "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
            }
            WhisperModel::Small => {
                "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"
            }
        }
    }

    /// The file's length in bytes, measured beside [`WhisperModel::sha256`]:
    /// the size the Transcribe button offers to download, the download's
    /// progress denominator, and its first check.
    pub const fn bytes(self) -> u64 {
        match self {
            WhisperModel::Base => 147_964_211,
            WhisperModel::Small => 487_614_201,
        }
    }

    /// Where [`WhisperModel::file_name`] is published, at the pinned commit.
    pub fn url(self) -> String {
        format!("{MODEL_URL_PREFIX}{}", self.file_name())
    }

    /// How to download this model and know it arrived whole.
    pub fn fetch(self) -> Fetch {
        Fetch {
            url: self.url(),
            sha256: self.sha256().into(),
            bytes: self.bytes(),
        }
    }

    /// The model `name` is the file name of, if it is one we ship — which is
    /// the same question as "does [`MODEL_URL_PREFIX`] resolve it".
    pub fn from_file_name(name: &str) -> Option<WhisperModel> {
        WhisperModel::ALL
            .into_iter()
            .find(|m| m.file_name() == name)
    }

    /// The model [`WhisperModel::label`] names, for what `state.json` last
    /// remembered. `None` for a label this version doesn't know, which is how
    /// a choice made by a later version reads.
    pub fn from_label(label: &str) -> Option<WhisperModel> {
        WhisperModel::ALL.into_iter().find(|m| m.label() == label)
    }
}

/// `best_of` for the greedy sampler — whisper.cpp's own default for greedy,
/// written down rather than inherited so the closeout's throughput number
/// describes a run somebody can reproduce. At temperature 0 the extra
/// decoders are never sampled; they exist for the temperature fallback.
const GREEDY_BEST_OF: c_int = 5;

/// How often the whisper run's percent is looked at. Nothing else waits on
/// it: the abort callback reads the caller's own flag.
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
    /// whisper.cpp reading the model at `model` — downloaded there first, if
    /// it is absent and `fetch` says from where.
    ///
    /// **`fetch` is the permission, and the path never implies it.** This is
    /// the one place that reasoning lives; everything else points here. The
    /// app sets it only for a model under its own cache directory — never
    /// under `$COACH_CUTS_WHISPER_MODEL`, a file in a directory that is the
    /// coach's, however it is named — and tests pointing at a missing file
    /// carrying our own name pass `None`, so that CI never touches Hugging
    /// Face.
    Whisper {
        model: PathBuf,
        fetch: Option<Fetch>,
    },
    /// `text` after `delay`, with the cancel flag polled throughout. For
    /// tests: no model, no whisper build, no GPU.
    Test { delay: Duration, text: String },
}

impl TranscribeKind {
    /// What a job of this kind would download before it runs: the `fetch`,
    /// when there is one and the model isn't on disk yet.
    ///
    /// The job asks this to decide, and the Transcribe button asks it to
    /// offer the download before the coach presses it — so the two can't
    /// disagree.
    pub fn will_download(&self) -> Option<&Fetch> {
        match self {
            TranscribeKind::Whisper {
                model,
                fetch: Some(fetch),
            } if !model.is_file() => Some(fetch),
            _ => None,
        }
    }

    /// The words in `samples` (16 kHz mono, as [`read_all`] returns them).
    ///
    /// Called on the [`Transcriber`]'s thread. `progress` reports whole
    /// percents, and `cancel` is polled throughout: a cancel is
    /// [`TranscribeError::Cancelled`] and never a failure, so the clip goes
    /// back to idle rather than wearing a message about a return code.
    ///
    /// `cancel` is an `&Arc` and not an `&AtomicBool` so that whisper's
    /// `'static` abort callback can hold *the caller's own flag* rather than
    /// a mirror something has to copy into. Everything that wants the plain
    /// reference still gets it by deref.
    ///
    /// The test arm leaves the samples unread and answers from `text`: the
    /// sound it was handed only has to have been readable.
    fn run(
        &self,
        samples: &[f32],
        progress: &mut dyn FnMut(u8),
        cancel: &Arc<AtomicBool>,
    ) -> Result<String, TranscribeError> {
        match self {
            TranscribeKind::Whisper { model, .. } => whisper_run(model, samples, progress, cancel),
            TranscribeKind::Test { delay, text } => test_run(*delay, text, progress, cancel),
        }
    }
}

/// whisper.cpp recognising `samples` with the model at `model`, and the line
/// the closeout's spike is written from.
///
/// **The percent comes back through an atomic, and a second thread does the
/// recognising.** whisper's progress callback has to be `'static`, so it
/// cannot hold `progress`; and [`WhisperState::full`](whisper_rs::WhisperState::full)
/// blocks for the whole job, so there is no moment afterwards worth reporting
/// in. The run goes beside this thread, which watches the atomic. (The
/// *abort* callback needs no such dance — `cancel` is already an `Arc`, so it
/// clones it.)
fn whisper_run(
    model: &Path,
    samples: &[f32],
    progress: &mut dyn FnMut(u8),
    cancel: &Arc<AtomicBool>,
) -> Result<String, TranscribeError> {
    let name = model.file_name().unwrap_or_default().to_string_lossy();
    if !model.is_file() {
        // Only reached when the job may not fetch it (see
        // [`TranscribeKind::Whisper`]), so the coach is told where to get it
        // by hand.
        //
        // **Only a model we ship has a URL.** The path is an escape hatch
        // (`$COACH_CUTS_WHISPER_MODEL`), so interpolating whatever file name
        // it ends in would hand the coach a fabricated Hugging Face URL — a
        // 404 for a typo, a bare directory listing for a folder — and send
        // them looking for a download instead of at their own path.
        return Err(TranscribeError::Failed(
            match WhisperModel::from_file_name(&name) {
                Some(ours) => format!(
                    "no speech model at {}: download {} and save it there",
                    model.display(),
                    ours.url(),
                ),
                None => format!("no speech model at {}", model.display()),
            },
        ));
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

    let started = Instant::now();
    let text = std::thread::scope(|scope| {
        let run = scope.spawn(|| recognise(model, samples, threads, cancel, &percent));
        let mut reported = 0;
        while !run.is_finished() {
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
    cancel: &Arc<AtomicBool>,
    percent: &Arc<AtomicU8>,
) -> Result<String, TranscribeError> {
    let context = WhisperContext::new_with_params(model, WhisperContextParameters::default())
        .map_err(|e| {
            // **The size is the diagnosis.** whisper-rs turns every loading
            // failure into a bare `InitError`, and whisper.cpp's own reason
            // for it — the magic, the version, which tensor ran off the end
            // of the file — goes to the logging hooks installed above and
            // therefore nowhere at all. The one fact still worth having is
            // how big the file the coach has actually is: a download that
            // stopped early is self-evident beside the 148 MB (`base.en`) or
            // 488 MB (`small.en`) a whole one prints. The downloader checks
            // what it fetches, so this is a file that came from somewhere
            // else: `$COACH_CUTS_WHISPER_MODEL`, or a copy put there by hand.
            TranscribeError::Failed(format!(
                "could not load the speech model at {} ({}): {e}",
                model.display(),
                file_size(model),
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
    // **Belt and braces, and not the mechanism.** Both default to `true`,
    // but in whisper.cpp 1.8.3 neither reaches stderr on its own:
    // `print_progress` is read by the CLI examples and never by the library,
    // and `print_timestamps` is read only inside `if (params.print_realtime)`,
    // which is off. `install_logging_hooks` above is what actually keeps the
    // 37-line model dump off the stream `bus: loaded …` is read from — do not
    // drop it on the strength of these two lines.
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
    // (BACKLOG #60). Cancellation is the whole of what `Drop` can do about a
    // run, so this is not a detail to get wrong.
    //
    // **And it is consulted twice a job, not per graph node.** whisper.cpp's
    // encoder and decoder both go through the `ggml_backend_sched_t` overload
    // of `ggml_graph_compute_helper`, which never calls
    // `ggml_backend_set_abort_callback`; only the per-node overload does, and
    // nothing on this path uses it. Measured on the reference laptop with
    // `small.en`: a run whose flag was set before `full` even started still
    // took 31 s to return, against 29 s for the same run left alone — one
    // 30-second chunk is one encode, and the abort is not looked at inside
    // it. That is why nothing waits on a cancel — see [`Transcriber`].
    let stop = cancel.clone();
    let abort_callback: Box<dyn FnMut() -> bool> = Box::new(move || stop.load(Ordering::SeqCst));
    params.set_abort_callback_safe(abort_callback);

    let outcome = state.full(params, samples);
    // **Our flag says *why*; the return code says *whether*.** An abort
    // surfaces as -6, -8 or -9 depending on where it caught the run, all of
    // them as `WhisperError::GenericError(n)`: the code can say the run
    // stopped, never that we are the ones who stopped it (spec S1). So a
    // refusal with our flag set is `Cancelled`, and the clip goes back to
    // idle rather than wearing a message about a return code.
    //
    // A run that *finished* keeps its words even though the cancel arrived —
    // the same trade the bus makes for a `Finished` that beat the cancel, and
    // the same one export makes when a cancel loses the race to a written
    // file. It also makes the cancel testable: with the flag set before the
    // run, `Cancelled` can only come back if the abort callback really
    // reached whisper.
    if cancel.load(Ordering::SeqCst) && outcome.is_err() {
        return Err(TranscribeError::Cancelled);
    }
    outcome.map_err(|e| TranscribeError::Failed(format!("the speech recogniser stopped: {e}")))?;

    // Lossy on purpose — one invalid byte is not worth throwing a whole
    // transcript away.
    let mut segments = Vec::new();
    for segment in state.as_iter() {
        let words = segment.to_str_lossy().map_err(|e| {
            TranscribeError::Failed(format!("the speech recogniser returned no text: {e}"))
        })?;
        segments.push(words.into_owned());
    }
    Ok(join_segments(&segments))
}

/// The transcript whisper's segments make: **concatenated, then trimmed
/// once.** Whisper's BPE tokens carry their own leading space, so every
/// segment already reads `" like this"`; joining with a space would double
/// every boundary and joining without trimming would leave the first one.
///
/// Its own function because it is the only subtle thing in this file that a
/// test can reach without a 488 MB model — the whole-run tests are
/// `#[ignore]`d, and a rule this easy to get backwards should not be pinned
/// only by a test nobody runs on CI.
fn join_segments<S: AsRef<str>>(segments: impl IntoIterator<Item = S>) -> String {
    let mut text = String::new();
    for segment in segments {
        text.push_str(segment.as_ref());
    }
    text.trim().to_owned()
}

/// How big the file at `path` is, in the form an error message wants it.
fn file_size(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) => format!("{:.1} MB", meta.len() as f64 / 1e6),
        Err(e) => format!("its size is unreadable: {e}"),
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
    /// The model is being downloaded, this far along in whole percent — the
    /// job's first step when the model is absent and the job may fetch it.
    /// A [`TranscribeMessage::Progress`] follows the last one, so the
    /// download is over when recognising begins, even if whisper never
    /// reports a percent of its own.
    Downloading(u8),
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
/// Dropping it cancels the run — and, unlike [`Exporter`](crate::Exporter),
/// **does not wait for it**.
///
/// **Nothing may block on a whisper cancel.** whisper.cpp looks at the abort
/// callback once per encode and once per decode pass (see `recognise`), so a
/// cancelled `small.en` run was measured returning 12.4 s later — and, on a
/// recording short enough to be a single 30-second chunk, no sooner than an
/// uncancelled one at all. Every cancel comes from the bus thread: a record
/// starting, a clip going to the trash, a project opening, a quit. A bus
/// thread parked for that long is a dead Stop Recording button, a frozen
/// deadline and a quit that holds the UI's GL teardown. So the thread is let
/// go instead: it holds nothing but its own model and samples, its last
/// message is tagged with a generation the bus has already moved past, and
/// the exiting process reclaims it. The cost is real and accepted: for those
/// seconds the abandoned run is still on the CPU, and a quit during one ends
/// the process with it still computing.
///
/// **One thread per job, not one worker for the queue.** A worker holding its
/// whisper context between jobs would save re-reading the model, and keep its
/// hundreds of megabytes resident through a whole recording session; the
/// queue lives on the bus, where it is a `VecDeque` and nothing else.
pub struct Transcriber {
    cancel: Arc<AtomicBool>,
}

impl Transcriber {
    /// Transcribes `recording` with `kind`.
    ///
    /// `on_message` is called on the transcription thread:
    /// [`TranscribeMessage::Downloading`] while the model downloads, if it
    /// must, then [`TranscribeMessage::Progress`] as the percent moves, then
    /// exactly one [`TranscribeMessage::Finished`]. The sound is read **here**, not on
    /// the bus: a minute of commentary decodes in about a second, and the
    /// event loop has frames to deliver.
    pub fn start(
        recording: PathBuf,
        kind: TranscribeKind,
        mut on_message: impl FnMut(TranscribeMessage) + Send + 'static,
    ) -> Transcriber {
        let cancel = Arc::new(AtomicBool::new(false));
        // The handle is dropped on purpose: nothing here ever joins (see the
        // type's own docs), and the last `Finished` is what says the job is
        // over, on every path including a cancelled one.
        std::thread::Builder::new()
            .name("transcribe".into())
            .spawn({
                let cancel = cancel.clone();
                move || {
                    let result = transcribe(&recording, &kind, &cancel, &mut on_message);
                    on_message(TranscribeMessage::Finished(result));
                }
            })
            .expect("spawn the transcription thread");
        Transcriber { cancel }
    }

    /// Asks the transcription to stop, and **returns at once**. It finishes
    /// with [`TranscribeError::Cancelled`] whenever it gets round to noticing
    /// — unless it had already finished, in which case its own result stands
    /// and the words are kept.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Fetch the model if the job may and must, read the sound, then recognise
/// it.
///
/// **The download is the job's first step** (Phase 11 spec S3), not a
/// mechanism of its own, so it inherits the cancel, the one-at-a-time and
/// `Failed` for free — and a recording that preempts the job preempts the
/// download with it, which restarts from zero. First rather than after the
/// read, so the coach sees "Downloading" at once instead of a second of
/// "Transcribing" before it.
fn transcribe(
    recording: &Path,
    kind: &TranscribeKind,
    cancel: &Arc<AtomicBool>,
    on_message: &mut impl FnMut(TranscribeMessage),
) -> Result<String, TranscribeError> {
    if let (TranscribeKind::Whisper { model, .. }, Some(fetch)) = (kind, kind.will_download()) {
        download(
            fetch,
            model,
            &mut |percent| on_message(TranscribeMessage::Downloading(percent)),
            cancel,
        )?;
        // Whisper reports nothing until its percent moves off zero, which on
        // a clip shorter than one chunk is never: this is what says the
        // download is over.
        on_message(TranscribeMessage::Progress(0));
    }
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

    /// A flag nothing sets: the uncancelled case. An `Arc`, because that is
    /// what a run takes; `read_all` and `Reader` take the `&AtomicBool` out
    /// of it by deref.
    fn running() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
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

    /// **Whisper's own spacing is already right.** Every segment arrives with
    /// its leading space, so the join is a concatenation and the trim happens
    /// once, at the end. Space-joining (which macOS did) doubles every
    /// boundary; trimming each segment welds the last word of one to the
    /// first of the next.
    ///
    /// Needs no model, which is the point of [`join_segments`] being its own
    /// function: this rule is the only subtle thing in the file, and it now
    /// has a test that runs on CI.
    #[test]
    fn segments_are_concatenated_and_trimmed_once() {
        assert_eq!(join_segments([" nice", " ball"]), "nice ball");
        assert_eq!(join_segments([" Thank you.", " Bye."]), "Thank you. Bye.");
        // One segment, and none at all.
        assert_eq!(join_segments([" over the top"]), "over the top");
        assert_eq!(join_segments([] as [&str; 0]), "");
        // Trailing whitespace goes with the leading kind, and only there:
        // whisper's spaces *inside* the transcript are its own.
        assert_eq!(join_segments([" a", "  b ", " "]), "a  b");
    }

    /// A missing model is a failure the coach can act on: it names the path
    /// it looked at and the URL of the file to put there. Needs no model, and
    /// so is not `#[ignore]`d.
    ///
    /// **Both models we offer count as ours**, which is what makes the picker
    /// safe: choosing the one that isn't downloaded yet has to land on this
    /// message and not on a bare path.
    #[test]
    fn a_missing_model_names_the_path_and_the_url() {
        let dir = dir();
        for model in WhisperModel::ALL {
            let path = dir.path().join(model.file_name());
            let error = TranscribeKind::Whisper {
                model: path.clone(),
                fetch: None,
            }
            .run(&[0.0], &mut |_| {}, &running())
            .expect_err("there is no model there");
            let TranscribeError::Failed(message) = error else {
                panic!("expected a failure, got {error:?}");
            };
            assert!(
                message.contains(&path.display().to_string()),
                "the message names no path: {message}"
            );
            assert!(
                message.contains(&model.url()),
                "the message names no URL: {message}"
            );
        }
    }

    /// The names are the two halves of one fact — the file to fetch and the
    /// label that stands for it — and nothing else may collide with them.
    #[test]
    fn every_model_round_trips_through_its_names() {
        for model in WhisperModel::ALL {
            assert_eq!(WhisperModel::from_file_name(model.file_name()), Some(model));
            assert_eq!(WhisperModel::from_label(model.label()), Some(model));
            assert_eq!(model.sha256().len(), 64, "{model:?}");
            // At a commit: `main` moves, and every hash would then fail.
            assert!(!model.url().contains("/resolve/main/"), "{model:?}");
        }
        // A file the coach pointed us at, and a choice made by a version that
        // knows more models than this one: neither is ours.
        assert_eq!(WhisperModel::from_file_name("my-tuned-model.bin"), None);
        assert_eq!(WhisperModel::from_label("medium"), None);
    }

    /// And a model path that is *not* the one we ship gets **no** URL: the
    /// path comes from `$COACH_CUTS_WHISPER_MODEL`, so a name pasted into
    /// the Hugging Face URL would send the coach to a 404 for their own typo.
    #[test]
    fn a_missing_model_of_our_own_choosing_is_not_given_a_url() {
        let dir = dir();
        let error = TranscribeKind::Whisper {
            model: dir.path().join("my-tuned-model.bin"),
            fetch: None,
        }
        .run(&[0.0], &mut |_| {}, &running())
        .expect_err("there is no model there");
        let TranscribeError::Failed(message) = error else {
            panic!("expected a failure, got {error:?}");
        };
        assert!(
            message.contains("my-tuned-model.bin") && !message.contains("http"),
            "a URL was invented for it: {message}"
        );
    }

    /// A file that is not a model at all — which is what a download cut off
    /// half way through leaves behind, when it came from anywhere but the
    /// downloader, which checks what it fetches.
    /// whisper-rs reports every loading failure as a bare `InitError`, and
    /// whisper.cpp's own reason for it is in the logging hooks and therefore
    /// nowhere, so **the size is the diagnosis**: 0.0 MB where 488 MB should
    /// be says what no error text here would. Needs no model.
    #[test]
    fn a_model_that_will_not_load_is_a_failure_that_names_its_size() {
        let dir = dir();
        let model = dir.path().join("ggml-small.en.bin");
        std::fs::write(&model, vec![0x5a; 20_000]).unwrap();
        let error = TranscribeKind::Whisper {
            model: model.clone(),
            fetch: None,
        }
        .run(&[0.0], &mut |_| {}, &running())
        .expect_err("that is not a model");
        let TranscribeError::Failed(message) = error else {
            panic!("expected a failure, got {error:?}");
        };
        assert!(
            message.contains(&model.display().to_string()) && message.contains("0.0 MB"),
            "the message does not give the coach the size: {message}"
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
    /// names and accepts. What this pins is that a real run comes back
    /// rather than erroring — the *shape* of what comes back is
    /// [`segments_are_concatenated_and_trimmed_once`]'s, which needs no
    /// model. Under `--nocapture` this also prints the throughput line the
    /// closeout's spike is written from, and that is half its job.
    #[test]
    #[ignore = "needs a whisper model in COACH_CUTS_WHISPER_MODEL"]
    fn a_recording_transcribes() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "commentary.webm", 3, 160, 90, 25, 25);
        let samples = read_all(&path, &running()).expect("the recording has sound");
        let mut percents = Vec::new();
        TranscribeKind::Whisper {
            model: model(),
            fetch: None,
        }
        .run(&samples, &mut |p| percents.push(p), &running())
        .expect("the model recognises the recording");
        eprintln!("transcribed, progress: {percents:?}");
    }

    /// A cancelled run is [`TranscribeError::Cancelled`] — **and the test
    /// asserts no return code.** An abort surfaces as -6, -8 or -9 depending
    /// on where it caught the run, so a test that pinned one would be
    /// testing whisper's internals rather than our answer to them.
    ///
    /// **And it only says so if the abort really fired.** `Cancelled` is
    /// reported for a run that *refused*, never for one that finished, so a
    /// build where the abort callback never reached whisper — the one
    /// unsound-by-default API this phase touches (BACKLOG #60) — comes back
    /// here as `Ok(words)` and fails the test.
    ///
    /// A *clock* would not do it. Whisper pads anything shorter than its
    /// 30-second chunk and looks at the abort once per pass, so aborting this
    /// three-second fixture saves nothing measurable: 31 s cancelled against
    /// 29 s left alone on the reference laptop.
    #[test]
    #[ignore = "needs a whisper model in COACH_CUTS_WHISPER_MODEL"]
    fn a_cancelled_run_says_cancelled() {
        let dir = dir();
        let path = fixtures::webm(dir.path(), "commentary.webm", 3, 160, 90, 25, 25);
        let samples = read_all(&path, &running()).expect("the recording has sound");
        // Set before the run rather than raced against it: the model still
        // loads and `full` still starts, so the abort callback still fires —
        // at the end of the first pass instead of somewhere unrepeatable.
        let cancel = Arc::new(AtomicBool::new(true));
        let outcome = TranscribeKind::Whisper {
            model: model(),
            fetch: None,
        }
        .run(&samples, &mut |_| {}, &cancel);
        assert_eq!(
            outcome,
            Err(TranscribeError::Cancelled),
            "the run was not refused, so the abort callback never reached \
             whisper"
        );
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

    /// **A job that may fetch its absent model downloads it first**, says so
    /// with `Downloading`, and ends the download with a `Progress(0)` before
    /// whisper starts — whisper itself may never report one. The "model" is
    /// the test server's body, so the whisper run then fails to load it,
    /// which is as far as a test without a model can follow it.
    #[test]
    fn an_absent_model_is_downloaded_before_the_run() {
        use crate::fixtures::{serve, served_body, Answer, SERVED_SHA256};

        let dir = dir();
        let recording = fixtures::webm(dir.path(), "commentary.webm", 1, 160, 90, 25, 25);
        let model = dir.path().join("models").join("ggml-test.bin");
        let messages = collect(
            recording,
            TranscribeKind::Whisper {
                model: model.clone(),
                fetch: Some(Fetch {
                    url: serve(Answer::Whole, Duration::ZERO),
                    sha256: SERVED_SHA256.into(),
                    bytes: served_body().len() as u64,
                }),
            },
        );

        assert_eq!(messages.first(), Some(&TranscribeMessage::Downloading(0)));
        let handoff = messages
            .iter()
            .position(|m| *m == TranscribeMessage::Progress(0))
            .unwrap_or_else(|| panic!("the download never said it was over: {messages:?}"));
        assert!(
            messages[..handoff]
                .iter()
                .all(|m| matches!(m, TranscribeMessage::Downloading(_))),
            "{messages:?}"
        );
        let Some(TranscribeMessage::Finished(Err(TranscribeError::Failed(e)))) = messages.last()
        else {
            panic!("expected whisper to refuse the body, got {messages:?}");
        };
        assert!(e.contains("could not load the speech model"), "{e}");
        assert!(
            std::fs::read(&model).unwrap() == served_body(),
            "the model did not land"
        );
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
