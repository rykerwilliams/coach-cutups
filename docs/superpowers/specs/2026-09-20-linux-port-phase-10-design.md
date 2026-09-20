# Linux Port — Phase 10: Transcription

**Date:** 2026-09-20
**Status:** Draft, pre-review. **Two decisions are gated on the spike** (S3's default model, S6's auto-enqueue) — see "The spike, first".
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 10; the locked decision at line 22; open question 3 and risk 6)
**Builds on:** Phase 3 (clip editing and undo), Phase 4 (capture, which writes the audio this reads), Phase 8 (the audio-only decode pipeline this generalizes)
**Evidence:** `apple/App/Intelligence/AppleClipIntelligence.swift`; `apple/VideoCoachCore/Sources/VideoCoachCore/Intelligence/TranscriptionCoordinator.swift`; `apple/App/Models/Workspace.swift:815-832`; `apple/App/Views/ClipInspector.swift:271-360`; `docs/superpowers/specs/2026-05-21-clip-transcript-and-summary-design.md`.

---

## Goal

The coach records commentary over a clip. Afterwards the words are there as text, on the clip, editable, without sending anything to anyone.

## Done when

1. **A clip transcribes.** Stopping a recording queues its clip; the transcript appears when it lands.
2. **The queue is honest.** One job at a time, FIFO behind it, and a clip that is waiting *says* it is waiting.
3. **The model arrives on its own.** First use explains what it's about to download, downloads it with visible progress, verifies it, and caches it for every project afterwards.
4. **The transcript is editable** and survives a reload.
5. **Editing a transcript is undoable; the machine writing one is not.**
6. **It runs with no network** once the model is cached, and the README says something true.

---

## Decisions

### S0. The spike, first

Nothing about whisper has been measured in this project, and there is no spike under `docs/superpowers/spikes/`. This project's rule is "measured, not preferred" — three GStreamer spikes exist because guessing was not good enough — and two decisions below are **gated on one number**: transcription wall-time per minute of commentary audio, CPU-only, on the reference laptop (i7-10610U, 8 threads, 15 W).

Measure `small.en` and `base.en`, with and without the `openmp` feature (`build.rs` disables it by default), on a real commentary recording of a few minutes. Record it as `docs/superpowers/spikes/2026-09-20-whisper-throughput.md`.

**What the number decides:**
- **Slower than ~1× realtime:** `base.en` becomes the default (S3), and auto-enqueue on recording stop is off by default (S6) — a coach who records six clips in a row should not hand the laptop over to a queue that takes longer than the session did.
- **Comfortably faster than realtime:** `small.en` stays the default and auto-enqueue stays on.

**Build requirement, and it is new.** `whisper-rs-sys`'s build-dependencies are `cmake`, `bindgen` and `fs_extra`; it drives whisper.cpp's own CMake build. The workspace already compiles C++ — but through skia-safe, which **downloads prebuilt binaries**, so neither `cmake` nor `libclang` is installed on the reference laptop today. Both must be added to the dev machine, to `.github/workflows/rust.yml`'s `workspace` job, and to Phase 11's packaging story. **Do not repeat the claim that this costs no new build dependency; it does.**

### S1. It lives in `video-coach-media`, behind `feature = "whisper"`

Forced, and confirmed three ways: `video-coach-core` declares no media dependency (its `Cargo.toml`, a dedicated CI job on a runner with no GStreamer, and the `verify` skill's exact four-dependency audit), and the job needs GStreamer to decode the recording anyway. The feature follows the existing `feature = "fixtures"` pattern.

`whisper-rs 0.16.0` (Unlicense, MSRV 1.88, vendoring whisper.cpp 1.8.3). `default = []` is a plain CPU build with no GPU SDK — keep it that way; a GPU feature is a packaging problem for a laptop that has no discrete GPU.

**The API changed under the ecosystem's feet.** `full_n_segments`, `full_get_segment_t0/t1` and `full_get_segment_text` were removed in 0.15.x. Current shape is `get_segment(i) -> Option<WhisperSegment>` / `as_iter()`, with `start_timestamp()`/`end_timestamp()`/`to_str()`, and `full()` returning `Result<(), WhisperError>`. Nearly every tutorial online predates this. **Read the 0.16 docs, not a blog post.**

**Timestamps are centiseconds** (divide by 100.0). Named here because it is exactly the kind of detail that ships a silent 100× error — though see S4: Phase 10 does not store them.

### S2. Audio extraction generalizes `composite::Reader`

whisper.cpp requires **16 kHz mono f32 in [-1, 1]** and does not resample internally (`whisper_full` takes no rate argument). The recording is 48 kHz stereo Opus in Matroska.

`crates/video-coach-media/src/composite/audio.rs`'s `Reader` is already `filesrc ! decodebin3 (audio only) ! audioconvert ! audioresample ! appsink`, and it already solves the hard parts: detecting a missing audio track from the `StreamCollection` rather than prerolling (`decodebin3` never posts `no-more-pads` here — measured), selecting only the audio stream (an unselected video stream still decodes every frame: 2.2 s vs 46 ms for 10 s of 1080p), cancellation, and zero-padding past EOF.

**Only its caps are wrong.** `caps_description()` hardcodes F32LE/48000/2ch, and `AUDIO_SAMPLE_RATE` is a core `const` with a compile-time assertion hanging off it, because it is the export mix rate. So:

- **Parameterize `Reader` with its own rate and channel count** rather than touching `AUDIO_SAMPLE_RATE`. `audioconvert` and `audioresample` are already in the chain, so mono at 16 kHz is a caps change, not new elements.
- **Promote it out of `composite`.** It is `mod audio;` private today, with `Reader` private and `Mixer` `pub(super)`. Transcription is a second consumer, so the reader becomes a `video-coach-media` helper. Its plumbing dependencies (`POLL`, `QUEUED`, `Stopper`, `Watch`, `CompositeError`) move or are shared with it.
- **Transcription reads the whole file**, not a cursor: a thin `read_all` over `read()` in a loop. A minute of 16 kHz mono f32 is 3.8 MB, so a long take is tens of megabytes — acceptable, and whisper wants one contiguous slice anyway.

**It reads the commentary recording only**, never the source video — same as macOS, and the parent spec lists source-video audio as a non-goal. The recording's first audio track is the mic.

### S3. The model: `small.en`, downloaded on first use

**User decision (2026-09-20):** download on first use; `small.en`. Both were open question 3 / BACKLOG #22.

- **Default `ggml-small.en.bin`, 466 MB**, sha256 `c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d` (downloaded and verified while writing this spec). **Gated on S0:** if the spike shows `small.en` is slower than realtime, `base.en` (148 MB) becomes the default instead. The spec's old "~140 MB" figure was pricing `base`.
- **From** `huggingface.co/ggerganov/whisper.cpp` (weights MIT). Don't hardcode a quantization suffix in any model table: tiny/base/small ship `q5_1` but medium/large ship `q5_0`, and there is no `.en` variant of large.
- **Cached at `$XDG_CACHE_HOME/coach-cuts/models/<name>.bin`** — a cache, not config: it is large, re-downloadable, and machine-scoped. The *path in use* is recorded in the app's `StateFile` (`$XDG_CONFIG_HOME/coach-cuts/state.json`, which today holds only `last_project`), so a coach who supplies their own model keeps it across launches.
- **First use asks before it downloads**, names the size, and shows progress. This is the one thing macOS got wrong (BACKLOG #19: the download blocked transparently inside `transcribe()` with no progress), and it is why the parent spec recommended this option.
- **Verify the sha256 before use**, and write to `.part` then rename — the same discipline the export path already uses. A truncated model is otherwise a confusing whisper error much later.
- **A missing model is not an error at rest.** Nothing downloads until the coach transcribes something.

### S4. The transcript is a plain `String`, and that is a decision

`Clip.transcript: String` already exists in the Rust format (v7, `#[serde(default)]`, written empty, read by nothing). Keep it exactly as it is. **The format does not change; there is no v8 in this phase.**

whisper hands back per-segment timestamps for free, and storing them would allow click-a-line-to-seek, which macOS could never do. **Phase 10 does not do this,** for one reason that outweighs the feature: *the transcript is editable.* The moment the coach fixes a mangled player name, stored segment timings describe text that no longer exists, and every consumer of them needs a reconciliation story. Editable-plus-timestamped is a real design; editable-plus-timestamped-with-no-reconciliation is a bug waiting to be found. Segments are backlogged with that reasoning, not dismissed.

Segments are still **joined with a space**, matching the macOS implementation (its protocol doc claims newlines; the code at `AppleClipIntelligence.swift:60` joins with a space — the doc is wrong).

`""` means "not transcribed yet", deliberately, not `Option<String>` — the macOS spec's reasoning holds: one empty string for both the user-facing and the encoded state.

### S5. The queue: serial, FIFO, idempotent, and it admits when it is waiting

Keep `TranscriptionCoordinator`'s semantics, drop its structure. One job in flight, a FIFO queue behind it, and enqueueing a clip that is already queued or running does nothing. macOS serialized with `@MainActor`; here the **bus thread owns the queue**, which is the same guarantee and needs no new machinery.

The worker follows the **export precedent exactly**: a `Transcriber` owning a named `std::thread`, an `Arc<AtomicBool>` cancel, `on_message` called on the worker thread and forwarded into the bus's single `mpsc::Receiver<Input>`, and a `Drop` that cancels and joins.

- `Command::Transcribe { clip_id }` and `Command::CancelTranscription`.
- `Event::Transcription(TranscriptionRun)` carrying **the whole queue state**, not a delta — the same reason export sends whole-run snapshots: a view can't be left holding a state the bus has moved past.
- **State is `Idle | Queued | Running | Failed(String)`.** `Queued` is the fix for BACKLOG #18, which macOS still has: a queued clip reported `.idle`, so it looked never-transcribed and its button stayed enabled.
- **Progress is a percentage**, from `set_progress_callback_safe`. Unlike export, there is no frame count to report, and unlike macOS there is no excuse for a spinner with no number.
- **Failure is in-memory and per-clip**, as macOS had it: a relaunch starts every clip `Idle`. A failed transcript is cheap to retry and not worth a format change.
- **Cancellation cannot promise export's ~10 ms.** `set_abort_callback_safe` fires between ggml graph computations, so the latency is one compute step. Say so in the UI's wording rather than implying instant.
- **Mutual exclusion:** transcription must not run during a recording or an export. Both are already explicit, hand-rolled rules in the bus; this is a third.

### S6. Triggers

- **Automatically on recording stop**, as macOS did — the clip is enqueued right after it is added. **Gated on S0:** if the spike shows transcription is slower than realtime, this defaults off, because a coach recording six takes in a row would leave with a queue longer than the session.
- **A "Transcribe" button** on the clip inspector, for backfill and re-run. Re-running overwrites without a confirmation, as macOS did — the transcript is derived data and the coach asked.
- **No "transcribe all"** in this phase.

### S7. The undo carve-out, unchanged from macOS

`Workspace.applyAIWrite` saves and **never** pushes undo, and its rationale carries over intact: an undo entry from an out-of-band write gets bundled into the user's next focus-loss flush, so Ctrl+Z on a notes edit would silently revert the transcript.

In Rust the two paths are already distinct, and this is the whole of the contract:
- **The coach edits a transcript:** `ClipEdit::Transcript(String)` — a new variant on the existing enum, which `Clip::set` matches exhaustively, so the compiler finds the two places to change. Undoable like any other field edit.
- **The machine writes a transcript:** mutate, `save()`, `publish_project()`, and **skip `record`**.

BACKLOG #17 (Ctrl+Z clobbering an AI write) is **already fixed by construction** here — `ClipEdit` snapshots one field, not the whole clip — so it does not need re-litigating, only noting.

### S8. The README stops lying

`README.md:5` still claims "no network calls — transcription and summaries run on-device via Apple's `SpeechAnalyzer` and `FoundationModels`." Every clause is wrong for the port: there are no summaries, there is no Apple framework, and there is now a one-time download. Reword to "runs entirely on your machine; one-time model download on first transcription." The "No FFmpeg" line is separately false once `gst-libav` ships. Closes BACKLOG #23.

---

## Deliberately not in this phase

- **Summarization.** Deleted, not stubbed — the locked decision in the parent spec. `ClipIntelligence` and `TranscriptionWorkspace` do not survive: the former exists only because Apple's frameworks don't link in headless `swift test`, which in Rust is a Cargo feature.
- **Segment timestamps and click-to-seek** (S4). Backlogged with the editability reasoning.
- **Searching clips by transcript.** The sidebar filters by tag; transcript search is its own design.
- **Streaming partial transcripts.** `set_segment_callback_safe` exists, so this is now possible where macOS listed it as a non-goal — but a transcript that rewrites itself while the coach reads it is worse, not better.
- **Speaker diarization, translation, non-English models, source-video audio.**
- **GPU acceleration.** CPU-only; the reference laptop has no discrete GPU.

## Noted for later

**GStreamer 1.28 ships a `whispertranscriber` element** (gst-plugin-whisper, MPL-2.0) that wraps this same `whisper-rs 0.16`, with sink caps fixed at exactly 16 kHz/mono/F32LE. For a codebase that is already GStreamer-native that would turn this from a build problem into a packaging problem — no cmake, no bindgen, no C++ in the workspace. It is **unusable today**: the workspace pins `features = ["v1_24"]` and the reference laptop runs 1.24.2 on Ubuntu 24.04. Record it as the Phase 11+ migration path. That GStreamer upstream chose this binding is independent confirmation it is the right one.
