# Linux Port — Phase 10 Plan (Transcription)

**Date:** 2026-09-20
**Spec:** `docs/superpowers/specs/2026-09-20-linux-port-phase-10-design.md` (decisions S0–S8)
**Status:** Draft, pre-review.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the workspace and passes its tests on its own.

**Task order is chosen around a blocker.** `cmake` and `libclang-dev` are not installed on the reference laptop and cannot be installed without the user. Only **Task 3** needs them. Tasks 1, 2 and 4 are written so they compile and test with no whisper build at all, which is the same property that keeps the queue testable on CI forever (S8).

**Known facts. Don't re-derive these.**
- **whisper-rs 0.16.0 and whisper.cpp 1.8.3 were vendored and read** during the spec review. Everything in S1 is quoted from source, not from a blog post: `full_n_segments` still exists; `full_get_segment_t0/t1/text` moved onto `WhisperSegment` as `start_timestamp()`/`end_timestamp()`/`to_str()` via `get_segment(i)`/`as_iter()`; timestamps are **centiseconds**; `full()` returns `Result<(), WhisperError>`.
- **`set_abort_callback_safe` is unsound in 0.16.0** (BACKLOG #60). Pass an **already-boxed** `Box<dyn FnMut() -> bool>` so the trampoline's `F` is the boxed type. A naive closure is UB.
- **An aborted run returns −6**, which whisper-rs maps to `GenericError(-6)`. Check our own cancel flag **before** interpreting the return code.
- **`whisper_full_default_params` sets `n_threads = min(4, hardware_concurrency())`** — 4 on this 8-thread laptop — and `print_progress = true`. Both must be set explicitly.
- **whisper needs 16 kHz mono f32** and does not resample internally.
- **`Reader::read` zero-pads past EOF and gives no EOF signal**, so a loop over it never terminates. **`Reader::open` collapses "no audio track" and "unreadable" into `None`** with an `eprintln!("export: …")`; transcription needs both distinguished and neither silent.
- **`AUDIO_SAMPLE_RATE` is a core `const` with a compile-time assertion on it** — it is the export mix rate. Do not touch it. `CHANNELS` is a module-level `const` used by `read` and `Mixer` both.
- **`Bus::command` is a deny-by-default allow-list while recording**, so `Command::Transcribe` is already refused there. `can_record` already refuses for export **and preview**.
- **`Clip.transcript: String`** exists at v7, `#[serde(default)]`, written empty, read by nothing. **No format bump in this phase.**
- **`ClipEdit`** has exactly `Name/Tags/Notes/ShowPip`; `Clip::set` matches exhaustively. The Slint `ClipField` enum + its match in `main.rs` is a **second** site the compiler only finds after the case is added.
- **The recorder's `CaptureKind::Test` seam** is the pattern Task 2's injected transcriber copies.

---

## Task 1 — Media: 16 kHz mono extraction

No whisper, no cmake. Pure GStreamer, testable today.

1. **`pub(crate) mod audio;` and `pub(crate) struct Reader`** in `composite` — two visibility keywords. **Do not** promote the module or relocate `POLL`, `QUEUED`, `Stopper`, `Watch` or `CompositeError` (already `pub`).
2. **Give `Reader` its own caps** (rate + channels) rather than changing `caps_description()`, which the export tail uses. Thread the channel count through `read`, which computes against the module `const CHANNELS`.
3. **`read` also returns the count of real samples**, so EOF is in the type. Export ignores it — it wants the zeros.
4. **`open` returns `Result<Option<Reader>, _>`**; export keeps its collapse at the call site. Rename `CompositeError::Cancelled`'s `"the export was cancelled"` message and the `"export:"` log prefixes now that a second caller exists.
5. **`crates/video-coach-media/src/transcribe.rs`**: `read_all(path) -> Result<Vec<f32>, _>` at 16 kHz mono, built on `start` (not `open`), mapping `Ok(None)` → a distinct "no audio track" error and `Err(_)` → "unreadable".
6. **Tests:** a fixture recording extracts to exactly 16 kHz mono; the sample count matches the duration; a file with no audio track gives the "no audio track" error rather than hanging or returning silence; a damaged file gives the other error; export's own audio tests still pass unchanged.

Commit: `feat(media): 16 kHz mono extraction for transcription`.

## Task 2 — The seam, the queue and the bus

No whisper, no cmake. **This is the task that makes the phase testable.**

1. **The seam:** `trait Transcribe { fn run(&self, pcm: &[f32], progress: &mut dyn FnMut(u8), abort: &dyn Fn() -> bool) -> Result<String, TranscribeError>; }` — shape it against what Task 3 actually needs, but keep it one method. Plus a test implementation returning canned text after a controllable delay, in the `fixtures` feature alongside `CaptureKind::Test`'s neighbours.
2. **`Transcriber`:** one named thread for the **whole queue**, not one per job — the model loads once when the queue goes non-empty and drops when it drains. `Arc<AtomicBool>` cancel, `on_message` on the worker thread, `Drop` cancels and joins.
3. **Bus:** `Command::Transcribe { clip_id }`, `Command::CancelTranscription` (**running job only, and clears the queue**), `Input::Transcribe(_)`, and
   `Event::Transcription { queued: Vec<Uuid>, running: Option<(Uuid, u8)>, failed: Option<(Uuid, String)> }` — whole state, three fields, **not** `ExportRun`'s struct-of-rows shape.
4. **Queue rules:** FIFO; idempotent enqueue; `Queued` **derived** from `queue.contains(id)`, never stored; `run_next_if_idle` refuses to start while `recording.is_some() || export.is_some() || preview.is_some()`; **starting a recording cancels the in-flight job and pushes its clip to the front**; a cancel returns the clip to `Idle`, never `Failed`; opening a project cancels and clears.
5. **The AI write:** mutate, `save()`, `publish_project()`, **skip `record`**. Not through `Clip::set`.
6. **Harness tests, all with the test transcriber and no model:** enqueue while running; idempotent re-enqueue; FIFO order; **preemption by a recording, and requeue at the front**; cancel → `Idle`; failure → `Failed`; project-open clears; and **the AI write leaves the undo stack untouched** while a subsequent user edit is undoable.

Commit: `feat(app): the transcription queue`.

## Task 3 — The whisper backend

**Blocked on `sudo apt install cmake libclang-dev`.** Everything else ships without it.

1. **`whisper-rs 0.16.0` as a plain dependency** of `video-coach-media` — **no feature gate** (S1). Add `cmake` and `libclang-dev` to `.github/workflows/rust.yml`'s `workspace` job, and note the first-build cost in `CLAUDE.md`.
2. **Implement the seam** over `WhisperContext`/`WhisperState`:
   - the **boxed** abort callback (BACKLOG #60), with a comment citing the upstream bug;
   - **check our cancel flag before interpreting the return code**, so a cancel is `Cancelled`, not `GenericError(-6)`;
   - `set_progress_callback_safe` for the percentage;
   - explicit `n_threads`, `print_progress = false`, `print_timestamps = false`, `install_logging_hooks`.
3. **Join by concatenating raw segment texts and trimming once** — whisper's tokens carry their leading space, so a space-join double-spaces every boundary.
4. **The model path:** `$COACH_CUTS_WHISPER_MODEL`, else `$XDG_CACHE_HOME/coach-cuts/models/ggml-<name>.bin` with the `~/.cache` fallback — **generalize `state.rs`'s `config_dir(xdg, home)`** rather than writing a second copy. A missing model is `Failed` with a message naming **the exact path and the exact URL**. Store nothing.
5. **Name silence hallucination in a comment** where the params are set: ten seconds of nothing yields "Thank you." or "[BLANK_AUDIO]". Accepted this phase.
6. **Tests:** one local test behind the env-var model path transcribing a short fixture; the missing-model message names path and URL; cancel yields `Cancelled` not a generic error. The local test must **skip, not fail**, when the env var is unset, so CI stays green.

Commit: `feat(media): whisper transcription`.

## Task 4 — UI

No cmake needed (the seam is injectable), though the real backend makes it worth a manual look.

1. **`ClipEdit::Transcript(String)`** — the enum, `Clip::set`, **and** the Slint `ClipField` case plus its match in `main.rs`. The compiler only finds the first.
2. **Inspector:** a transcript field (multi-line, editable, like notes), a **Transcribe** button, the percentage while running, "Queued" when queued (BACKLOG #18's fix), and the failure message.
3. **Auto-enqueue on recording stop**, behind a preference so the closeout can flip the default from S0's number without touching code.
4. **Screenshots** through callbacks, no camera: a clip mid-transcription showing progress, a queued clip, and a failure.

Commit: `feat(app): the transcript field and Transcribe button`.

## Task 5 — Closeout

1. Adversarial review of the Phase 10 diff; apply and backlog.
2. **Measure and record** `docs/superpowers/spikes/2026-09-20-whisper-throughput.md`: wall-clock per minute of audio for `small.en` and `base.en`, **pinning `n_threads`, the sampling strategy, `openmp`, the clip length and whether the machine was on AC** (a 15 W i7 throttles over a multi-minute run).
3. **Pick the two defaults from it** — model, and whether auto-enqueue stays on.
4. `CLAUDE.md`: the build requirement, the model path, and the preemption rule.
5. Hands-on checklist items.

## Deliberately not in this phase

- The model downloader (Phase 11, with the bundling decision).
- The README rewrite (Phase 11, per BACKLOG #23).
- Segment timestamps, transcript search, streaming partials, summarization.
