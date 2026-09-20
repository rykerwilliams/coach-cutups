# Linux Port — Phase 8 Plan (Full Export)

**Date:** 2026-09-20
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-8-design.md` (decisions E1–E8)
**Status:** Draft, pre-review.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the whole workspace.

**Known facts. Don't re-derive these.** The spec's "Measured facts", plus:
- **Prototypes to read** (in `/tmp/claude-1000/-home-rajah-git-coach-cutups/e3d8d025-26c5-4369-a3ee-3c564dfbf895/scratchpad/`): `p8-spec-review/{fourpad.py,fourpad_probe.py,pipstarve.py,never.py,aac.py,moov.sh,fourpad_bench.sh,textcheck/}` and `p8-research/{bench.sh,audio.sh,textbench/}`.
- **Benchmarks need a real trim.** `glvideomixer` ignores `identity eos-after=N` and runs to the demuxer's segment end; always assert the output's duration.
- **Existing code:** `composite/mod.rs` (`head`, `install_zoom`'s PTS-keyed probe, `stamp`, `fit_rect`, `place`, `Gl`, `QUEUED = 4`, `wait_for_room`), `composite/export.rs` (the single-pad tail, `OUTPUT_WIDTH/HEIGHT`, `QP`), `composite/preview.rs` (three pads, the cursor and generation, `link_recording`), `overlay.rs` (strokes at the picture rect), `core/{export.rs,plan.rs,layout.rs}`, `bus/export.rs`, `ui/app.slint`'s inline export progress.
- **CI** installs plugins base/good/ugly/bad and `gstreamer1.0-gl`, but **not** `gstreamer1.0-libav`, which `avenc_aac` needs.
- **The font** is vendored into `video-coach-media` (DejaVuSans, 741 KiB, permissive licence, with its licence file).

---

## Task 1 — Core: the compilation schedule and the text line

1. `ExportTarget::Clip(Uuid)` beside `AllClips` and `Tag`.
2. `compilation_schedule(project, target) -> Compilation`, built **on `compilation_plan`**, with entries quantized to whole output frames.
   - `Compilation { frames: Vec<FrameSpec>, entries: Vec<Entry> }`.
   - `FrameSpec { entry, source_time, zoom }`; `source_index` and `record_time` derive from the entry and frame index.
   - `Entry { clip_id, source_index, recording, start_frame, frames, text, show_pip }`.
   - `text` is `"<n> / <total> | <name> | tags"` with empty parts collapsed; `<total>` is the target's clip count.
3. **Delete `frame_schedule`** and update its callers (`composite/export.rs`, `composite/preview.rs`, `bus/export.rs`, `bus/preview.rs`) to the one-entry compilation.
4. Tests: entry order and quantization, derived record time, a tag target, an empty target, a single-clip target, and the text line.

Commit: `feat(core): compilation schedule`.

## Task 2 — Core: audio splice, ramps and the export run

1. The audio maths, pure:
   - the game track's regions from the schedule's play segments (freezes silent), and the commentary's region per entry;
   - **5 ms linear fades at the start and end of every contiguous region on either track**, clamped at t=0;
   - gains from `preview_source_volume` / `preview_commentary_volume`;
   - an API that suits block-at-a-time mixing: given an output frame range, what does each track contribute?
2. `ExportRun`: targets with frame counts, a trailing-window rate, remaining time and a finish time, with the **stability gate** (≥5 samples and ≥2 s) and **no** monotonic clamp.
3. Tests: sample counts, the ramp envelope, a region shorter than a ramp, the t=0 clamp, silence during freezes; and the rate gate and projection (port macOS's cases).

Commit: `feat(core): export audio splice, ramps and run projection`.

## Task 3 — Media: the three-pad export tail

1. Rework `composite/export.rs` to the spec's pads: base (per-entry fit rect and caps), PiP, overlay.
   - **Geometry is keyed to PTS in pad probes**, never set from the pushing thread.
   - **The PiP pad is fed every frame:** the entry's recording through a `Decoder`, a 1×1 transparent RGBA when `show_pip` is off or the recording is unusable, and `repeat-after-eos` so a short recording holds its last frame.
   - One `Decoder` per distinct source for the run; the recording's decoder opens and closes per entry.
2. `overlay.rs` renders at the **output size**: strokes mapped into the entry's fit rect (line width from the picture's height), plus the bar (background and glyphs).
   - cosmic-text with the vendored TTF, `Wrap::None` and a tail ellipsis.
   - `layout.rs` gains the bar's rect and the PiP's raised margin (`bar height + margin`).
3. Resolution and quality parameters (720p/1080p, QP 28/24/20).
4. Tests:
   - **the multi-clip fiducial** (two counter fixtures, different sizes and frame rates, every frame's counter checked);
   - the three-pad composite over a synthetic base: the PiP above the bar, strokes in the fit rect on a non-16:9 entry, the bar, premultiplied alpha;
   - a long text line clipped with an ellipsis;
   - `show_pip` off, and a recording shorter than its entry;
   - the output's duration matches the schedule.

Commit: `feat(media): compilation export composite`.

## Task 4 — Media: audio into the file

1. One audio-only pipeline per source video and per recording, ending in an appsink at F32LE/48k/2ch; each play segment seeks its source's audio pipeline.
2. The Rust mixer pushes blocks **interleaved with the video pump**, bounded a little ahead, never after the last frame.
3. `audioconvert` → `avenc_aac bitrate=192000` → `aacparse` → `mp4mux`, with the mixed stream's timestamps **shifted earlier by the encoder's 1024-sample delay**, clamped at zero.
4. CI: add `gstreamer1.0-libav`.
5. Tests:
   - **the tone test:** a tone at a known time decodes back within a millisecond;
   - the gate and the ramps over a fixture whose tracks are distinguishable;
   - an export with audio still matches the schedule's duration.

Commit: `feat(media): export audio`.

## Task 5 — Preview keeps matching export

1. Preview draws the text bar with `n / total = 1 / 1`.
2. Preview plays the game audio through the same mixer, at the same volumes and ramps, alongside the commentary.
3. Re-measure the Phase 7 gate (30 fps, the A/V offset, the UI budget) and record it in the notes; the audio path is new, so the earlier numbers don't carry over.

Commit: `feat(media): preview gains the text bar and game audio`.

## Task 6 — Bus and UI: the export sheet

1. **Bus:** compilation exports; the target list (All clips, each tag with its count and length, the selected clip); progress as exact frame counts; cancel leaving finished targets alone; a missing source or recording refused up front naming the clip.
2. **UI:** an Export… button opening a sheet with the targets (all ticked but the clip), resolution and quality pickers, a run list (pending / progress / done with encode time and fps), the run line hidden until the rate is stable, and Export and Cancel. The clip's "Export video…" opens the sheet with that clip ticked.
3. **Harness:** export two targets; progress rises and completes; cancel; the refusals.

Commit: `feat(app): the export sheet`.

## Task 7 — Closeout

1. Adversarial review of the Phase 8 diff; apply the fixes and backlog any deferrals.
2. `CLAUDE.md`: the export's audio rules (one pipeline per file, the priming shift, ramps) and the layer order.
3. The hands-on checklist items, in the Task 7 notes.

## Deliberately not in this phase

- HEVC, a combined single file, per-clip settings.
- Volume UI (#52), 2160p (#53).
