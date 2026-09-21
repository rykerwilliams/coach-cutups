# Linux Port — Phase 10: Transcription

**Date:** 2026-09-20
**Status:** Reviewed (simplify and correctness passes applied)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 10; the locked decision at line 22; open question 3 and risk 6)
**Builds on:** Phase 3 (clip editing and undo), Phase 4 (capture, which writes the audio this reads, and whose `CaptureKind::Test` seam this copies), Phase 8 (the audio-only decode pipeline this reuses)
**Evidence:** `apple/App/Intelligence/AppleClipIntelligence.swift`; `apple/VideoCoachCore/Sources/VideoCoachCore/Intelligence/{TranscriptionCoordinator,ClipIntelligence}.swift`; `apple/App/Models/Workspace.swift:815-832`; `apple/App/Views/ClipInspector.swift:271-360`; `docs/superpowers/specs/2026-05-21-clip-transcript-and-summary-design.md`. **whisper-rs 0.16.0 and whisper.cpp 1.8.3 were vendored and read** for every claim in S1.

---

## Goal

The coach records commentary over a clip. Afterwards the words are there as text, on the clip, editable, without sending anything to anyone.

## Done when

1. **A clip transcribes.** Stopping a recording queues its clip; the transcript appears when it lands.
2. **The queue is honest.** One job at a time, FIFO behind it, and a clip that is waiting *says* it is waiting.
3. **Recording always wins.** Hitting record never gets refused because a transcript is running.
4. **The transcript is editable** and survives a reload.
5. **Editing a transcript is undoable; the machine writing one is not.**
6. **The queue is tested without a model**, on CI, with no whisper build.

---

## Decisions

### S0. What gets measured, and when

**The risk worth retiring first is the build, not the throughput.** Before committing to the phase, confirm `whisper-rs 0.16` compiles on the reference laptop and on the CI runner — cmake, libclang, whisper.cpp's own CMake build. That is a hello-world, not a study.

**The throughput number is taken at the end of the phase, not as a gate.** An earlier draft made the default model and the auto-enqueue default conditional on a spike. That was ceremony: the spike cannot run without the build dependencies *and* the 16 kHz extraction, so it is the first two tasks with a stopwatch attached, and nothing structural branches on the answer — both "gated" decisions are a constant and a bool that flip in a one-line commit. The three GStreamer spikes exist because they decided whether an *approach* was viable, which is a different thing.

So: the transcribe task prints its wall-clock ratio, in the style the codebase already uses (`bus: exported …: N frames in X s (Y fps)`), and the closeout records it in `docs/superpowers/spikes/2026-09-20-whisper-throughput.md` along with the two defaults chosen from it.

**Pin what you measure, or the number is worthless.** `whisper_full_default_params` sets `n_threads = min(4, hardware_concurrency())` — **4 on an 8-thread laptop**, which halves the figure — and the default sampling strategy is `Greedy { best_of: -1 }`. Record `n_threads`, the strategy, the model, whether `openmp` was on (`build.rs` disables it by default), the clip length, and **whether the machine was on AC**: a 15 W mobile i7 throttles over a multi-minute run, so a 2-minute and a 10-minute take give different answers. `CLAUDE.md`'s `measure-media` skill exists for exactly this failure mode.

**Build dependencies are genuinely new.** `whisper-rs-sys`'s build-deps are `cmake`, `bindgen`, `fs_extra`, `cfg-if` and `semver`; it drives whisper.cpp's CMake build. The workspace already compiles C++ — but through skia-safe, which **downloads prebuilt binaries**, so neither `cmake` nor `libclang` is installed on the reference laptop (verified: both absent, and `target/debug/build/skia-bindings-*` exists). Both go on the dev machine, in CI, and into Phase 11's packaging story.

### S1. `whisper-rs 0.16.0` in `video-coach-media`, no feature gate

The crate placement is forced: `video-coach-core` declares no media dependency (its `Cargo.toml`, a dedicated CI job on a runner with no GStreamer, and the `verify` skill's exact four-dependency audit), and the job needs GStreamer to decode the recording anyway.

**No `feature = "whisper"`.** The `fixtures` precedent is not analogous — `fixtures` is test-only and adds no API the app compiles against. A `whisper` feature would gate a `Transcriber` the app needs, so either the app enables it unconditionally (and `cargo test --workspace` builds it anyway, making the gate worthless) or the command, the event, the bus field and the Slint wiring all become `#[cfg]`-conditional and a second, never-exercised compilation of the UI rots. Transcription is part of the product; it is a plain dependency. The honest cost — cmake and libclang for every contributor and every CI run, plus a multi-minute first build — is stated once here rather than hidden behind a flag. S8's test seam is what keeps the *queue* testable without any of it.

`whisper-rs 0.16.0` (newest on crates.io, Unlicense, MSRV 1.88 against the workspace's 1.92, vendoring whisper.cpp 1.8.3). `default = []` is a plain CPU build with no GPU SDK — keep it; the reference laptop has no discrete GPU. Licences are compatible: repo AGPL-3.0-or-later, whisper-rs Unlicense, whisper.cpp/ggml MIT, weights MIT.

**`set_abort_callback_safe` is unsound in 0.16.0. Do not call it naively.** Verified by reading the source: it boxes the closure into a `Box<Box<dyn FnMut() -> bool>>` and then installs `trampoline::<F>` with `F` the *concrete closure type*, so the trampoline reinterprets the fat pointer's data half as the closure. The correct sibling twelve lines above (`set_progress_callback_safe`) instantiates `trampoline::<Box<dyn FnMut(i32)>>`; `set_segment_callback_safe` is also correct. Only abort is wrong.

The fix costs nothing — hand it an already-boxed trait object so `F` *is* `Box<dyn FnMut() -> bool>` and the cast is correct:

```rust
let abort: Box<dyn FnMut() -> bool> = Box::new(move || cancel.load(Ordering::SeqCst));
params.set_abort_callback_safe(abort);
```

Comment it with the upstream bug. Cancellation is the whole of `Drop`-cancels-and-joins, so getting this wrong hangs or crashes the app on shutdown.

**An aborted run returns an error, not a cancellation.** whisper.cpp returns −6/−8 when the abort callback fires, and whisper-rs maps anything unrecognised to `WhisperError::GenericError(-6)`. So the worker must check **its own cancel flag before interpreting the return code** and yield a distinct `Cancelled` outcome. *Refined by the shipped-code review:* it takes **both** — the flag says why, the return code says whether. A run that was cancelled but **finished anyway** keeps its words, which is the trade the bus already makes for a `Finished` that beat the cancel, and which is also the only thing that makes the abort testable: with the flag set before the run, `Cancelled` can only come back if the callback really reached whisper. A test that reads `Cancelled` off our own flag would pass with the callback unplugged, and a timing bound would not discriminate either (see the two measurements above). Export already does exactly this (`ExportError::Cancelled`); transcription follows it rather than shipping `Failed("Generic error: -6")` when the coach hits record.

**Params to set explicitly**, because the defaults are wrong for us: `n_threads` (4 by default on an 8-thread machine), `print_progress = false` and `print_timestamps = false`, and `install_logging_hooks` — the latter is what keeps whisper off stderr, where it would bury `bus: loaded …`, the zero-copy diagnostic `CLAUDE.md` relies on. *Corrected by the shipped-code review:* in whisper.cpp 1.8.3 **neither flag writes anything**. `print_progress` is read by the CLI examples and never by the library, and `print_timestamps` is read only inside `if (params.print_realtime)`, which defaults false; the hooks are doing 100% of the work, and the 37-line model dump comes back the moment they go. Set the two flags anyway, as belt and braces against a future whisper.cpp — but never as a reason to drop the hooks. **Silence hallucination is whisper's characteristic failure and is likely here:** a coach who records ten seconds of nothing gets "Thank you." or "[BLANK_AUDIO]" written onto the clip. `no_speech_thold` and `suppress_nst` are the levers. This phase **names the failure and accepts it**; a transcript is one button away from being cleared.

**Corrections to the earlier draft, from reading the crate:** `full_n_segments` was **not** removed — it is public on `WhisperState` in 0.16.0. What moved is `full_get_segment_t0/t1/text`, now `WhisperSegment::start_timestamp()/end_timestamp()/to_str()` via `get_segment(i)`/`as_iter()`. Timestamps really are centiseconds. And abort granularity was claimed here to be **per graph node** — sub-millisecond in practice, so the UI needn't hedge. *Corrected by the shipped-code review, measured and then read in the source: both halves of that are wrong.* whisper.cpp's encoder and decoder call the `ggml_backend_sched_t` overload of `ggml_graph_compute_helper`, which **never** calls `ggml_backend_set_abort_callback`; only the per-node overload does, and nothing on this path reaches it. So the callback is consulted **once per encode and once per decode pass**. Measured twice, on different audio: a flag set 1 ms into `full` returned after **12.4 s**, and a three-second fixture — one 30-second chunk, so one encode — took **31 s** to abort against **29 s** run to completion, i.e. an abort inside a single chunk saves nothing at all. The UI therefore very much does have to hedge, and S5's `Drop` is where it does it. All three `*_safe` setters leak their box (`into_raw` with no matching free); three small boxes per job, negligible, noted so nobody hunts it later.

### S2. Audio extraction reuses `composite::audio::Reader` where it is

whisper.cpp requires **16 kHz mono f32 in [-1, 1]** and does not resample internally (`whisper_full` takes no rate argument; `WHISPER_SAMPLE_RATE 16000`). The recording is 48 kHz stereo Opus in Matroska.

`Reader` is already `filesrc ! decodebin3 (audio only) ! audioconvert ! audioresample ! appsink`. The reuse is worth it for one reason above all: **it detects a missing audio track from the `StreamCollection` rather than by prerolling.** `decodebin3` never posts `no-more-pads` here, so a recording with no audio track would otherwise hang forever. (The stream-selection optimisation — 2.2 s vs 46 ms for an unselected video stream — is real but irrelevant next to a whisper run measured in minutes. Don't lead with it.)

**It does not move out of `composite`.** An earlier draft proposed promoting it and relocating `POLL`, `QUEUED`, `Stopper`, `Watch` and `CompositeError`. Unnecessary: `CompositeError` is already `pub`, and the rest are used only inside `Reader`'s own implementation. Two visibility keywords (`pub(crate) mod audio`, `pub(crate) struct Reader`) let a sibling `transcribe.rs` call it, with zero churn in the export path. Give `Reader` its **own caps**, leaving the export tail's format alone (`caps_description` ended up parameterized by rate and channels, with export passing its own two constants — same string, one formatter); `AUDIO_SAMPLE_RATE` stays untouched, since it is the export mix rate with a compile-time assertion on it. Note the channel count is a module-level `const CHANNELS: usize = 2` that `read` and `Mixer` both use, so parameterizing is slightly more than a caps change.

Two corrections that matter before this reaches a plan:

- **`read_all` cannot be "a thin loop over `read()`".** `read` always returns exactly `frames * CHANNELS` samples, **zero-padded past EOF**, and gives the caller no EOF signal — a loop over it reads silence forever. *Corrected during Task 1:* rather than making `read` return a real-sample count, which perturbs the export's hot path for a caller that never uses it, transcription got its own entry point, `rest(&mut self, cancel) -> Result<Vec<f32>, CompositeError>` — everything from the cursor to the end of the file, over `pull`.
- **Transcription must use `start`, not `open`.** `open` collapses "no audio track" and "couldn't be read" into `None` with an `eprintln!("export: no sound from …")`. For export that collapse is correct — silence, run continues. For transcription the two are different failures the coach must see, and both must reach `Failed(msg)`: an empty transcript is indistinguishable from never-having-run, because S4 defines `""` as exactly that. Have the promoted `open` return `Result<Option<Reader>, _>` and let export keep its collapse at the call site. *Done in Task 1:* `CompositeError::Cancelled` no longer names the export, and the `"export:"` prefixes on the log lines `rest` can reach are gone — the path names the file. `Reader::open`'s prefix stays, because `open` is export's alone.

**What `rest`'s error does and does not close.** `Reader` records *why* it stopped, so a **cancel or a decode error** reaches the caller as `Err` rather than as a short buffer that nothing can tell from a whole one. That is the half that matters for cancellation, and it is closed. It is **not** a truncation guarantee: a Matroska or WebM recording cut off mid-stream — a crash, a full disk — commonly EOSes cleanly with no `ERROR` on the bus, so a ninety-second take comes back as ten seconds of `Ok` and transcribes as the whole thing. Closing that needs the duration in the container compared against the samples decoded, which is a separate piece of work; it is backlogged, and no code in this phase should be read as covering it.

**It reads the commentary recording only**, never the source video — as macOS did, and as the parent spec's non-goals require.

### S3. The model path is found, not fetched — the downloader is Phase 11

**User decision (2026-09-20), recorded and unchanged:** download on first use; `small.en`. That is the *decision*. **The implementation moves to Phase 11**, and the reasoning is a dependency the earlier draft priced at zero: the workspace has **no network dependency of any kind** — no `reqwest`, `ureq`, `hyper`, `rustls`, `native-tls`, `openssl`, `curl`, `ring` or `sha2`. Adding "download 466 MB over HTTPS with progress and a sha256 check" means a TLS tree larger than everything Phases 5–9 added combined, in a project whose `Cargo.toml` agonizes over one `cosmic-text` feature. BACKLOG #22 already parks the artifact-size question in the packaging phase, and that is where the cost belongs — **alongside the option of bundling, which would make the dependency moot**.

Phase 10 therefore:
- takes the model from **`$COACH_CUTS_WHISPER_MODEL`**, else `$XDG_CACHE_HOME/coach-cuts/models/ggml-<name>.bin` (with the `~/.cache` fallback — generalize `state.rs`'s existing `config_dir(xdg, home)`, which already implements this shape with three tests, rather than writing a second copy);
- treats a **missing model as `Failed`, with a message naming the exact path and the exact URL** to put there. Two lines and a good error message;
- **stores nothing.** An earlier draft put the path in `StateFile`, which has exactly one field and a doc contract that says losing it costs a re-open — losing a model path would cost a 466 MB re-download. And nothing in this phase lets the coach *supply* a model, so the field would only ever hold a derivable default.

This also buys the thing the earlier draft had no answer for: **an env var makes the whole path testable**, with a small model locally and none at all in CI.

For Phase 11, two notes so they aren't rediscovered: the download needs **per-model** sha256 (the earlier draft pinned one constant while leaving the model choice open), and `souphttpsrc ! filesink` is the GStreamer-native option — verified present, rank primary, already in CI's plugin set, follows the Hugging Face redirect, gives byte progress off the sink pad, and adds **zero** Rust dependencies.

**Model facts, so a table isn't written wrong:** `ggml-small.en.bin` is 466 MB, sha256 `c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d` (downloaded and verified byte-for-byte). `base.en` is 148 MB — the "~140 MB" the parent spec quoted. tiny/base/small ship `q5_1` but medium/large ship `q5_0`, so never hardcode a quantization suffix; there is no `.en` variant of large.

### S4. The transcript is a plain `String`, and that is a decision

`Clip.transcript: String` already exists (v7, `#[serde(default)]`, written empty, read by nothing). Keep it exactly. **The format does not change; there is no v8 in this phase.**

whisper hands back per-segment timestamps for free, and storing them would allow click-a-line-to-seek, which macOS could never do. **Phase 10 does not**, for one reason that outweighs the feature: *the transcript is editable.* The moment the coach fixes a mangled player name, stored timings describe text that no longer exists, and every consumer needs a reconciliation story. Editable-plus-timestamped is a real design; editable-plus-timestamped with no reconciliation is a bug waiting to be found. Backlogged with that reasoning, not dismissed.

**Join by concatenating the raw segment texts and trimming once.** Do *not* inherit macOS's space-join: whisper's BPE tokens carry their leading space, so every segment already begins `" Hello there."` and joining with a space yields a double space at every boundary plus a leading one. whisper's own spacing is already correct.

`""` means "not transcribed yet", deliberately, not `Option<String>` — one empty string for both the user-facing and the encoded state. This is also why S2's `open`-vs-`start` distinction matters: a swallowed extraction failure would be invisible.

### S5. The queue: serial, FIFO, idempotent, preemptible

Keep `TranscriptionCoordinator`'s semantics, drop its structure. One job in flight, FIFO behind it, and enqueueing a clip already queued or running does nothing. macOS serialized with `@MainActor`; here the **bus thread owns the queue**, which is the same guarantee with no new machinery. The queue earns its place because S6 auto-enqueues: six recordings produce six enqueues.

**One `Transcriber` per job — this reverses the draft, and the reversal is what shipped.** The draft asked for one worker thread for the whole queue, holding its `WhisperContext` between jobs, on the grounds that reloading `ggml-small.en.bin` would otherwise cost "minutes of pure overhead on a six-clip session". Plan Task 2 overrode it, and the code follows the plan: the overhead was overstated (a warm `whisper_init_from_file` is sub-second), a long-lived worker would hold 466 MB resident through an entire recording session beside the capture pipeline, and it contradicted this section's own "the bus thread owns the queue" — it needed a second channel and a drain protocol the design never named. So transcription follows the export precedent after all: one `Transcriber` per job, one thread per `Transcriber`. **Caching the context is BACKLOG #62**, which records the measurement that would justify it and the shape it would take.

The worker follows export exactly: a named `std::thread`, an `Arc<AtomicBool>` cancel, `on_message` called on the worker thread and forwarded into the bus's single `mpsc::Receiver<Input>`, and a `Drop` that cancels.

**One departure from export, forced by S1's corrected abort granularity: `Drop` cancels and does *not* join.** A whisper abort takes about twelve seconds to return, and every cancel here happens on the bus thread — a record starting, a clip trashed, a project opened, a quit. Joining made Stop Recording dead for twelve seconds (no recorder message handled, no deadline dispatched), and made quitting mid-transcription hold the UI's GL teardown for the same twelve seconds, since `Command::Shutdown` drops the bus before it acks. The abandoned thread is safe to let go: the generation counter discards its `Finished`, the transcript write already tolerates a job nobody is waiting for, and it holds nothing but its own model and samples. What it does cost, and what is accepted, is that the abandoned run is still on the CPU for those seconds — beside the capture encoder, if a recording is what preempted it.

- `Command::Transcribe { clip_id }` and `Command::CancelTranscription` — **the latter cancels the running job only and clears the queue**; say so, because "cancel" is otherwise ambiguous with a queue present.
- `Event::Transcription { queued: Vec<Uuid>, running: Option<(Uuid, u8)>, failed: Option<(Uuid, String)> }` — whole state, so no view is left holding something the bus has moved past, but **not** `ExportRun`'s struct-of-rows shape: that exists because the export sheet renders a list of targets with per-target progress and a time estimate. Transcription's UI is one inspector row. These three fields are exactly macOS's `queue` / `inFlightClipID` / `lastFailure`.
- **State is `Idle | Queued | Running(percent) | Failed(String)`**, with `Queued` *derived* from `queue.contains(id)` rather than stored — as macOS derived `state(for:)` from three scalars. `Queued` is the fix for BACKLOG #18, which macOS still has.
- **A cancel returns the clip to `Idle`**, never `Failed` (see S1).
- **Progress is a percentage** from `set_progress_callback_safe`. Unlike export there is no frame count, and unlike macOS there is no excuse for a spinner with no number.
- **Failure is one slot, in memory.** macOS had a single `lastFailure: (clipID, message)?`, not a per-clip map — "per-clip" read literally would build a `HashMap` macOS never had. A relaunch starts every clip `Idle`; a failed transcript is cheap to retry and not worth a format change.
- **Opening a project cancels the running job and clears the queue**, the same way the undo history is cleared. Otherwise the queue holds ids from the previous project and a dead job writes into nothing.

**Transcription is preemptible, not exclusive — and this is load-bearing.** The earlier draft said "transcription must not run during a recording or an export," which composes with S6 into a trap: stop clip 1 → transcription starts → hit record for clip 2 → *refused*. On a laptop where `small.en` may run near realtime, a coach recording six takes would be locked out of their own app for most of the session. Instead:

- **`run_next_if_idle` refuses to start** while `recording.is_some() || export.is_some() || preview.is_some()`. One condition, no new pairwise rule. (Preview belongs in it: `can_record` already treats preview as a third exclusive party, and it holds the audio sink.)
- **Starting a recording cancels the in-flight job and pushes its clip back to the front of the queue.** Three lines, and `cancel` must exist for `Drop` anyway.
- **`can_record` gains nothing**, and neither does the export path. Refusing an export because a transcript is running is the wrong trade.
- Half of what the earlier draft asked for **already exists**: `Bus::command` is a deny-by-default allow-list while recording, so `Command::Transcribe` is refused there by construction.

One note for whoever adds the fifth such rule: this is now a fourth pairwise `is_some()` guard. A single "exclusive job" concept would replace the chain — but adding that abstraction for a fourth would not earn its place today.

### S6. Triggers

- **Automatically on recording stop**, as macOS did. Whether this stays the default is decided at the closeout from S0's number: if transcription runs slower than realtime, a coach recording six takes in a row leaves with a queue longer than the session, and manual becomes the default.
- **A "Transcribe" button** on the clip inspector, for backfill and re-run. Re-running overwrites without confirmation, as macOS did — the transcript is derived data and the coach asked.
- **No "transcribe all"** in this phase.

### S7. The undo carve-out, unchanged from macOS

`Workspace.applyAIWrite` saves and **never** pushes undo, and its rationale carries over intact: an undo entry from an out-of-band write gets bundled into the user's next focus-loss flush, so Ctrl+Z on a notes edit would silently revert the transcript.

- **The coach edits a transcript:** `ClipEdit::Transcript(String)` — a new variant on the existing enum. `Clip::set` matches exhaustively so the compiler finds *that* site; the Slint `ClipField` enum and its match in `main.rs` are a second site the compiler only finds **after** the `ClipField` case is added. Don't claim the compiler finds both.
- **The machine writes a transcript:** mutate, `save()`, `publish_project()`, and **skip `record`**.

**BACKLOG #17 is fixed by construction for the cross-field case only.** `ClipEdit` snapshots one field, so a transcript write can't be bundled into a *notes* edit. But `show_clip` refuses to re-render while any clip field has focus, so a machine write landing while the coach has the **transcript field itself** focused is silently overwritten by their stale text on focus loss, as one undoable edit. Rare, one click to recover, and the same trade #17 already accepted — **accepted, not fixed**, and #17 stays open with that narrowed scope.

### S8. A one-method seam, so the queue is tested without whisper

macOS's `ClipIntelligence` had **two** reasons to exist, and the parent spec only retires one. Yes, the real implementation lived in the App target because `Speech` and `FoundationModels` don't link headless — a Cargo feature (or here, crate placement) replaces that. But its docstring also says: *"The test fake in this package returns canned strings so coordinator tests are deterministic and run headlessly."* Nothing replaces that, and without it Phase 10 ships with the FIFO queue, idempotent enqueue, one-in-flight, `Queued` derivation, preemption, cancel and failure mapping — *"the part worth keeping"* — untested.

So: **transcription goes through a one-method seam** (`fn(&[f32], progress, abort) -> Result<String, _>`, injected), with a test implementation that returns canned text after a controllable delay. This is not re-adding `ClipIntelligence`; it is the same trick `CaptureKind::Test` already uses for the recorder, which `CLAUDE.md` codifies.

**Then `video-coach-harness` tests the queue on CI with no model and no whisper run:** enqueue while running, idempotent re-enqueue, FIFO order, preemption by a recording and requeue at the front, cancel → `Idle`, failure → `Failed`, project-open clearing. The real whisper call is exercised by one local test behind the env-var model path, and by the closeout's measurement.

---

## Deliberately not in this phase

- **Summarization.** Deleted, not stubbed — the locked decision in the parent spec. `TranscriptionWorkspace` does not survive either.
- **The model downloader** (S3) — Phase 11, with the artifact-size and bundling decision BACKLOG #22 already parks there.
- **The README rewrite.** `README.md` is 113 lines describing a macOS app: line 4 is "Native macOS app…", line 5 opens "Built on Swift + SwiftUI + AVFoundation", and the Tests section is `swift test` and `xcodegen`. Rewording one clause of line 5 would leave it false in a dozen places and internally contradictory besides. BACKLOG #23 already defers the rewrite to Phase 11, "when the Linux build becomes the primary artifact"; leave it whole until then. (With the downloader gone, Phase 10 adds no network call anyway.)
- **Segment timestamps and click-to-seek** (S4).
- **Searching clips by transcript.**
- **Streaming partial transcripts.** `set_segment_callback_safe` exists and is sound, so this is now possible where macOS listed it as a non-goal — but a transcript that rewrites itself while the coach reads it is worse, not better.
- **Speaker diarization, translation, non-English models, source-video audio, GPU acceleration.**

## Noted for later

**GStreamer 1.28 ships a `whispertranscriber` element** (gst-plugin-whisper, MPL-2.0) wrapping this same `whisper-rs 0.16`, with sink caps fixed at exactly 16 kHz/mono/F32LE. For a codebase already GStreamer-native that would turn this from a build problem into a packaging problem — no cmake, no bindgen, no C++ in the workspace. **Unusable today:** the workspace pins `features = ["v1_24"]` and the reference laptop runs 1.24.2 on Ubuntu 24.04. Record as the Phase 11+ migration path. That GStreamer upstream chose this binding is independent confirmation it is the right one.
