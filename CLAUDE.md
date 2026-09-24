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

**The project format reads `MIN_READABLE_FORMAT_VERSION..=CURRENT_FORMAT_VERSION`**
(`store.rs`; v7, the first the port wrote, onward). Below it is a macOS file
(`LegacyProject`), above it `TooNew`. See spec F in
`docs/superpowers/specs/2026-09-22-match-vision-design.md`.
- **Every phase that stores a new field bumps the version once**, even though
  serde ignores unknown keys: an older build would ignore them too, and drop them
  on its next save. The bump makes it refuse the file instead.
- **A field added to an existing struct is an `Option` or a `Vec` with a
  field-level `#[serde(default)]`**, which is exactly what an older file means. A
  new struct's fields get no default: a missing one is a malformed file. Never a
  field-level default on an `f64` or a `bool` (`project.rs`'s header).
- **Every bump comes with a test that every readable version still loads**
  (`project_format.rs::v7_to_v9_files_load_under_the_current_version`, and one
  per bump beside it).
- **v10 adds `Project.avatar` and `Clip.inset`.** `avatar` is the image's file
  name in the project folder and *is* avatar mode — `Some` means takes record
  commentary only, `None` means they record on camera, and there is no second
  flag to disagree with it. `inset` is what a clip was recorded with, defaulting
  to `Camera`, which is what every v7–v9 clip was. Both are additive, so the
  readable floor stays 7.
- **v11 adds `Preferences::last_export_scoreboard`** (the export sheet's
  Scoreboard picker, `None` = Default). A field added to `Preferences` takes
  **no** attribute: that container carries `#[serde(default)]` and fills from
  its hand-written `Default` impl, so a field-level one would be a second copy
  of the default.
- **The first save after an upgrade keeps `project.json.v<old>`**, once, never
  overwritten, so the older build can still be gone back to. It is copied to a
  temporary name and renamed, like `project.json` itself, so a failed copy
  leaves no backup to block the next try; never a hard link (exFAT and FAT
  have none).

**One avatar image per project, copied into the project folder.** It is
`<project>/avatar.<ext>` beside `project.json`, and `Project.avatar` holds its
**file name** — which *is* avatar mode (the format rules above).
- **Picking one is decode, copy, save** (`bus/project.rs::set_avatar`): the file
  is decoded first, so one that will not decode is refused before anything is
  copied; the bytes are then copied through a temp file and a rename in the same
  directory, as `store::write` writes `project.json`. Replacing one deletes
  **exactly the file `Project.avatar` names**, never an `avatar.*` glob over a
  folder the coach can also put files in. **Remove** deletes the project's copy
  alone — the coach's original is theirs.
- **`media::decode_still` is the one avatar decoder, and `media::avatar_drawn`
  the one rasterizer.** The pick validates with the decoder; the drawn pixels —
  cover-cropped, circle-masked, premultiplied — are made in one place and taken
  by the render, the Devices popover's thumbnail and the corner during a take
  alike, so a file that passes the pick cannot fail in an export, nothing has to
  keep a list of extensions in step, and the coach is never shown something the
  file won't get. The decoder runs
  `decodebin3 ! videoflip video-direction=auto`, which is what keeps an
  EXIF-rotated phone photo upright (`probe` *refuses* a rotated **source**
  instead: there a timeline and a stored aspect are at stake). One sample is
  pulled, so a multi-frame file yields its first frame and no error. The file
  dialog offers PNG and JPEG; what is accepted is what decoded.
- **GStreamer's `RGBA` is straight alpha and tiny-skia is premultiplied**, so
  the copy into the avatar's pixmap multiplies R, G and B by A — a memcpy leaves
  a cut-out PNG haloed. The pixmap is pre-scaled to the **square**
  `pip_rect(out_w, out_h, 1.0)` and masked **once, at open time** to the circle
  inscribed in it, so the per-frame draw is a plain blit. **Square whatever was
  picked, and the image cover-cropped into it** (scaled until its shorter side
  fills the box, then centred): what is drawn is always a circle, so a box of
  the image's own aspect would put that circle somewhere other than where the
  webcam inset sits — a 3:4 portrait's about 240 px above the corner at 1080p.

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
- **An avatar project records audio only.** `Project.avatar.is_some()` is the mode, so no camera is opened, no `video_%u` pad is requested on the mux, no H.264 encoder is chosen (a machine with neither VA-API nor `x264enc` still records commentary) and there is no self-view pipeline. `CaptureSources`' video side is `Option` on **both** arms, and `capture_sources`' early return for `CaptureKind::Test` honours the mode too — a branch written only into the `Devices` arm would leave every test recording with video whatever the project said.
- **`RecorderMessage::FirstBuffer` is the first-buffer gate** (not `FirstVideo`): a buffer reached the muxer, so there is a file worth keeping. It comes from the one pad there is — video where there is one, audio otherwise — and the start timeout says which was missing.
- **The `level` element carries two numbers and the recorder passes both on.** The meter draws `peak_db`; the avatar pulses on `rms_db`, because speech's 10–14 dB crest factor would peg a peak-driven inset while the export barely moved.
- **The corner during an avatar take is the project's image, pulsing.** It is the copy the Devices popover decoded (`media::avatar_drawn`, once per change of the file behind it — never at the start of a take, where a decode with a ten-second bound would sit on the UI thread the instant the coach presses R; and never `slint::Image::load_from_path`, since slint is built with no image decoder) and sized by the **live** estimator: `level`'s `rms_db` through `core::avatar::level_from_db`, smoothed at `dt = 0.1` — the message interval — by the same `core::avatar::smooth` the render uses at `1/30`. Previews and exports use the **rendered** estimator instead (RMS per output frame, read from the recording). Two estimators of one quantity, same constants, nothing persisted. `place-self-view` carries the level **and which kind of take it is**, and there is one placement rule per kind: a camera take is `self_view_rect` → `pip_rect_over_picture` exactly, an avatar take is `avatar_self_view_rect` → `avatar_rect(avatar_box(pip_rect_over_picture(picture, 1.0)), level)`, the render's own two functions. **The self-view's quiet timer is the camera's alone:** an avatar take has no frames arriving and must not be hidden by it.
- **Tests:** they use injected test sources (`CaptureKind::Test`) and never the real camera or mic. See `docs/superpowers/specs/2026-09-19-linux-port-phase-4-design.md`.

**Export runs one GL graph everywhere, on its own GL display.**
- **The graph:** decode (`decodebin3` → the player's `gl_bin` → pull `appsink`) → a Rust pump → `appsrc` → `gltransformation` (zoom) → `glvideomixer` (letterbox, pinned to 1920×1080@30) → NV12 `gldownload` → encoder → `mp4mux`.
- **The GL display:** process-wide and surfaceless (`GLDisplayEGL::new_surfaceless()`), never the UI's. CI has no GPU, so Mesa's llvmpipe runs the same graph; there is no software variant.
- **Picking source frames:** use **stream time** (`segment.to_stream_time`), not raw PTS: MP4 edit lists offset raw PTS. Round seconds to ns (`seconds_to_clock`). Seek `KEY_UNIT|SNAP_BEFORE`, then pull forward: ACCURATE seeks drop frames in VFR or gapped files.
- **Quality is a constant quantizer, because nothing else is on offer.**
  `vah264lpenc`'s `rate-control` enum has exactly one member on the reference
  driver (`gst-inspect-1.0 vah264lpenc`); `rate-control=cbr` and `=vbr` fail to
  parse. `bitrate`, `target-usage` and `b-frames` exist as properties and are
  measurable no-ops, and `trellis=true` **doubles** the file. So there is no
  bitrate target and no size ceiling — a busy passage costs what it costs.
  Low/Medium/High are **VA QP 30/26/22**, and `x264enc` gets **QP − 4**
  (26/22/18), which matches the VA encoder's SSIM within 0.001. `x264enc` also
  needs **`vbv-buf-capacity=0`**: it hands `bitrate`'s 2048 kbit/s default to
  libx264 as a VBV maximum even in constant-quality mode, which silently capped
  every software export at ~1.7 Mbit/s. The ladder and its bitrates are
  tabulated on `quantizers` in `composite/export.rs`; `the_quality_ladder_reaches_both_encoders`
  pins it.
- **Never block a push or pull without a bound.** A blocking `appsrc` push hangs forever after a downstream error.
- **To test CI's path locally,** hide the GPU with `GST_REGISTRY=<scratch>/reg.bin bwrap --dev-bind / / --tmpfs /dev/dri cargo test …`. See `docs/superpowers/specs/2026-09-19-linux-port-phase-5-design.md`.

**The speakers are `autoaudiosink` with `pulsesink` demoted.** `keep_pulsesink_out()` drops `pulsesink`'s rank process-wide at `Bus::spawn`, so `autoaudiosink` picks `alsasink`, which reaches PipeWire through `pipewire-alsa`. Against Ubuntu 24.04's `pipewire-pulse` (PipeWire 1.0.5), `pulsesink` wedged the stream permanently after a quick burst of flushing seeks while playing — a dragged scrubber or a held skip key — and since it supplies the pipeline clock, picture and position froze with it (journal: `pipewire-pulse … [coach-cuts]: stream … OVERFLOW`). Measured A/V offset is unchanged (~+1 ms, audio leading). **The test harness's `Harness::production()` runs the app's exact path** — the GL sink on a surfaceless display plus the real `autoaudiosink` — because the default harness (`fakesink` audio, silent WebM fixtures) can never reach the sound server, which is why this escaped. Real-footage checks are `#[ignore]`d: `COACH_FOOTAGE=/path/to/game.mp4 cargo test -p video-coach-harness --test real_footage -- --ignored --nocapture`.

**Transport keys: the arrows skip, `,` and `.` step one frame while paused** (`Command::StepFrame`, refused while playing, recording or previewing). A step works from the shown frame's *end*, which a seek never clips: forward seeks to it, back to half a nominal frame before the frame's nominal start, or its own start where that is earlier (a frame held long). The readout shows tenths while paused, whole seconds while playing.

**`J`/`L` set the scan speed** (`Command::ScanSpeed(ScanStep)`: the bus steps 1×–32×, while scanning only; any pause returns to 1×, so a recording starts at 1×). The player owns the rate, and **every scan seek carries it** (`pipeline.seek(rate, …)`, never `seek_simple`, whose 1.0 would drop it on the next scrub or skip). `set_rate` issues no seek: the bus does, through `load`, unless a seek still to be issued will carry it; after a pause it seeks to the frame on screen, not the position the picture trails at speed. Opening a preview returns to 1× with no seek. Every frame is decoded even at 32× and the scan sink's QoS (`max-lateness` 20 ms) drops what's late: measured on 1080p30 H.264, that showed 88 fps at 32× against key frames only's 16, with a tenth of the lag.

**Preview and export share one composite** (`video-coach-media/src/composite/`).
- **Common:** `decode.rs` (`Decoder::frame_at`: reuse, pull ≤0.5 s, else `KEY_UNIT|SNAP_BEFORE` and walk forward), the pump, `frame_time`/`stamp`, `install_zoom`'s PTS-keyed probe, and the mixer geometry.
- **Tails:** `export.rs` encodes as fast as it can on a private surfaceless display; `preview.rs` ends in a `sync=true` appsink filling the shared `FrameMailbox`, on **Slint's** GL context (chosen by sink kind: the app never falls back to a private display, and tests pass `Gl::shared()`).
- **Preview's pads:** the pumped source through `gltransformation`, the recording played **natively** for PiP and commentary audio (record time *is* output time, so it needs no pump), and a second appsrc carrying the overlay.
- **The overlay rasterizes at the picture rect,** not the output frame: strokes are normalized to the content rect.
- **Measured:** 30.005 fps on 1440p HEVC, audio leading the picture by 2–7 ms. Don't measure the rate first-frame-to-last-frame; the mixer flushes its tail late. The UI budget is relative to a scanning control in the same session, not to an idle window.

**Export burns in the overlay and mixes the audio** (Phase 8).
- **Layers:** the pumped source (zoom, per-entry fit rect), the webcam PiP, then one output-size overlay carrying strokes (mapped into the picture rect), the text bar's background and its glyphs. Pad rects are **PTS-keyed in probes**; set from the pushing thread they land up to `QUEUED` frames early.
- **The PiP pad is fed every frame,** with a **GL** 1×1 transparent filler when a clip has `show_pip` off or its recording is unusable. An unfed pad stalls the run, and a system-memory filler breaks `glupload` when a later entry has a real inset.
- **The avatar is that same inset pad, never the overlay.** An avatar clip's inset is the project's image: decoded and pre-scaled once, **uploaded to GL once** (the filler's own hop, for the filler's own reason) and pushed as a re-stamped header over the one texture per frame; preview, whose recording has no video pad to play, feeds the pad from an `appsrc` of its own. **The avatar's box is `AVATAR_BOX_RATIO` (0.75) of the webcam inset**, shrunk about the inset's bottom-right corner so it keeps that corner's margins and is simply smaller (the coach, 2026-09-23); `core::avatar::avatar_box` is the whole of it, one pure function on the avatar paths only, and retuning the size is that one constant. The pulse is the **pad's rect**, `core::avatar::avatar_rect(box, level)`, set in the PTS-keyed probe from one level per output frame built at job setup from the recording's own audio — never in the frame loop, where an audio decode would stall the pump — and **bounded by the entry**: the reader is asked for exactly the samples the entry's frames cover, so a two-second entry of an hour-long take reads two seconds. `avatar_rect` is exactly the rect it is given at level 1.0, which is what every non-avatar frame carries, so a camera export is unchanged to the integer. Both raster pads blend `blend-function-src-rgb=one`: what they carry is a premultiplied tiny-skia pixmap. **Measured, and the reason:** drawing the inset in the overlay costs 4.2–4.8 ms a frame against the 3.2 ms the whole overlay costs — `tiny_skia` has no sprite fast path (`the_avatar_blit_costs`, `#[ignore]`d in `media/tests/avatar.rs`).
- **`Clip::shows_camera_pip()` and `shows_avatar()` are the only readings of `show_pip × inset`.** Preview has **three** sites to the first — the launch string's branch, the pad's placement, `decodebin3`'s pad-added link — and they must agree or the mixer stalls. They take one answer, and it includes a **probe of the recording**: `show_pip` says the coach wants an inset, but an avatar take's file has no video track and neither has a webcam take whose camera died. Export probes before it opens a decoder and falls back to the filler; preview has no filler and asks for no pad at all.
- **Audio:** one audio-only pipeline per file (flushing ACCURATE seeks per play segment, silence for a file with no audio), mixed in Rust from `core::audio`'s regions and envelope, pushed **at or ahead of** the video into an **unbounded** appsrc, then `avenc_aac` (needs `gstreamer1.0-libav`). **Drop the first 1024 samples** for the encoder's priming; shifting timestamps does nothing. A tone at 1.000 s must decode back within a millisecond.
- **Every denominator is `plan.total_frames()`,** never a duration sum: per-entry quantization can add a frame per entry.
- **Chapters are a hand-written `chpl`** (`media/src/chapters.rs`), one per plan entry (`CompilationPlan::chapters`, none under two entries), spliced into the reserved `moov` by shrinking the `free` after it, on the `.part` before the rename. `mp4mux` has no `GstTocSetter`. A chapter starts at `start_frame / OUTPUT_FPS`, never at a duration sum. **Chapters never cost an export:** any layout problem found before the write (no room, no `moov`, a box that doesn't fit) keeps the file whole without chapters, and `bus: exported …` says why. Only an I/O error opening the file or in the positioned write itself fails it.
- **A reel's chapters are worded twice, and mark the periods** (`core::reel::reel_plan`). The chapter reads as prose — `Goal 3 — Rovers 2-1` — where the bar burned across the picture reads `3 / 6 | Rovers goal | 2-1`; the same split as the Match panel's `labelled_events` and the whole match's `chapter_events`, and the two are meant to differ. Where the goals cross a period the boundary is marked **on the chapter already there**, as a prefix (`Second half: Goal 3 — Rovers 2-1`): a reel's entries run back to back, so a chapter of its own would share an instant with the next goal's — a zero-length chapter in the MP4, and a goal dropped by the ten-second rule in the pasteable list. A reel's entries and chapters are therefore built together, unlike every other target's, because the chapter needs the goal its entry was cut around.
- **`ffprobe` is the chapter test's reader** (`qtdemux` doesn't read `chpl`), so `ffmpeg` is a **test-only** build dependency: the test fails without it, never skips, and the `.deb` doesn't depend on it.

**Every exported MP4 says what it is, in its header** (`core::metadata::file_tags` → `media/src/composite/tags.rs`). The words live in **core**, beside the chapter and caption wording, as one pure function of the project, the export target and a date; media only sets them on `mp4mux`'s `GstTagSetter` before `PLAYING`, on **both** renderers. `ExportJob::tags` carries them, and `FileTags::default()` is an untagged file.
- **What actually reaches the file, measured on GStreamer 1.24.2** (mux, then `ffprobe -show_format`): `title`, `comment` (the final score), `keywords` (both team names, which GStreamer joins `", "`) and `encoder` (`Coach Cuts <version>`) land in `moov/udta` and read back by name; `date` lands in `©day`, unpadded, as `2026-9-21`; **`description` lands only in the XMP `uuid` box** as `<dc:description>` — `mp4mux` writes no `desc` atom, and nothing else on this muxer carries a description. **`datetime` is ignored**: it sets neither a tag nor `mvhd`'s creation time, so the date is a `glib::Date`.
- **The date is the footage's, not the export's** (a user decision): the first source file's mtime, read in the bus (`source_date`) because the bus knows the paths and media has no business stat-ing files. Resolved in the local zone there and handed to core as a `CalendarDate`; no date available writes no date.
- **The tag merge mode is `Keep`.** The encoder pushes an `ENCODER` tag of its own ("x264") into the same muxer; `Keep` is what leaves ours standing.
- **Tags cost the copy no losslessness and the chapters no room** (measured). They are header boxes beside the tracks, so not a sample changes; and `mp4mux` grows the reserved `moov` to fit them rather than spending the sample-table headroom — with tags, without them, and with a 40 KB payload, `reserved-duration-remaining` came back identical and the `free` box after `moov` that `chapters::splice` eats into stayed exactly 842 bytes.
- **Where a tag can't be told the truth it is left out, never guessed.** No scoreboard — or a kick-off not tagged yet — means no `comment`, and no teams means no `keywords` and a title that says what the export is rather than inventing "Home v Away".
- **The export target's labels live in core too** (`metadata::{ALL_CLIPS_LABEL, REEL_LABEL, WHOLE_MATCH_LABEL, UNTITLED, clip_label, reel_label}`), because the sheet row, the file name and the title are the same words: renaming a target renames it in all three.

**The whole match in track mode is a stream copy, not an encode** (`media/src/composite/copy.rs`). `ExportJob::render` picks the renderer — `Render::Encode` carries everything only the encoder reads, so a copy can't be handed a resolution or a cue-drawing scoreboard — and `composite::export::run` branches on it once, at the top; the `.part`, the chapters, the rename and the delete-on-failure stay in `run`, shared. **There is no `concat`:** Rust owns the ordering, one source at a time, as the encode path's pump does — two `concat`s, one per track, switch source independently and deadlocked one run in three under load. How the copy does it — the re-based segments, the `async=false` sinks, the header-reading caps gate, the single wait and its memory bound — is in that module's header, which is the one place it belongs.
- **The gate is also `video_coach_media::can_copy`,** a header read per file that the bus asks before it chooses a renderer. Every refusal names the file and says to choose **Scoreboard: burned in**.
- **The scoreboard sidecar is `job.path.with_extension("srt")`** — `ExportJob::cues` as `core::cues::cues_to_srt`, written in `composite::export`'s `finish` **after** the rename, so it inherits the run's name cleaning and its `" (2)"` de-duplication and is the matching basename a player auto-loads. A failure is logged and reported as `ExportDone::sidecar: None`; a good `.mp4` is never thrown away over a text file, and a cancel, which never reaches the rename, leaves the last good export's `.srt` alone.
- **`cues` says what belongs beside the output, including nothing.** `Some(cues)` writes them, `Some(empty)` writes none **and removes a stale one**, and `None` is a target that carries no sidecar at all, whose `.srt` is the coach's own file and no export's business. `write_sidecar`'s doc says why the removal is not optional.
- **The chapters also go beside the file as pasteable text**, `job.path.with_extension("chapters.txt")` — `core::chapters::chapter_list` of `CompilationPlan::chapters`, written in the same `finish` as the `.srt` and so with the same name cleaning, `" (2)"` de-duplication, after-the-rename timing and never-fatal failure (`ExportDone::chapter_list`). **A YouTube upload can't read `chpl`**, so the list a coach pastes into the description is the only way those chapters reach a video anyone watches there. **Every target that has chapters gets one**, not just the whole match. There is no "leave the path alone" case as `cues` has: `.chapters.txt` is a name of ours, so a run with no list to write **removes** the stale one.
- **YouTube ignores the whole list silently if any rule is broken**, so `chapter_list` bends the app's chapters to them and says no when it can't: the first line is exactly `0:00` (a first chapter under 10 s in is **moved** there, keeping its own words; 10 s or more in gets a `0:00 Start` line above it), consecutive chapters are at least 10 s apart (a later one inside that is **dropped**, never merged — merging invents a title neither had), times are floored `m:ss` / `h:mm:ss` and ascending, and titles are flattened to one line (a newline would cost every chapter, not just its own). **Fewer than three survivors writes no file at all**: a two-line list is not a shorter list, it is one YouTube ignores, leaving loose timestamps in a description with no hint why.
- **The same cues also ride *inside* the copy, as a `tx3g` track** — the sidecar is what VLC loads without being asked, the embedded track is what survives the file being sent on. It is a third `mp4mux` pad (`subtitle_%u`, `trak-timescale=1000`) fed from an `appsrc` of `text/x-raw,format=utf8`, **requested only when there are cues** and **written whole before the first source is opened**, then ended: a requested pad that runs dry stalls the muxer, so the track is never trickled. **`mp4mux` writes an empty sample between cues** (3,400 cues → 6,799 samples); that is the muxer's, not ours. Only the copy carries it — the encoded path is unchanged.

**The export sheet's third picker is Scoreboard: Default / Burned into the picture / Separate track,** carried by `Command::Export`'s `scoreboard: Option<ScoreboardMode>` (`None` is Default) and remembered in `Preferences::last_export_scoreboard` (v11) by the same write-back as the other two.
- **"Default" means the best available, never a silent trade.** It copies the whole match when `can_copy` agrees and burns the board in otherwise — a project of Matroska or HEVC sources re-encodes as it always did rather than failing at a gate the coach never asked for. Choosing **Separate track** by hand refuses instead, naming the file: there the coach asked for the copy. The sheet's one explanatory line follows the *effective* mode, so Default says so too.
- **Only the whole match carries the board beside the file.** A clip or a reel asked for on a separate track **burns it in** — it re-encodes either way, and the picker must never lose the board.
- **The whole mapping is `carry_scoreboard` in `bus/export.rs`** — mode + target into the renderer and the two job fields that carry the board. **Track mode blanks `job.scoreboard`** rather than carrying a mode flag into media: `None` is already media's one "don't draw the board", so there is no third state to keep consistent and `overlay.rs` never learns a picker exists.
- **The run's rate window is cleared when a target finishes** (`Active::finish_target`), along with the rate the event carries. A copy runs at thousands of output frames a wall second against an encode's tens, so the clips queued behind one would otherwise inherit its rate and be promised they finish at once.

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

**Match events are tagged at the playhead or typed, in one grammar.** `z` /
`x` / `v` tag where the game video is; the editor sheet ("Edit events…" in the
Match panel) takes the same events as lines — `2 14:05 home goal` — in a row's
field and in its paste box alike, both read by `core::match_entry`
(`parse_line`, `parse_batch`, `format_line`, `edit_from_line`).
- **A time has a colon, and a leading bare integer is a video number** — never
  a time. `900 home goal` is refused as *"there is no video 900"*, because a
  bare number read as seconds puts an event minutes out in silence.
- **Kick-off words are refused, not read as period boundaries.** `kickoff`,
  `kick`, `ko`, `restart`, `whistle` each get a refusal naming the damage:
  `interpret` is positional, so one spurious start/stop moves every later
  period and the clock burned into every export. `kickoffs.txt` is by
  definition a list of *restarts*, and pasting it must add nothing.
- **The sheet's rows are rebuilt by its own committed edit and never from
  `show_project`** (`main.rs`'s `editor_rebuild` flag): a transcript landing
  (`bus/transcribe.rs`) and a source found missing (`bus/transport.rs`) both
  publish `ProjectChanged` with no command behind them, and a `LineEdit` inside
  a `for` can only be bound one way — a rebuild would overwrite what is being
  typed. Every commit drops focus, because `for` reuses its items by index and
  a re-timed event moves.
- **A sheet with fields folds its `editing` into the window's `text-editing`**
  (`app.slint`), or Esc never leaves the field: the sheet's key guard tests
  `!text-editing`, so without the fold the first Esc closes the sheet and
  throws away a half-typed paste.

**The goals reel** (`video-coach-core/src/reel.rs`, spec R).
- **It is an `ExportTarget` (`Reel`), never a clip.** Its entries have
  `clip_id: None`, so its PiP is the GL filler and its audio is the game's alone.
- **It holds confirmed goals only** (every goal match event), in match order.
- **Each entry is one `Play` segment**, `[goal − lead-in, goal + tail]` on the
  goal's source, clamped to the source and to the previous entry's end on it. A
  goal at or before that end makes no entry of its own: it extends that one.
- **The defaults are 20 s and 6 s** (`REEL_LEAD_IN`, `REEL_TAIL`), overridden per
  side by the goal's trim. Never replace them with a guess that could be
  shorter: a cut-off assist is the one failure the reel must not have.

**Player highlights** (`video-coach-core/src/highlight.rs`, spec H).
- **A highlight belongs to the footage, not to a clip.** It is stored on the
  project (`Project.player_highlights`, v9) and keyed by `source_index` and the
  **displayed frame's stream time** (`Frame.stream_time`), never by record time,
  so it shows wherever that footage does — scanning, recording, a preview, every
  clip export that crosses it, and the reel — and it freezes with the footage
  through a commentary pause. Two keys on one frame are the same number, so a
  key replaces another by exact equality and no tolerance is stored or needed.
- **It is drawn from `highlight_shapes`**, the one piece of drawing geometry,
  which maps source-normalized rects through `Zoom::transform` — the affine the
  picture itself is drawn with. No new mapping function, in either the media
  overlay or the live Slint layer. Strokes stay zoom-agnostic (they live in the
  content rect); a highlight lives in source space and moves with the zoom.
- **The shape carries the label too** — its font size, the pill's height
  (`LABEL_PILL_RATIO`) and the pill's y, above the box or below it — and
  `highlight::label_ink` says whether the number is black or white. A drawer
  decides only how *wide* the pill comes out, because only a drawer shapes
  text; `app.slint` takes the rest as properties rather than repeating the
  ratios. The ring is stroked at `layout::STROKE_LINE_WIDTH`, the **one** pen
  width, which the live stroke layer and a logged `Stroke` also take from
  there.
- **The live ring is placed on the displayed frame**, `main.rs`'s
  `shown_position` — the same frame a key is placed on and "Delete key here"
  offers — not on `project.locate`. That is what makes the ring on screen the
  ring export burns in (spec H6).
- **A highlight may be placed outside a recording**, while pen drawings stay
  recording-only: a highlight describes the footage, a drawing the commentary.

**Match analysis is dB over a rolling median, never a level** (P3 — the
measurement phase: it stores nothing, suggests nothing and adds no command).
- **Core owns the maths, media owns the decode.** `core::signals` is
  `whistles` / `cheers` / `cheer_excess`, pure functions over a slice of
  16 kHz mono samples on 32 ms windows at a 16 ms hop.
  `media::analyze::audio::samples` is the **third caller of the export's own
  `Reader`** (`composite::audio::read_all`, which transcription now shares) and
  adds no decode path. A whole half is 27 MB of `f32`, which is what lets every
  rule stay a pure function of a slice.
- **Nothing absolute can work.** Venue gain differs by ~27 dB between the three
  tagged matches, so every threshold is dB over a **60 s rolling median of the
  signal's own band**, computed from a half-dB histogram — sorting each span
  over a hundred thousand windows costs minutes.
- **A whistle needs three terms**: level over that median, a ±150 Hz pitch
  hold, and **tonality**, the peak bin over the median of the other bins in the
  same window. Most broadband sound fails the pitch hold on its own — a noise
  burst's loudest bin hops. Tonality is what rejects sound that is broadband
  *and* steady (a horn, a buzzer), and the horn fixture in
  `core/tests/signals.rs` is the test that fails without it.
- **The picture is `core::motion` and `core::kickoff`**, read off the same
  decode: five frames a second of mean absolute luma difference, and one 32x18
  thumbnail a second. **Stillness is a quantile of the half's own motion**
  (`still_theta`), never a level — the median motion of a half runs 16–19 in two
  of the three venues and 4–8 in the third. On this footage the threshold has to
  land near the **median** (the camera's motion is bimodal), and the spec's
  "then motion above θ for 3 s" has to be read as the *median* of those 3 s: the
  literal reading found **0 candidates in 6 halves**.
- **P3's verdict is `docs/superpowers/spikes/2026-09-24-match-vision-measurements.md`,
  and it is negative. Read it before touching any of this.** Held out, the whole
  rule finds **7 goals of 9 with 29 false ones**, whose windows cover **57% of
  the match** — against a chance recall of 0.63, so the lift is **+0.15**.
  Periods are worse (start 0.50/0.50, end 0.00): the period whistles are audible
  and detected, but nothing tells them from the 41–85 other whistles in a half,
  not duration and not loudness. The cheer is the one real cue (8 of 9 held-out
  goals at 24 firings a half). **P4 is not justified and is not started**;
  nothing here is wired to the bus, the format or the UI.
- **Measured on the three tagged matches** (2026-09-24, release): the whole
  `Analyzer` is **67–84 s per file, ~24x realtime**, well inside G4's 5-minute
  bar.
- **The measurement run is `#[ignore]`d and needs `--release`** — an
  unoptimised Goertzel bank is about forty times slower:
  ```bash
  COACH_GROUND_TRUTH=B=<folder>:A=<folder>:C=<folder> \
    cargo test --release -p video-coach-harness --test ground_truth -- \
      --ignored --nocapture --test-threads=1
  ```
  **One run, not two** — sound and picture are scored together, off one
  `Analyzer` pass per source. The coach's folders are **the only copy of the
  footage and are read-only**:
  `store::read` plus a `kickoffs.txt` read, never a `Bus`, never a write. CI
  never sees them; every unit test is synthetic. **Nothing identifying goes in
  the repo or a pasted report** — no club, opponent, player, file or folder
  name. The matches are A, B and C.

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
