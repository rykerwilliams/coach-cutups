# Linux Port — Phase 5 Plan (Passthrough Export)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-5-design.md` (decisions X1–X5)
**Status:** Draft, pre-review.

**Execution.**
- Each task runs in a fresh subagent that is given this plan, the spec and `CLAUDE.md`.
- The orchestrator runs `verify` and commits per task.
- Every task must build the whole workspace and keep CI green.

**Known facts. Don't re-derive these.** Everything in the spec's "Measured facts", plus:
- **Prototypes.** The spike's bench (the pump, `frame_at`, the context sync handlers) is in `/tmp/claude-1000/-home-rajah-git-coach-cutups/e3d8d025-26c5-4369-a3ee-3c564dfbf895/scratchpad/export-spike/`. The review's copy with `GLDisplayEGL::new_surfaceless()` and the zoom probe is in `…/scratchpad/p5-spec-review/xb/`. Read them; they are working code to adapt, not to copy blindly.
- **Player code to reuse.**
  - `player/sink.rs` `gl_bin` is private; make it `pub(crate)`.
  - `player/mod.rs` `seconds_to_clock` is `pub(crate)`.
- **Dependencies.**
  - `gstreamer-gl-egl` is a dependency of the app only; media needs it with feature `v1_24`.
  - Workspace gstreamer crates are pinned to `v1_24`.
- **CI** (`.github/workflows/rust.yml`) installs plugins-base, good, bad, ugly and `gstreamer1.0-gl`. It has no libav. Mesa EGL/DRI may be missing.
- **Bus.**
  - The recorder's pattern: an `Input::Recorder(generation, msg)` with the generation captured at the call site.
  - The recording guard's allow-list is at the top of `Bus::command`.
  - Notices are `UserError::is_notice()`.

---

## Task 1 — Core: `frame_schedule`

`video-coach-core/src/export.rs`, exactly per X1: `OUTPUT_FPS`, `FrameSpec { source_time, zoom }`, and `frame_schedule(clip, source_duration)`.
- The count is `ceil(total·30 − 1e-6)`.
- Each frame binary-searches the flat segment list.

**Tests** (`tests/export.rs`), as listed in the spec's Testing → Core.

Commit: `feat(core): export frame schedule`.

## Task 2 — Media: fixtures and the export graph

1. **Fixtures** (`fixtures.rs`): `counter_video(path, fps, frames, format)`.
   - It generates a 640×360 video whose frame `i` shows `i` as a binary counter of black and white blocks, each at least 32 px. Build it with `appsrc` pushing raw frames.
   - Formats:
     - `Vp8Webm`, at 25 fps (`vp8enc ! webmmux`);
     - `H264Mp4BFrames`, at 60 fps (`x264enc bframes=2 ! mp4mux`).
   - Plus a `read_counter(frame) -> u32`, which thresholds the block centres, and a helper that decodes a file and returns every frame's counter.
   - Test the round trip of both fixtures.
   - Check that the MP4 fixture's `qtdemux` segment start is non-zero. If it isn't, note it in the task notes and find a way to produce an edit list; if that fails, record it.
2. **`export/` module:**
   - **GL:** the surfaceless EGL display and a context, shared through bus sync handlers on both pipelines.
   - **Decode:** `filesrc ! decodebin3`, with the video pad selected by caps, into `gl_bin`, into a pull-mode `appsink`.
   - **Encode:** the graph, per X2, with the pinned caps after the mixer.
   - **Letterbox:** the mixer pad's fit rect from the decoded caps (including PAR).
   - **Zoom:** the mapping as a pure `zoom_params(zoom) -> (s, tx, ty)`, applied by a buffer probe keyed on PTS.
   - **The pump:** reuse, pull ≤ 0.5 s, seek; stream time; `seconds_to_clock`; `buffer.copy()` restamped to `n/30`.
   - **Encoder:** probe `vah264lpenc`, then `x264enc`, with the X3 settings.
   - **Output:** `.part`, then rename on success.
   - **API:**

     ```rust
     pub struct ExportJob { pub source: PathBuf, pub schedule: Vec<FrameSpec>, pub output: PathBuf }
     pub fn export(job: ExportJob, control: &ExportControl, progress: impl FnMut(usize /*frames*/, usize /*total*/)) -> Result<ExportReport, String>
     // ExportControl: cancel flag + pause gate (Condvar/AtomicBool), shared with the caller.
     // ExportReport: decoder, encoder, upload path — for the log line.
     ```

     `export` is synchronous; the bus runs it on a thread.
   - **Cancel:** stop, set both pipelines to NULL, delete `.part`, return `Err("cancelled")` or a typed error. There is no EOS.
   - **Errors:** check the encode bus for errors between pushes, and fail fast.
3. **Tests** (`crates/video-coach-media/tests/export.rs`):
   - `zoom_params`, as a pure test;
   - **the fiducial test** on both fixtures, per the spec: every output frame's counter equals the index the schedule predicts. Build a `Clip` with plays, a freeze, skips and off-boundary anchors, and compute the expected index from `frame_schedule` and the fixture's fps;
   - output shape: 1920×1080, 30/1, the frame count, `moov` before `mdat`;
   - a 4:3 letterbox: the pixels at the edges are black and the content is inside the fit rect;
   - a zoom check: one frame with s=2 and a pan shows the expected quadrant, by sampling a fixture region;
   - cancel mid-export: no `.part`, no output, and a pre-existing file at the path survives;
   - an encoder error doesn't hang (e.g. an output path in a read-only dir, or an injected bad encoder property).
4. **CI:** add the Mesa EGL/DRI packages the llvmpipe path needs (`libegl-mesa0 libgl1-mesa-dri`, or whatever the runner lacks), plus `LIBGL_ALWAYS_SOFTWARE=1` only if needed. Verify locally with `LIBGL_ALWAYS_SOFTWARE=1 env -u DISPLAY cargo test -p video-coach-media --test export`.
5. **Hardware check** (the laptop, no camera):
   - Export a 60 s schedule of the user's HEVC file (`~/Downloads/phone_Videos/20260502121738_000004.MP4`, read-only) to the scratchpad.
   - Record the fps, the upload path (`GST_DEBUG=glupload:4`, or `ExportReport`) and the encoder in the Task 2 notes.
   - Delete the output.

Commit: `feat(media): passthrough export graph`.

## Task 3 — Bus and harness

1. **Commands:** `ExportClip { id, path }` and `CancelExport`. `CancelExport` goes on the recording allow-list.
2. **Events and errors:** `Event::Export(ExportStatus::{Running(f64), Done(PathBuf), Cancelled})` and `UserError::ExportFailed(String)`.
3. **Starting an export:**
   - **Refused** if the clip or its source is missing, or an export is already running. Recording is refused by the guard.
   - **Otherwise:** compute the schedule, spawn a thread running `export`, and hold `{ job_id, control, handle }`.
   - **Messages** arrive as `Input::Export(job_id, msg)`, and those from a stale job are dropped.
   - **Progress** is emitted when the whole percent changes.
4. **Pause while recording:** the bus sets the job's pause gate when a recording becomes Active and clears it at Idle.
5. **Cancel and shutdown:** cancel, join, `Cancelled`.
6. **Log line:** on `Done`, log `ExportReport` (`bus: exported …: decoder …, encoder …, upload …`).
7. **Harness tests** (`tests/export.rs`, using the fixtures):
   - export → `Running` → `Done`, with the file present and the right frame count;
   - cancel → `Cancelled`, with no file;
   - `ExportClip` during a recording → refused;
   - a recording started during an export pauses it: progress stops while recording, then the export completes.

Commit: `feat(app): clip export on the bus`.

## Task 4 — UI

- **Clip context menu:** "Export video…" → an `rfd` save dialog (default `<name>.mp4` in the project folder, `*.mp4` filter) → `ExportClip`. It is disabled while an export runs or while recording.
- **Progress:** an `export-progress` property (hidden when negative) drives a progress bar and a **Cancel** button in the transport.
- **Completion:** `Done` → a notice "Exported to <file name>". `Cancelled` → the bar hides.
- **Screenshot pass:**
  - Use a scratch project with a fixture source and a clip written into `project.json`. No camera.
  - Drive the export through a temporary callback driver; don't inject input.
  - Screenshot the progress bar, then the notice. Delete everything.

Commit: `feat(app): export a clip from its context menu`.

## Task 5 — Closeout

1. Adversarial review of the Phase 5 code diff; apply the fixes and backlog any deferrals.
2. **`CLAUDE.md`:** one paragraph on export:
   - the surfaceless EGL display it owns;
   - one graph everywhere, with llvmpipe on CI;
   - stream time, and rounded ns;
   - CQP quality.
3. The user's hands-on checklist items for Phase 5, in the Task 5 notes.

## Deliberately not in this phase

- Overlays, PiP, audio, compilations, the quality/resolution picker: Phase 8.
- More encoders: #46.
