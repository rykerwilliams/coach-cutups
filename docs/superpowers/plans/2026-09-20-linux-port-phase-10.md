# Linux Port — Phase 10 Plan (Transcription)

**Date:** 2026-09-20
**Spec:** `docs/superpowers/specs/2026-09-20-linux-port-phase-10-design.md` (decisions S0–S8)
**Status:** Reviewed (simplify and correctness passes applied).

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the workspace and passes its tests on its own.

**Task order is chosen around a blocker, and the order matters.** `cmake` and `libclang-dev` are not installed and cannot be installed without the user. S1 forbids a feature gate, so the moment `whisper-rs` becomes a dependency of `video-coach-media`, **every** `cargo build`, `cargo test --workspace` and `verify` in the repo needs them — and `video-coach-app` depends on `video-coach-media`, so there is no escape via `-p`. The backend therefore comes **fourth, after the UI**: Tasks 1–3 ship real, verified progress while the blocker stands.

**Known facts. Don't re-derive these — each was verified against primary source during review.**
- **whisper-rs 0.16.0 / whisper.cpp 1.8.3 were vendored and read.** `full_n_segments` still exists; `full_get_segment_t0/t1/text` moved onto `WhisperSegment` (`start_timestamp`/`end_timestamp`/`to_str`) via `get_segment(i)`/`as_iter()`; timestamps are **centiseconds**; `full()` returns `Result<(), WhisperError>`.
- **`set_abort_callback_safe` is unsound in 0.16.0** (BACKLOG #60): it boxes to `Box<Box<dyn FnMut() -> bool>>` then installs `trampoline::<F>` with `F` the concrete closure type. Passing an **already-boxed** `Box<dyn FnMut() -> bool>` makes `F` the boxed type and the cast correct — verified. `set_progress_callback_safe` hardcodes the right instantiation and is fine.
- **An aborted run returns −6, −8 *or* −9** depending on where the abort lands (encode / first decode / beam decode). All become `GenericError(n)`. **Never assert on the code**; check our own cancel flag first and report `Cancelled`.
- **`whisper_full_default_params`:** `n_threads = min(4, hardware_concurrency())` — 4 on this 8-thread laptop — plus `print_progress = true`, `print_timestamps = true`, `greedy.best_of = -1`. All must be set explicitly. `install_logging_hooks` exists un-gated.
- **The progress callback fires at the top of the chunk loop** as `100*(seek - seek_start)/(seek_end - seek_start)`, with `seek` advancing in ≤30 s chunks, and **never reports 100%**. A 20 s clip gets **exactly one callback, reading 0**. Verified in `whisper.cpp:7000-7004`. Design around this (Task 3.2), don't ship a bare percentage.
- **whisper needs 16 kHz mono f32**; it does not resample internally. `full()` returns `Ok(())` with **zero segments** for silence or input under 100 ms.
- **`Reader::start` already returns `Result<Option<Reader>, CompositeError>`** (`audio.rs:258`). `open` (`:245`) is the collapsing wrapper export wants — it folds "no audio track", "unreadable" **and "cancelled"** into `None`. **Leave both alone**; transcription calls `start`.
- **`Reader::read` zero-pads past EOF with no EOF signal, but `pull` returns `false`** when nothing more is coming. **`pull` sets `done = true` on cancel exactly as on EOS** — that conflation is the trap: a cancelled read looks like clean EOF and yields a silently truncated buffer.
- **`Reader::start`'s stream-collection loop exits only on `watch.check()`**, so extraction needs a real cancel flag or it cannot be preempted or shut down.
- **`AUDIO_SAMPLE_RATE` is a core `const` with a compile-time assertion** — it is the export mix rate. Don't touch it. `CHANNELS` is a module `const` that `read` and `Mixer::block` both index with.
- **`Bus::command` is deny-by-default while recording**, so `Command::Transcribe` is already refused there. **`can_record` already refuses for export *and* preview** — it needs no transcription clause.
- **`Bus::project_changed()`** (`bus/project.rs:108-111`) *is* save + publish without `record`. Use it; don't inline a fourth copy.
- **`trash_clip`** (`bus/clips.rs:214-236`) already calls `close_preview_of(id)` first, because the preview holds the recording open. Transcription needs the same treatment.
- **`Clip.transcript: String`** exists at v7, `#[serde(default)]`, read by nothing. **No format bump.** `store::read` refuses `found < CURRENT_FORMAT_VERSION`, so a v8 would make every existing project unreadable by this build.
- **`CaptureKind`** is in `video-coach-app/src/bus/recording.rs:34` and **`CaptureSources::Test` is unconditionally compiled**, not behind `fixtures` (which is `video-coach-media`'s, for synthetic media files). The capture seam is a plain always-compiled enum resolved at `Bus::spawn` — that is the pattern to copy.
- **GitHub's `ubuntu-latest` already ships cmake and clang**, and the `workspace` job already uses `Swatinem/rust-cache@v2`. The apt additions are insurance, not load-bearing.

---

## Task 1 — Media: 16 kHz mono extraction

No whisper, no cmake.

1. **Visibility only:** `pub(crate) mod audio`, `pub(crate) struct Reader`, `pub(crate) fn start`, and the new `rest` below. **Four keywords, not two.** Don't promote the module; don't move `POLL`, `QUEUED`, `Stopper`, `Watch`; `CompositeError` is already `pub`.
2. **A parametrized `caps(rate, channels)` alongside** the existing `caps_description()`, which the export tail uses. **Leave `read`, `CHANNELS` and `Mixer` untouched** — perturbing the export's hot path for a caller that never uses it isn't worth it.
3. **`pull` must distinguish cancelled from EOF.** Today both set `done = true`, so a cancelled read is indistinguishable from a clean end — which would hand whisper a silently truncated clip and transcribe it as complete. This is the one real change inside `Reader`.
4. **`rest(&mut self, cancel: &AtomicBool) -> Result<Vec<f32>, CompositeError>`** — everything from the cursor to EOF, over `pull`, ~8 lines. Not over `read`, which pads forever.
5. **Bound `start`'s stream-collection wait** so a file that posts neither an error nor a collection fails instead of hanging.
6. **`crates/video-coach-media/src/transcribe.rs`:** `read_all(path, cancel) -> Result<Vec<f32>, _>` at 16 kHz mono via `start` (never `open`), mapping `Ok(None)` → a distinct "no audio track" error and `Err(_)` → "unreadable". **It runs on the worker thread, never the bus** — decoding a minute of audio takes about a second and would stall the event loop.
7. **Tests:** extraction is 16 kHz mono; the sample count matches the duration **within a tolerance** (`audioresample` has filter latency and a tail — an exact count flakes on a plugin bump); no audio track gives that error rather than hanging or returning silence; a damaged file gives the other error; a cancel mid-extraction returns `Cancelled`, **not a short buffer**; export's audio tests pass unchanged.

Commit: `feat(media): 16 kHz mono extraction for transcription`.

## Task 2 — The seam, the queue and the bus

No whisper, no cmake. **This is the task that makes the phase testable.**

1. **The seam mirrors `CaptureKind`:** `TranscribeKind { Whisper { model: PathBuf }, Test { delay, text } }`, resolved inside `video-coach-media`, chosen at `Bus::spawn`. Unconditionally compiled — **not** behind `fixtures`, which the app doesn't enable and so could never name. Both `Bus::spawn` and `spawn_with_state` gain the parameter. The call takes **PCM** (`&[f32]`), not a path, so extraction errors stay distinguishable from transcription errors and the shape matches Phase 11's `whispertranscriber`. Abort is an **`&AtomicBool`**, as `Exporter` and `Reader` already use.
2. **`Transcriber`: one thread per job**, copying `Exporter` exactly — named thread, `Arc<AtomicBool>` cancel, `on_message` on the worker, `Drop` cancels and joins. **The bus owns the queue** (`VecDeque<Uuid>`), unambiguously. An earlier draft had a long-lived worker caching the model "to save minutes"; that contradicted the spec's own "the bus thread owns the queue", needed an unspecified second channel and a drain protocol, and would hold 466 MB resident through a whole recording session. Backlog the context cache; let Task 5's number decide if it ever earns its place.
3. **A generation counter**, `transcribe_generation: u64`, bumped on every cancel or clear, stamped into `Input::Transcription`, checked on entry. Every other async subsystem on this bus has one (`generation` for the recorder, `preview_generation`). Without it: a job finishes into the channel, the project is closed, a new job starts, the stale `Finished` clears `running`, and `run_next_if_idle` launches a **second concurrent job**.
4. **Bus surface:** `Command::Transcribe { clip_id }`, `Command::CancelTranscription` (**the running job only, and it clears the queue**), `Input::Transcription(u64, _)`, and
   `Event::Transcription { queued: Vec<Uuid>, running: Option<(Uuid, u8)>, failed: Option<(Uuid, String)> }` — whole state, three fields, **not** `ExportRun`'s struct-of-rows shape.
5. **Queue rules, in full:**
   - FIFO; enqueue is idempotent against **the queue *and* the running clip** (the running clip is not in `queue`, so a naive `contains` re-runs it);
   - `Queued` is **derived** from `queue.contains(id)`, never stored;
   - `run_next_if_idle` refuses to start while `recording.is_some() || export.is_some() || preview.is_some()`;
   - **it must be called from:** enqueue, job finish, cancel, `finish_recording`, **`abort_recording`** (a recording that produced no clip must still let the preempted job resume), export's final `Finished`, and `close_preview` — and **must not start a job during shutdown**, since `close_preview` is also called from the `Shutdown` handler. Miss one and the queue stalls silently until the next enqueue;
   - **preemption:** starting a recording cancels the in-flight job and pushes its clip to the **front**. Fire the cancel **immediately after `self.recording = Some(...)`**, never at the top of `toggle_recording` — that function bails at five points, and a refused record (e.g. "a preview is open") would otherwise kill a transcript for nothing;
   - **a cancel returns the clip to `Idle`**, never `Failed`;
   - **a cancel that arrives too late keeps the transcript**, following the export precedent ("a cancel too late to stop a target reports it done"). Discarding finished work is the worse trade;
   - **`failed` is cleared** on that clip's next enqueue, on that clip's next success, and on project open. Otherwise the message sticks for the session, including after a successful re-run;
   - **opening a project** cancels and clears, via `commit()` — the single hook both `open_project` and `restore_last_project` funnel through;
   - **`trash_clip` cancels-if-running and dequeues**, beside its existing `close_preview_of`. Otherwise a deleted clip's job runs against a recording now in `.trash/` and leaves a permanent error naming a clip that no longer exists. The AI write additionally relies on `apply_edit` returning `Option`, so a clip deleted mid-job no-ops.
6. **Auto-enqueue on recording stop** — bus logic, hooked in `finish_recording`, behind a **`const`**, not a `Preferences` field. `Preferences` lives in `project.json`, so a field there is an additive schema change that `CLAUDE.md` says to bump for — and a bump to v8 would make every existing project unreadable. The closeout flips the literal.
7. **The model path is resolved here, in the app crate**, beside `state.rs`'s `config_dir`, and passed down as a `PathBuf` in `TranscribeKind::Whisper`. Media cannot reach into the app, and `config_dir` is a private app fn — "generalize it" is unimplementable from media. Generalize it to take the env var name and the `.config`/`.cache` suffix, since this wants `XDG_CACHE_HOME`.
8. **The AI write is `project_changed()`** — the existing helper, not a re-inlined save+publish.
9. **Harness tests, all on the test transcriber with no model:** auto-enqueue on recording stop; enqueue while running; idempotent re-enqueue of a queued **and** of the running clip; FIFO order; preemption by a recording with requeue at the front; resume after `abort_recording`; cancel → `Idle`; failure → `Failed`; `failed` cleared on re-enqueue; project-open clears; a clip deleted while queued and while running; **and the AI write leaves the undo stack untouched** while a subsequent user edit is undoable.

Commit: `feat(app): the transcription queue`.

## Task 3 — UI

No cmake. The last task that compiles without it.

1. **`ClipEdit::Transcript(String)` touches seven sites**, and the compiler finds only two:
   | Site | Compiler finds it? |
   |---|---|
   | `core/src/undo.rs:37` `ClipEdit` | — |
   | `core/src/project.rs:157` `Clip::set` | yes |
   | `ui/app.slint:80` `ClipField` | no |
   | `src/main.rs:702-704` field match | only after the `ClipField` case exists |
   | **`ui/app.slint:307` `editing: name.has-focus \|\| tags.has-focus \|\| notes.has-focus`** | **no — and this one is a bug** |
   | `src/main.rs:1368-1371` `show_clip` | no |
   | `ui/app.slint:1274` + `:2149` the `clip-notes` sibling property and its `<=>` binding | no |

   The `editing` omission is the dangerous one: it feeds `text-editing`, which gates every window shortcut. Miss it and typing a transcript makes Space start playback, `r` start a recording and Esc close things **while the coach types**.
2. **Progress is not a bare percentage.** whisper reports 0 for the whole of a typical clip (see Known facts). The inspector shows **elapsed wall-clock** from when it first saw that clip running — "Transcribing… 0:24" — and appends the percentage only when it is non-zero. A number that sits at 0% for forty seconds looks stuck; a rising clock doesn't.
3. **Inspector:** the transcript field, a **Transcribe** button, the running state from item 2, a **Cancel** control beside it (`Command::CancelTranscription` otherwise has no caller, and at `small.en` speeds abandoning an eight-minute job is a real want), "Queued" when queued (BACKLOG #18's fix), and the failure message.
4. **A completed run with an empty transcript reads "no speech found", not blank.** `full()` returns `Ok(())` with zero segments for silence, and S4 defines `""` as "not transcribed yet" — so without this the coach presses Transcribe, waits, and sees no change whatsoever. Derive the state from `Event::Transcription` plus the transcript, not from `transcript.is_empty()` alone.
5. **Decide the layout, don't leave it to the implementer.** `notes` is already a stretchy `TextEdit` and the last stretchy child of the inspector's `VerticalLayout`. A second one halves both in a panel that is already tight. Use a fixed-height transcript box with its own scroll.
6. **Screenshots** through callbacks, no camera: a clip mid-transcription, a queued clip, a failure, and a "no speech found".

Commit: `feat(app): the transcript field and Transcribe button`.

## Task 4 — The whisper backend

**Blocked on `sudo apt install cmake libclang-dev`.** Its literal first step is a hello-world `cargo build` of `whisper-rs`, before any backend code is written — that is S0's build risk, retired here because the blocker forced it last.

1. **`whisper-rs 0.16.0` as a plain dependency** of `video-coach-media` — **no feature gate** (S1). Add `cmake` and `libclang-dev` to `.github/workflows/rust.yml`'s `workspace` job (insurance; the runner has them) and note the first-build cost in `CLAUDE.md`.
2. **Implement `TranscribeKind::Whisper`:** the **boxed** abort callback with a comment citing BACKLOG #60; **check our cancel flag before interpreting the return code**, so a cancel is `Cancelled` and never `GenericError(n)`; `set_progress_callback_safe` for the percentage; explicit `n_threads`, `print_progress = false`, `print_timestamps = false`, `install_logging_hooks`.
3. **Join by concatenating raw segment texts and trimming once** — whisper's tokens carry their leading space, so a space-join double-spaces every boundary.
4. **A missing model is `Failed` naming the exact path and the exact URL.**
5. **Name silence hallucination in a comment** where the params are set: ten seconds of nothing yields "Thank you." or "[BLANK_AUDIO]". Accepted this phase.
6. **Print the throughput on completion** — `bus: transcribed …: N s of audio in X s (Y×)`, with the model name and `n_threads` in the line, so Task 5's spike cannot be written against an unrecorded configuration.
7. **Tests:** `#[ignore]`d (not env-skipped — a test that passes vacuously on CI forever is exactly the failure mode `fixtures.rs` warns about), with the exact command in `CLAUDE.md`: a short fixture transcribes; the missing-model message names path and URL; a cancel yields `Cancelled` and **asserts no return code**.

Commit: `feat(media): whisper transcription`.

## Task 5 — Closeout

1. Adversarial review of the Phase 10 diff; apply and backlog.
2. **Measure and record** `docs/superpowers/spikes/2026-09-20-whisper-throughput.md`: wall-clock per minute of audio for `small.en` and `base.en`, **pinning `n_threads`, the sampling strategy, `openmp`, the clip length and whether the machine was on AC** (a 15 W i7 throttles over a multi-minute run).
3. **Pick the two defaults from it** — model, and whether auto-enqueue stays on. **Weigh the livelock more heavily than the raw number:** a preempted job restarts from zero, so if the coach's recording cadence is shorter than a job's runtime, *no job ever completes* while they keep recording — front-requeue or back, either way. It is bounded (the queue drains once recording stops) and the fixes all cost real complexity (chunked checkpointing, partial-transcript merge), so **no fix is recommended** — but it is the strongest argument for auto-enqueue defaulting off.
4. `CLAUDE.md`: the build requirement and its first-build cost, the model path and the `#[ignore]`d test's command, the preemption rule, and the progress-reporting trap.
5. Hands-on checklist items.

## Deliberately not in this phase

- The model downloader (Phase 11, with the bundling decision).
- The README rewrite (Phase 11, per BACKLOG #23).
- Segment timestamps, transcript search, streaming partials, summarization.
- Caching the `WhisperContext` across queued jobs (backlogged from Task 2.2).
