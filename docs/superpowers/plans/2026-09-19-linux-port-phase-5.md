# Linux Port — Phase 5 Plan (Passthrough Export)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-5-design.md` (decisions X1–X5)
**Status:** Reviewed. Simplification and correctness passes are applied; the correctness pass ran the fixtures, llvmpipe exports, the hang and the parallel exporters on the reference laptop.

**Execution.**
- Each task runs in a fresh subagent that is given this plan, the spec and `CLAUDE.md`.
- The orchestrator runs `verify` and commits per task.
- Every task must build the whole workspace and keep CI green.

**Known facts. Don't re-derive these.** Everything in the spec's "Measured facts", plus:

- **Prototypes to adapt, not copy blindly:**
  - `scratchpad/export-spike/`: the spike's bench, with `frame_at` and the context sync handlers;
  - `scratchpad/p5-spec-review/xb/`: the surfaceless display and the zoom probe;
  - `scratchpad/p5-plan-review/`: `hang.py`, `fix.py`, `av.py` and `seek.py`.

  `scratchpad` = `/tmp/claude-1000/-home-rajah-git-coach-cutups/e3d8d025-26c5-4369-a3ee-3c564dfbf895/scratchpad`.
- **Hangs and timeouts:**
  - A **blocking `appsrc` push hangs forever** after a downstream error, with the ERROR sitting on the bus (`hang.py`).
  - `try_pull_sample(long)` waits out its whole timeout after a decode error.
  - So the pump must never block without a bound (spec X4).
- **Fixtures:**
  - **Edit list:** `x264enc bframes=2 ! mp4mux` gives a `qtdemux` segment start of 0.0333 s, so it has an edit list.
  - **Robustness:** a binary block counter (≥32 px blocks) survives VP8 at 640×360, x264, `openh264dec` and the full GL graph on llvmpipe, with 0 bad frames.
  - **Speed:** generation takes about 1 s for 250 VP8 frames (`deadline=1`) and about 2 s for 600 frames at 60 fps.
- **Frame PTS:** frame `i` has PTS `floor(i·1e9/fps)`, for both webm ms timecodes and mp4mux timescale 6000. The pump uses `round(t·1e9)`.
- **Freeze clamp:** anchors near the end are clamped to `duration − 0.05` (`timeline.rs` `FREEZE_EOF_BACKOFF`).
- **llvmpipe is slow:** about 0.45 CPU-s per 1080p frame (7 s wall for 90 frames on 8 threads). The harness `TIMEOUT` is 15 s. **Keep test exports short:** at most about 90 frames each.
- **CI's path, locally:** `GST_REGISTRY=<scratch>/reg.bin bwrap --dev-bind / / --tmpfs /dev/dri cargo test …`
  - This hides VA, so `x264enc`, `vp8dec`/`openh264dec` and llvmpipe are used.
  - The separate registry keeps the user's cache untouched.
  - `LIBGL_ALWAYS_SOFTWARE` alone doesn't hide VA.
- **CI packages:** `libegl-mesa0`, `mesa-libgallium` and `libgl1-mesa-dri` are already pulled in as hard dependencies, so expect no CI change. CI runs only on PRs and `main`, so it hasn't run on this branch.
- **Parallel exporters:** 3 at once, each with its own surfaceless display, all fine. *Wrong, see Task 2 notes: the displays share one `EGLDisplay`.*
- **Long pauses:** a pump stalled for 40 s with both pipelines PLAYING is fine.
- **Audio in the source:** `decodebin3` with its audio pad left unlinked decodes the video correctly across seeks.
- **Player code:**
  - `player/mod.rs` declares `mod sink;` privately, so re-export `pub(crate) use sink::gl_bin;` and make `gl_bin` `pub(crate)`.
  - `seconds_to_clock` is `pub(crate)`.
  - `Diagnostics` and `diagnostics()` (`player/mod.rs`) should become a free `pub(crate)` function over `(pipeline, glupload)`, reused by export.
- **Dependencies:** `gstreamer-gl-egl` is an app-only dependency; media needs it, with `v1_24`.
- **`fixtures.rs`'s module doc** claims base and good plugins only; `x264enc` is ugly. Update it.
- **The recording guard** (`bus/mod.rs`) drops unlisted commands with only `eprintln!`. It doesn't send an error.

---

## Task 1 — Core: `frame_schedule`

`video-coach-core/src/export.rs`, per X1: `OUTPUT_FPS`, `FrameSpec { source_time, zoom }` and `frame_schedule(clip, source_duration)`.
- Walk `playback_segments` forward: output times only increase.
- The count is `ceil(total·30 − 1e-6)`.

**Tests** (`tests/export.rs`), as listed in the spec's Testing → Core.

Commit: `feat(core): export frame schedule`.

## Task 2 — Media: fixtures, and the export graph running end to end

1. **Fixtures:** `counter_video(path, w, h, fps, frames, kind)`, with `kind`:
   - `Vp8WebmWithAudio`: 25 fps, Opus or Vorbis audio, following `fixtures::webm`'s pattern;
   - `H264Mp4BFrames`: 60 fps.

   Frame `i` shows `i` as a binary counter of ≥32 px black and white blocks, pushed through `appsrc`.
   - `read_counter(frame)` thresholds the block centres.
   - `decode_counters(path) -> Vec<u32>`.
   - **Round-trip test** for both kinds, plus an assert that the MP4's `qtdemux` segment start is > 0.
2. **Module `export/`:**
   - **`Exporter::start(job, on_msg)`** owns a thread. On that thread it creates the surfaceless display and context, both pipelines (per X2, with the pinned caps), the encoder (probe `vah264lpenc`, then `x264enc`, with the X3 settings) and the zoom probe.
   - **Letterbox:** the fit rect comes from the decoded caps, including PAR.
   - **Decode:** `decodebin3` into `gl_bin` into a pull `appsink`. Non-video pads are left unlinked.
   - **The pump** (per X4, never blocking without a bound):
     - reuse, pull ≤ 0.5 s, seek;
     - stream time and `seconds_to_clock`;
     - `buffer.copy()` restamped to `n/30`;
     - `appsrc block=false` with a wait-for-room loop;
     - short pull timeouts;
     - each loop iteration checks cancel and pops both buses for errors.
   - **Messages:**
     - `Progress(u8)` whenever the whole percent changes;
     - exactly one `Finished(Result<ExportDone, ExportError>)`.
     - `ExportDone { path, encoder, diagnostics }`, reusing the player's `Diagnostics`.
     - `ExportError { Cancelled, Failed(String) }`.
   - **Output:** `.part`, renamed on success. A cancel or error deletes the `.part`.
   - **`Drop`** cancels and joins.
   - **Zoom mapping:** `zoom_params(zoom) -> (s, tx, ty)`, private, per the spec's measured mapping.
3. **Tests** (`tests/export.rs`). Each export is ≤ ~90 frames, for llvmpipe CI.
   - **Fiducial** on both fixtures:
     - **The clip:** plays, a freeze, skips, anchors off frame boundaries, and identity zoom.
     - **Check:** every output frame's counter equals the oracle's `max i : floor(i·1e9/fps) ≤ round(t_source·1e9)`, in integer arithmetic.
     - **Also assert on the same file:** 1920×1080, 30/1, the frame count, and `moov` before `mdat`.
   - **Letterbox + zoom:** one 480×360 (4:3) export of 2–3 frames.
     - The bar pixels are black.
     - With s=2 and a pan, a known block lands where `zoom_params` and the fit rect predict.
   - **Cancel:** put a file at the target path, then cancel from the progress callback at frame k. The result: `Finished(Err(Cancelled))`, no `.part`, and the original file unchanged.
   - **A mid-stream error doesn't hang:** a crate-internal unit test in `src/export/` inserts `identity error-after=N` before the encoder, through a `#[cfg(test)]` hook. It gets `Finished(Err(Failed))` within a bounded time.
4. **Local CI-path run:**
   - Run the export tests under `bwrap --tmpfs /dev/dri`, with a separate `GST_REGISTRY` (Known facts).
   - Record the timings in the Task 2 notes.
   - Add CI packages only if something is missing.
5. **Hardware check** (no camera):
   - Export 60 s of `~/Downloads/phone_Videos/20260502121738_000004.MP4` (read-only) to the scratchpad, with 2 s play / 1 s freeze segments.
   - Record the fps, the diagnostics line and the encoder in the Task 2 notes.
   - Delete the output.

Commit: `feat(media): passthrough export`.

### Task 2 notes

**Hardware** (reference laptop, 60 s of `20260502121738_000004.MP4` from 300 s, 20 × (2 s play, 1 s freeze), release build, two runs):
- 1800 frames in 16.5 s: **109 fps overall, 112–114 fps after the first 1%, 3.6× realtime**. The output is 57 MB (7.7 Mbps at QP 24), H.264 High, 1920×1080, 30/1, 60.000 s. It was deleted.
- `encoder vah264lpenc, decoder vah265dec, glupload caps video/x-raw(memory:DMABuf), format=DMA_DRM, 2560x1440, drm-format=NV12:0x0100000000000002, gl platform egl`.
- `GST_DEBUG=glupload:6`: 1209 `DirectDmabufExternal` imports (the ~1200 source frames decoded; freezes reuse), and no `Raw Data`.

**Tests.**
- **Hardware** (VA decode and encode): 7 integration tests in ~15 s wall, in parallel. Each 87-frame fiducial export takes ~1.0–1.3 s. All export tests (integration and unit) passed 3×.
- **CI path** (`GST_REGISTRY=<scratch>/t2-reg.bin bwrap --dev-bind / / --tmpfs /dev/dri`, `GL_RENDERER: llvmpipe (LLVM 20.1.2, 256 bits)`, `x264enc`, 8 threads):
  - With the tests running in parallel, the 87-frame fiducials take **11.5 s (vp8dec) and 12.5 s (avdec_h264)**; the 60-frame cancel 4.5 s; the 3-frame letterbox export 3.2 s. The whole binary takes 26 s wall and 116 s CPU.
  - With `GST_PLUGIN_FEATURE_RANK=avdec_h264:0` (CI has no libav), `openh264dec` decodes the H.264 fixture and output, and all pass.
  - The unit tests, including the mid-stream error, pass too.
  - No CI package change was needed. The test `TIMEOUT` is 120 s: a 4-core runner should take ~2–3× these times.

**Surprises. Both are corrected in the spec.**
1. **The zoom wasn't clipped.** Offered the choice by `glvideomixer`, `gltransformation` doesn't render. It passes the frame through with an affine-transformation meta, and the mixer draws the transformed quad unclipped, so a zoomed 4:3 source spilled into the bars (measured: white at x = 1724 with the bar starting at 1680).
   - The fix: a post-query probe on `gltransformation`'s src pad strips that meta from the allocation answer. It then renders into its own source-sized texture, which clips the zoom.
   - On that render path, **`translation-y` has the opposite sign**: `zoom_params` is `(s, −pan_x·s, −pan_y·s)`, in units of the picture's width and height. The spec's `+pan_y·s` was measured on the meta path. The letterbox test now checks the bars on the zoomed frames too.
2. **One surfaceless display per export breaks exports running side by side.** Every surfaceless `GLDisplayEGL` wraps the same `EGLDisplay` (the same handle, verified), and `GstGLDisplayEGL` calls `eglTerminate` when finalized.
   - With a display per export, the parallel tests failed as soon as one finished: `glupload` "Failed to upload buffer", "could not create an EGL context … EGL_SUCCESS", and one SIGSEGV.
   - The display and context are now **one per process**, in a `OnceLock` (a failure is cached too), and never dropped. Both pipelines stay on the export thread. With that change, the parallel runs passed 3/3. The plan-review "3 parallel exporters fine" result was luck.

**Smaller findings and deviations.**
- **`filesrc` linked before `location` is set posts an ERROR.** Linking sends a scheduling query, which starts the source. `fixtures::for_each_gray` and the decoder therefore set the location first.
- `gldownload` is followed by a pinned `video/x-raw,format=NV12`: the system-memory readback that was measured.
- The mixer's output caps also pin `pixel-aspect-ratio=1/1`.
- **The injection hook** is a private `Exporter::spawn(job, on_msg, inject)` rather than `#[cfg(test)]` state. `start` passes `None`.
- **`Exporter::start` returns `Result<Exporter, String>`.** It errs only for an empty schedule or a failed thread spawn; everything else arrives as `Finished`.
- There is no `Progress(0)`: the first message is `Progress(1)` or higher.
- **`error_text`** moved from `recorder.rs` to `lib.rs` for reuse. The player's `answer_need_context` now takes `(display, context)`, and `gl_caps()` is shared.
- **Media now depends on `video-coach-core`** (for `FrameSpec`/`Zoom`), with `uuid` as a dev-dependency.
- **Counter layout:** a 6 × 4 grid over the picture, bits in rows 1–2, and blocks at 60% of a cell. They are ≥32 px at 480×360 and scale with the picture, so `read_counter` needs no source size.

## Task 3 — Bus, harness, UI

1. **Bus:**
   - **Commands:** `ExportClip { id, path }` and `CancelExport`.
   - **Events:** `Event::Export(ExportStatus::{Running(u8), Done(PathBuf), Cancelled})` and `UserError::ExportFailed`.
   - **`Input::Export(msg)`** is a new input.
   - **Starting** (per X4):
     - refuse, with an error, if the clip or source is missing or an export is running;
     - `can_record()` refuses with `CantRecord("an export is running")`;
     - otherwise compute the schedule and start the `Exporter`.
   - **Finishing:** on `Finished`, drop the exporter (which joins), emit the outcome, and log `bus: exported …: decoder …, glupload caps …, encoder …`.
   - **Shutdown** drops the exporter.
2. **Harness** (`tests/export.rs`), testing the bus's own behavior; media covers the file contents:
   - `Running…`, then `Done(path)`, and the file exists;
   - `CancelExport` → `Cancelled`;
   - refusals: a second export, and a missing source;
   - `ToggleRecording` during an export is refused with `CantRecord`;
   - `ExportClip` during a recording is dropped, shown with the shutdown-barrier pattern (`tests/clips.rs`): no `Export` event, and no file.
3. **UI:**
   - **Clip context menu:** "Export video…" → an `rfd` save dialog (default `<name>.mp4` in the project folder, `*.mp4` filter) → `ExportClip`. It is disabled while exporting or recording.
   - **Record** is disabled while exporting.
   - **Progress:** an `export-progress` property (hidden when negative) drives a progress bar and **Cancel**.
   - **Outcome:** the bar hides on any terminal outcome (`Done`, `Cancelled`, or an `ExportFailed` error). `Done` shows the notice "Exported to <file name>".
   - **Screenshot pass:**
     - a scratch project with a fixture source and a clip in `project.json`; no camera;
     - driven by a temporary callback driver, with no input injection;
     - screenshot the progress bar and the notice;
     - delete everything.

Commit: `feat(app): export a clip`.

## Task 4 — Closeout

1. Adversarial review of the Phase 5 code diff; apply the fixes and backlog any deferrals.
2. **`CLAUDE.md`:** one paragraph on export:
   - it owns a surfaceless EGL display;
   - one graph everywhere, with llvmpipe on CI;
   - stream time and rounded ns;
   - CQP quality;
   - never block a push without a bound;
   - the `bwrap` recipe for testing the CI path locally.
3. The user's hands-on checklist items, in the Task 4 notes.

## Deliberately not in this phase

- Overlays, PiP, audio, compilations, the quality and resolution picker: Phase 8.
- More encoders and non-surfaceless EGL: #46.
