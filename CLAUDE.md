# Coach Cutups — Project Conventions

## Workflow for non-trivial features

Each feature goes through a four-stage loop, with adversarial review at every artifact handoff:

1. **Brainstorm → spec** (`docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`)
2. **Adversarial review on the spec** — see "Review pattern" below. Apply fixes, then commit.
3. **Write plan** (`docs/superpowers/plans/YYYY-MM-DD-<topic>.md`)
4. **Adversarial review on the plan** — same pattern. Apply fixes, then commit.
5. **Compact the conversation before plan execution.** Plans get long; execution dispatches many subagents and consumes context fast. Start the execution phase with fresh context — re-read the plan + spec + this file rather than relying on accumulated chat history.
6. **Execute** via `superpowers:subagent-driven-development` (fresh subagent per task).
7. **Adversarial review on the shipped code changes** — apply fixes, then commit.
8. **Backlog deferred items** to `BACKLOG.md` at the worktree root.

## Review pattern (use for specs, plans, and shipped code)

For each review pass, spawn **two adversarial agents in parallel**:

- **Simplify agent** — find every place the design / plan / code is more complex than it needs to be. Recommended subagent: `general-purpose`. Frame as "adversarial simplification review."
- **Code-review / correctness agent** — find correctness bugs, fragile patterns, things that pass tests today but break tomorrow. Recommended subagent: `feature-dev:code-reviewer` for code; `general-purpose` for specs/plans.

Both agents get:
- The artifact under review (spec, plan, or diff range)
- The relevant codebase reference paths (so they can verify claims, not just trust the artifact)
- The full "user values" block (below)

After both reviews return:

1. **Group similar findings** across the two reviews.
2. **Spawn one deliberation agent per group** (in parallel). Each agent's job:
   - Research all issues in its group against the codebase
   - For each issue, decide the best long-term fix
   - Adversarial self-review of its own conclusions
   - **Defer to human** if the right fix isn't obvious
3. **Apply / skip per group**:
   - **APPLY** when the fix is strictly better than the original
   - **SKIP** when the fix is worse than the original issue (every change must earn its place)
   - **DEFER** when judgment is required from the human
4. Surface anything deferred at the end.

## User values (paste into every adversarial review prompt)

- Best long-term design over short-term tradeoffs
- It's OK to change adjacent code if it helps get to the best long-term design
- Simplicity — avoid over-engineered systems and fixes
- Don't care about effort or severity
- Care about long-term codebase quality and maintainability
- Don't need to fix every single race condition or edge case if they're super rare unless the fix has zero tradeoffs
- Pay close attention to fixes that add complexity — the fix needs to be worth it
- Every change must earn its place; if the fix is worse than the original issue, skip it
- Leave the code in a better place than we found it

## Project skills (`.claude/skills/`)

| Skill | Use it to |
|---|---|
| `port-swift-module` | Translate a module from `apple/` into `video-coach-core` without repeating past mistakes |
| `verify` | Run fmt, clippy, tests and the core dependency audit before committing |
| `measure-media` | Benchmark GStreamer decode/seek on real hardware without fooling yourself |
| `adversarial-review` | Run the review pattern below on a spec, plan, or diff |

`.claude/` is committed; personal overrides go in `.claude/settings.local.json` (gitignored).

## Build + test conventions

### Rust port (primary)

The Linux port is the active codebase. Spec: `docs/superpowers/specs/2026-09-19-linux-port-design.md`.

```bash
cargo test -p video-coach-core     # pure logic -- needs NO GStreamer
cargo test --workspace             # everything -- needs GStreamer dev libraries
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

**Speech recognition needs `cmake` and `libclang-dev`** (`sudo apt install
cmake libclang-dev`). `video-coach-media` depends on `whisper-rs`
unconditionally — there is no feature gate, by decision — so without them
nothing builds but `video-coach-core`. With them, the first build spends
**about three minutes** compiling the vendored whisper.cpp, and nothing
afterwards.

**whisper.cpp's instruction set is pinned to x86-64-v3** — Haswell-class:
AVX2, FMA, F16C, BMI2 — by `.cargo/config.toml`'s `[env]`, so dev, test, CI
and the `.deb` all build the same library. `GGML_NATIVE=OFF` stops
`-march=native` (a CI runner with AVX-512 would build a binary that `SIGILL`s
on the laptop); the explicit `GGML_*=ON` flags are what survive
`SOURCE_DATE_EPOCH`, which otherwise switches off *all* SIMD. **After changing
any `GGML_*`, run `cargo clean -p whisper-rs-sys` and again with `--release`**
— cargo does not rerun the build script when `[env]` changes, and each clean
removes one profile's copy only. `packaging/build-deb.sh` checks the release
build it packaged and fails on anything but `-mavx2` without `-march=native`.

**Which model runs is the coach's, machine-wide.** The inspector's transcript
row has a picker (`base.en` / `small.en`, default `small.en`), remembered in
`state.json` and never in `project.json` — a `Preferences` field would be a
format change every existing project fails `store::read`'s version guard on.
Switching models leaves a job *transcribing* alone (a whisper cancel costs
~12 s of CPU) but preempts one still *downloading* (that stops within 100 ms),
which restarts on the new model; the queue behind it picks the new one up. `$COACH_CUTS_WHISPER_MODEL`
still beats the picker, which says so by going grey and showing the file that
variable names. `WhisperModel` in `video-coach-media/src/transcribe.rs`
carries each model's file name, size and **measured** sha256.

**The model downloads on first use, and only when the bus says it may.** It
lives in `$XDG_CACHE_HOME/coach-cuts/models/`; a job whose model is absent
downloads it first (`souphttpsrc ! filesink` to a `.part`, glib's sha256 of
the file, rename), as `TranscribeMessage::Downloading` and its own
inspector line. **Permission is `TranscribeKind::Whisper`'s `fetch`, never
the path** (the reasoning lives on that variant): `bus::whisper` sets it only
under the cache directory, and a model switch moves only a path that has one.
**No test may reach Hugging Face** — serve from `video_coach_media::fixtures::serve`.
The URL is pinned to a Hugging Face commit, not `main`.
The Transcribe button is the prompt ("Download 488 MB and transcribe"). A
cancel leaves the `.part`; every other failure deletes it; a failed download
drops the queue behind it. Downloads take turns process-wide, so a cancelled
one can't rename a `.part` its successor is writing.

**Transcription is asked for, never automatic.** `AUTO_TRANSCRIBE` in
`bus/transcribe.rs` is `false`: a preempted job restarts from zero, so a coach
recording faster than a job finishes would complete none of them. Measured:
`small.en` runs at **0.73x realtime** on the reference laptop
(`docs/superpowers/spikes/2026-09-21-whisper-throughput.md`).

**Recording always wins, and cancelling is not free.** Starting a recording
cancels the job in flight and puts its clip back at the **front** of the queue.
whisper only consults its abort flag once per encode and once per decode pass,
so a cancel costs **~12 s of CPU** — `Transcriber::drop` therefore cancels
*without* joining, because the bus thread used to block on it and froze Stop
Recording along with both deadlines.

**Whisper's progress percentage is nearly useless on a short clip.** It is
reported at the top of a loop advancing in <=30 s chunks and never reaches 100,
so a 20 s clip yields exactly one callback reading 0. The inspector shows an
elapsed clock and appends the percentage only once it moves off zero.

The whisper tests are **`#[ignore]`d**, because they need a model CI has
no copy of. Run them by pointing `$COACH_CUTS_WHISPER_MODEL` — the same
variable the app finds its model with — at one, and read the throughput line
off `--nocapture`:

```bash
COACH_CUTS_WHISPER_MODEL=~/.cache/coach-cuts/models/ggml-small.en.bin \
  cargo test -p video-coach-media transcribe -- --ignored --nocapture --test-threads=1
```

`--test-threads=1` is **not optional**: `--ignored` runs *only* the ignored
tests, and libtest would run them in parallel — two whisper contexts, ~1 GB
resident and sixteen threads on eight cores, which is not the machine the
throughput line describes.

**Running the app** (needs a display and GStreamer's runtime plugins incl.
`gstreamer1.0-gl`):

```bash
cargo run --release -p video-coach-app               # restores the last project
cargo run --release -p video-coach-app -- <folder>   # opens (or creates) a project there
```

The application ID is **`coach-cuts`** everywhere: the binary
(`target/*/coach-cuts`, via `[[bin]]` — the package is still
`video-coach-app`), the config directory, `packaging/coach-cuts.desktop` and
its icons, and the window's `WM_CLASS` / Wayland `app_id`, set by
`slint::set_xdg_app_id` in `main.rs` (only valid after `BackendSelector::select()`).

The app must run on Slint's **Skia OpenGL** renderer (it selects it and fails
loudly otherwise): that renderer is EGL on X11 and Wayland, and EGL is what
lets GStreamer import decoded frames without a CPU copy. It logs the decoder,
the caps entering `glupload` and the GL platform on every source load (`bus:
loaded …` on stderr); that line is the zero-copy diagnostic, and on the
reference laptop it reads `vah265dec` / `memory:DMABuf` / `egl`.
`scripts/linux-gate-check.sh` measures decode throughput. The last project
and the chosen speech model live in `$XDG_CONFIG_HOME/coach-cuts/state.json`;
point `XDG_CONFIG_HOME` elsewhere when testing so the real one isn't
touched. With
the monitor off (DPMS), playback slows unless run with `vblank_mode=0`
(BACKLOG #36).

**Packaging** — a `.deb` for Ubuntu 24.04 / Mint 22 (needs `cargo install
cargo-deb`, `cargo install cargo-about --features cli`, `dpkg-dev` and docker):

```bash
packaging/build-deb.sh                                           # → target/debian/coach-cuts_<version>_amd64.deb
packaging/smoke-test.sh target/debian/coach-cuts_<version>_amd64.deb
```

- **Build with the script, never bare `cargo deb`.** It first generates the crate
  licence notices with cargo-about, which the asset list ships. Its `accepted` list
  (`packaging/about.toml`) is a **tripwire**: a crate licensed GPL-2.0-*only* can't
  be combined with this AGPL program, so an unlisted licence fails the build.
  Review it before adding one; never add GPL-2.0-only.
- **`packaging/copyright` is hand-written** and covers what crate tools can't see:
  whisper.cpp, the prebuilt Skia (with VulkanMemoryAllocator) and the DejaVu fonts.
  A new statically linked C/C++ library or embedded asset needs a stanza there.
- **`Depends:` is `$auto` plus a hand-kept list** (`[package.metadata.deb]`,
  commented). `$auto` is `dpkg-shlibdeps`; without `dpkg-dev`, cargo-deb only warns.
  Anything loaded at run time — a GStreamer element, a `dlopen`ed library — must
  be added by hand.
- **The smoke test is the only proof of the dependency list.** It runs in a clean
  `ubuntu:24.04` container, because the laptop already has every dev package. It
  checks the libc floor, installs without Recommends, looks up every software-path
  element, and launches the app under Xvfb. A new element the code names goes in
  its list.
- **Build dependencies are one list**, `packaging/build-deps.txt`, read by both
  workflows and the README.
- **Releasing:** bump `version` in the root `Cargo.toml`'s `[workspace.package]`,
  commit, then `git tag v<version> && git push origin v<version>`.
  `.github/workflows/release.yml` fails a tag that isn't `v` + that version, gates
  on `rust.yml` (called whole, via `workflow_call`), builds on `ubuntu-24.04` (the
  libc floor) with `build-deb.sh` (which asserts whisper's `-mavx2`), smoke-tests,
  and attaches the `.deb` to a GitHub Release. To check the
  pipeline without releasing, `gh workflow run release.yml --ref <branch>` runs
  everything and publishes nothing but a workflow artifact — **but only once
  `release.yml` exists on the default branch**; GitHub refuses to dispatch it
  otherwise (`HTTP 404: workflow … not found on the default branch`). Until then,
  a temporary `push: branches: [<branch>]` trigger does the same job; the release
  job requires a tag ref, so a branch run cannot publish. The `package` job uses
  no build cache on purpose (a cached whisper build can outlive an `[env]` change).

**Crate layout:**

| Crate | Holds |
|---|---|
| `video-coach-core` | Pure logic: project format, playback timeline, zoom, stroke replay. |
| `video-coach-media` | GStreamer: source player, capture, export frame driver, overlay rasterizer. |
| `video-coach-app` | Slint UI, command bus, event layer. |
| `video-coach-harness` | Headless integration tests driven over the bus. |

**`video-coach-core` declares no media dependency** — not GStreamer, not an image
or font crate, not a feature that pulls one in. CI runs its tests on a runner
with no GStreamer installed, so adding one fails the build rather than passing
silently. If you need a media type in core, you need a different design.

**Bus contract — caller-captured timestamps.** Any command that lands in the
commentary event log carries its timestamp (and source-position anchor) as a
field, captured at the input event on the UI thread, never assigned by the bus
handler. Queue delay would reintroduce the drift that puts drawings behind the
ball on replay. Querying position on a running pipeline is the only direct
pipeline access permitted outside the bus task.

**Pixel work split.** GStreamer owns every full-frame pixel operation, on the
GPU. Rust owns the edit (which decoded frame lands at each output PTS) and the
vector overlay layer only. This is measured, not preferred — see
`docs/superpowers/spikes/2026-09-19-compositing-throughput.md`. Do not move
full-frame resampling into Rust.

**Decode path stays zero-copy, which needs `decodebin3` AND an EGL context.**
Use `decodebin3` (or `playbin3`) with the video stream selected by caps
(`video/x-raw(ANY)`), or an explicit `demux ! parse ! <hw decoder>` chain —
never `decodebin`. And the GL context must be **EGL**: on X11 GStreamer defaults
to GLX, where 1.24's DMABuf importer is unavailable and every frame is copied
through the CPU (~11× slower; seeks ~5× slower). In the app the EGL context is
Slint's Skia renderer's, shared with GStreamer.
Verify on real hardware with `scripts/linux-gate-check.sh <file>`; see
`docs/superpowers/spikes/2026-09-19-seek-latency.md`.

**Capture records on the system clock, from time 0 = `base_time`.**
- **Sources:** the camera is `v4l2src`, for kernel timestamps and the `exposure_dynamic_framerate=0` control that stops low-light drops to 7.5 fps. The mic is `pipewiresrc`.
- **Clock:** the recorder always forces `SystemClock` (CLOCK_MONOTONIC). `pulsesrc`'s clock was measured days off.
- **Time 0:** `matroskamux` writes running time as-is, so recording time 0 is the pipeline's `base_time`, read when `set_state(PLAYING)` returns. **Never wait for PLAYING:** the mux holds preroll until the camera's first frame.
- **Event times:** `host_ns` comes from `video_coach_media::now_ns()`.
- **Tests:** they use injected test sources (`CaptureKind::Test`) and never the real camera or mic. See `docs/superpowers/specs/2026-09-19-linux-port-phase-4-design.md`.

**Export runs one GL graph everywhere, on its own GL display.**
- **The graph:** decode (`decodebin3` → the player's `gl_bin` → pull `appsink`) → a Rust pump → `appsrc` → `gltransformation` (zoom) → `glvideomixer` (letterbox, pinned to 1920×1080@30) → NV12 `gldownload` → encoder → `mp4mux`.
- **The GL display:** process-wide and surfaceless (`GLDisplayEGL::new_surfaceless()`), never the UI's. CI has no GPU, so Mesa's llvmpipe runs the same graph; there is no software variant.
- **Picking source frames:** use **stream time** (`segment.to_stream_time`), not raw PTS: MP4 edit lists offset raw PTS. Round seconds to ns (`seconds_to_clock`). Seek `KEY_UNIT|SNAP_BEFORE`, then pull forward: ACCURATE seeks drop frames in VFR or gapped files.
- **Quality is a constant QP:** `vah264lpenc` is CQP-only.
- **Never block a push or pull without a bound.** A blocking `appsrc` push hangs forever after a downstream error.
- **To test CI's path locally,** hide the GPU with `GST_REGISTRY=<scratch>/reg.bin bwrap --dev-bind / / --tmpfs /dev/dri cargo test …`. See `docs/superpowers/specs/2026-09-19-linux-port-phase-5-design.md`.

**The speakers are `autoaudiosink` with `pulsesink` demoted.** `keep_pulsesink_out()` drops `pulsesink`'s rank process-wide at `Bus::spawn`, so `autoaudiosink` picks `alsasink`, which reaches PipeWire through `pipewire-alsa`. Against Ubuntu 24.04's `pipewire-pulse` (PipeWire 1.0.5), `pulsesink` wedged the stream permanently after a quick burst of flushing seeks while playing — a dragged scrubber or a held skip key — and since it supplies the pipeline clock, picture and position froze with it (journal: `pipewire-pulse … [coach-cuts]: stream … OVERFLOW`). Measured A/V offset is unchanged (~+1 ms, audio leading). **The test harness's `Harness::production()` runs the app's exact path** — the GL sink on a surfaceless display plus the real `autoaudiosink` — because the default harness (`fakesink` audio, silent WebM fixtures) can never reach the sound server, which is why this escaped. Real-footage checks are `#[ignore]`d: `COACH_FOOTAGE=/path/to/game.mp4 cargo test -p video-coach-harness --test real_footage -- --ignored --nocapture`.

**Transport keys: the arrows skip, `,` and `.` step one frame while paused** (`Command::StepFrame`, refused while playing, recording or previewing). A step works from the shown frame's *end*, which a seek never clips: forward seeks to it, back to half a nominal frame before the frame's nominal start. The readout shows tenths while paused, whole seconds while playing.

**Preview and export share one composite** (`video-coach-media/src/composite/`).
- **Common:** `decode.rs` (`Decoder::frame_at`: reuse, pull ≤0.5 s, else `KEY_UNIT|SNAP_BEFORE` and walk forward), the pump, `frame_time`/`stamp`, `install_zoom`'s PTS-keyed probe, and the mixer geometry.
- **Tails:** `export.rs` encodes as fast as it can on a private surfaceless display; `preview.rs` ends in a `sync=true` appsink filling the shared `FrameMailbox`, on **Slint's** GL context (chosen by sink kind: the app never falls back to a private display, and tests pass `Gl::shared()`).
- **Preview's pads:** the pumped source through `gltransformation`, the recording played **natively** for PiP and commentary audio (record time *is* output time, so it needs no pump), and a second appsrc carrying the overlay.
- **The overlay rasterizes at the picture rect,** not the output frame: strokes are normalized to the content rect.
- **Measured:** 30.005 fps on 1440p HEVC, audio leading the picture by 2–7 ms. Don't measure the rate first-frame-to-last-frame; the mixer flushes its tail late. The UI budget is relative to a scanning control in the same session, not to an idle window.

**Export burns in the overlay and mixes the audio** (Phase 8).
- **Layers:** the pumped source (zoom, per-entry fit rect), the webcam PiP, then one output-size overlay carrying strokes (mapped into the picture rect), the text bar's background and its glyphs. Pad rects are **PTS-keyed in probes**; set from the pushing thread they land up to `QUEUED` frames early.
- **The PiP pad is fed every frame,** with a **GL** 1×1 transparent filler when a clip has `show_pip` off or its recording is unusable. An unfed pad stalls the run, and a system-memory filler breaks `glupload` when a later entry has a real inset.
- **Audio:** one audio-only pipeline per file (flushing ACCURATE seeks per play segment, silence for a file with no audio), mixed in Rust from `core::audio`'s regions and envelope, pushed **at or ahead of** the video into an **unbounded** appsrc, then `avenc_aac` (needs `gstreamer1.0-libav`). **Drop the first 1024 samples** for the encoder's priming; shifting timestamps does nothing. A tone at 1.000 s must decode back within a millisecond.
- **Every denominator is `plan.total_frames()`,** never a duration sum: per-entry quantization can add a frame per entry.

**The match clock is the displayed frame's source time** (Phase 9).
- **Never a per-clip constant.** `ScoreboardContext::state_at(entry.source_index,
  frame.source_time)` is called per frame, with `source_time` coming from
  `FrameSpec` — not `timeline::source_time`, and nothing cached on `PlanEntry`.
  macOS computed the clock as a per-clip constant plus the commentary's wall
  clock, so every pause and skip pushed the clock ahead of the footage; since
  every recording opens with a pause, that was nearly always (BACKLOG #27).
  A clip that pauses reads the same match time either side of the pause, and
  `core`'s pause test pins it.
- **The absolute events are derived per job** and must never be cached across a
  source add, move, remove or relink — a relink can change a duration, and so
  every later offset.
- **Every scoreboard label is fitted** (shrunk to a floor, then ellipsized).
  `draw_label` centres and does not clip, so an unfitted label spills out of
  both ends of its cell. The columns are sized so nothing realistic shrinks;
  fitting is what makes a spill impossible rather than unlikely.

### Reference implementation (`apple/`, not maintained)

The macOS app is kept as the reference for behavior and invariants. It is **not
maintained in parallel** and is not built by CI. Read it to answer "what did the
original do?", not to change it. Several known bugs are deliberately left in it
(see `BACKLOG.md` #27); the port fixes them by construction.

- **Core package tests:** `swift test --package-path apple/VideoCoachCore`
- **App build:** the `.xcodeproj` is gitignored, regenerated from `apple/project.yml`. After creating any new file under `apple/App/**`:
  ```
  cd apple && xcodegen generate && cd ..
  xcodebuild -project apple/VideoCoach.xcodeproj -scheme VideoCoach -destination 'platform=macOS' build
  ```
- Core package files under `apple/VideoCoachCore/**` are auto-discovered by SwiftPM — no xcodegen needed.

## Architecture notes (reference implementation)

These describe `apple/`. The Rust port's architecture is in the spec above.

- **`VideoCoachCore`** (Swift Package) holds all pure logic: data model, clock semantics, custom AVFoundation compositor, export pipeline. Tested headlessly via `swift test`.
- **App target** (`apple/App/`) is SwiftUI + AppKit interop. Workspace is `@Observable @MainActor`; ContentView owns ephemeral UI state (`@State` + `@Binding` to children).
- **`Workspace` is project-data only** — never put pure UI mode flags on it. Inspector mode, modal-flow flags, etc. live on `ContentView` as `@State`.
- **Custom compositor lives on the export path only.** Preview playback uses AVFoundation's built-in compositor because macOS 26 strips custom-compositor instruction subclasses (`ClipPreviewBuilder.swift` documents this). Overlays in preview live as AppKit overlay views above `AVPlayerView`.
- **Project file is `project.json` under the project folder**, plus a `recordings/` subdir of `.mov` clips. `formatVersion` discipline: bump on every additive schema change; migration happens at decode time, never at save. (The Rust port starts at v7 and refuses anything lower.)

## Backlog

Carry deferred items in `BACKLOG.md` (worktree root). Format: numbered list under headings (Spec/plan corrections, Code follow-ups, UX gaps). Each entry includes "Why deferred" and "When to revisit."
