# Linux Port — Phase 8 Plan (Full Export)

**Date:** 2026-09-20
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-8-design.md` (decisions E1–E8)
**Status:** Reviewed. Simplification and correctness passes are applied; the AAC priming fix, the appsrc bounds, audio seeks, PiP caps and llvmpipe timings were measured.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the whole workspace **and passes its tests on its own**.

**Decisions settled before execution:**
- **The export sheet is the only export UI.** Phase 5's inline "Exporting N%" row and its Cancel (`ui/app.slint:~1552`) are deleted, along with the save-picker path (`Pick::Export`, `wire_export`, `export_file_name`, `Command::ExportClip { path }`, and the "that file is the clip's game video" check).
- **One rendering of the estimate:** the run line shows the **finish time** ("finishes at 3:42 PM"). No per-row fps or encode time; the `bus: exported …` log already carries them.
- **Preview gains the text bar only.** Game audio in preview is BACKLOG #54.

**Known facts. Don't re-derive these.** The spec's "Measured facts", plus:
- **Prototypes:** `scratchpad/p8-plan-review/{aacmux2.py,tone.py,pipcaps2.py,aseek.py,pads_llvmpipe.py}`, `scratchpad/p8-spec-review/{fourpad.py,fourpad_probe.py,never.py,aac.py,moov.sh}`, `scratchpad/p8-research/{bench.sh,audio.sh,textbench/}` (`scratchpad` = this session's scratchpad directory.
- **Benchmarks need a real trim;** `glvideomixer` ignores `identity eos-after=N`. Assert the output duration.
- **The font is NOT yet vendored** and there is no `cosmic-text` dependency. Task 3 adds both (media only).
- **A 1×1 transparent RGBA satisfies a mixer pad** and is invisible; mid-stream caps changes land on exactly the right frame. Only *geometry* races, so caps may be set from the pushing thread but rects must be PTS-keyed.
- **An appsrc-fed pad never EOSes,** and `Decoder::frame_at` already holds the last frame past the end, so `repeat-after-eos` on the PiP pad is dead config.
- **Audio seeks:** flushing **ACCURATE** costs 1–11 ms and is sample-exact; `KEY_UNIT | SNAP_BEFORE` would land early. 1 s of audio decodes in ~4 ms.
- **llvmpipe:** 1080p composite is 75 ms/frame with one pad, 92 ms with three; the media export suite is 58 s. **Test at 720p output with short entries.**
- **CI's `gstreamer1.0-libav`** makes `avdec_h264` (rank 256) outrank `openh264dec` (64), so CI moves closer to the dev machine. `avenc_aac` is rank none and must be built by name. Update the stale comment in `fixtures.rs:~167`, and pin one readback test with `GST_PLUGIN_FEATURE_RANK=avdec_h264:0` to keep openh264 coverage.
- **`frame_schedule`'s 11 core tests** (`core/tests/export.rs`) plus `media/tests/export.rs:~219` and `media/tests/preview.rs:~112` are its callers. Port them onto a one-entry compilation; don't lose them.

---

## Task 1 — Core: the compilation schedule

Core only. No callers change; `frame_schedule` stays public for one more commit.

1. `ExportTarget::Clip(Uuid)`.
2. **Extend the existing plan types** rather than adding parallel ones (`CompilationPlan`/`PlanEntry` have no production callers yet): `PlanEntry` gains `start_frame`, `frames`, `text` and `show_pip`, and `compilation_schedule(project, target) -> Compilation { frames: Vec<FrameSpec>, plan: CompilationPlan }`.
   - `FrameSpec { entry, source_time, zoom }`; `source_index` and `record_time` derive from the entry and frame index.
   - Entries are quantized to whole frames, each built by the per-clip walk (now private).
   - `text` is `"<n> / <total> | <name> | tags"`, empty parts collapsed, `<total>` the target's clip count.
3. **Every denominator and displayed length comes from the frame count,** never `total_duration_seconds`: per-entry quantization can exceed it by up to one frame per entry. Say so where the field is defined.
4. **Tests:** port the 11 `frame_schedule` tests onto a one-entry compilation; add quantization across entries, derived record time, and the text line. Don't re-test tag filtering or empty targets — `tests/plan.rs` already pins those.

Commit: `feat(core): compilation schedule`.

## Task 2 — Core: audio regions, ramps and the rate window

1. **Concrete API, no abstraction:**
   - `audio_regions(&Compilation) -> Vec<Region>`, where `Region { track: Game | Commentary, out_samples: Range<u64>, source_offset: f64, gain: f64 }` — game regions from play segments (freezes silent), one commentary region per entry.
   - `envelope(region, sample) -> f64` for the 5 ms linear fades at each region's edges, clamped at t=0.
   - **The ramps are computed on the emitted timeline**, i.e. after the priming drop in Task 4, so the first region doesn't start mid-fade.
2. **`RateWindow::sample(frames_done, elapsed) -> Option<f64>`:** a trailing window returning `None` until ≥5 samples and ≥2 s. No monotonic clamp. The bus does `remaining_frames / rate`.
3. **Tests:** region boundaries and sample counts; the envelope, including a region shorter than a ramp and the t=0 clamp; silence during freezes; the rate gate (port macOS's cases).

Commit: `feat(core): export audio regions, ramps and the rate window`.

## Task 3 — Media: the three-pad composite

This task migrates media and the bus to the compilation, so it compiles and tests on its own.

1. **Vendor the font:** `DejaVuSans.ttf` plus its licence into `video-coach-media`, and add `cosmic-text` (media only).
2. **`overlay.rs` renders at the output size:** strokes mapped into the entry's fit rect (line width from the picture's height), plus the bar's background and glyphs, `Wrap::None` with a tail ellipsis.
   - **Switch preview's overlay to output space in this task too** (its appsrc caps and `sink_2` rect), or `tests/preview.rs::the_composite_places_the_pip_and_the_overlay_on_the_picture` fails here rather than in Task 5.
3. **`layout.rs`:** the bar's rect, and the PiP's raised margin (`bar height + margin`).
4. **The export tail's three pads,** per E2:
   - pad 0's rect and caps per entry; **rects PTS-keyed in pad probes**, sharing one per-frame table and `frame_index(pts)` helper with `install_zoom` rather than building a second one;
   - **pad 1 (PiP) is fed every frame** from the entry's recording `Decoder`, or a 1×1 transparent RGBA when `show_pip` is off or the recording is unusable; **its rect comes from the recording's probed aspect, PTS-keyed** — never from the pushed caps, since the filler's 1×1 would make it square;
   - **no `repeat-after-eos`** (dead config on a pumped pad);
   - pad 2 is the overlay, fixed for the run.
5. **Resolution and quality parameters** (720p/1080p, QP 28/24/20), replacing the constants.
6. **The minimal bus change** to build a one-entry compilation (its recording, text and `show_pip`) so `ExportJob` has a producer. The full target list is Task 6.
7. **Tests:**
   - **the multi-clip fiducial:** two counter fixtures of different sizes and frame rates, **720p output, short entries**, every frame's counter checked, and the output's duration asserted in the same test;
   - pad placement, z-order and premultiplied alpha over a synthetic base, including `show_pip` off;
   - **stroke mapping into the fit rect and the ellipsis as `overlay.rs` unit tests** (exact pixels, no pipeline).

Commit: `feat(media): compilation export composite`.

## Task 4 — Media: audio into the file

1. One audio-only pipeline per source and per recording, appsink at F32LE/48k/2ch; **flushing ACCURATE seeks** per play segment; **a file with no audio track contributes silence**.
2. The mixer pushes blocks **at or ahead of** the video frames they cover; **the audio appsrc is effectively unbounded and the pump never waits on it** (a bound deadlocks: the encoder keeps the muxer ~0.43 s behind the pushed video).
3. `audioconvert` → `avenc_aac bitrate=192000` → `aacparse` → `mp4mux`, **dropping the first 1024 samples** of the mixed stream. (Shifting timestamps and clamping at zero measurably does nothing.)
4. **CI:** add `gstreamer1.0-libav`; update `fixtures.rs`'s stale plugin-set comment; pin one readback test to `openh264dec` by rank.
5. **Tests:**
   - **the tone test:** a tone at 1.000 s decodes back within a millisecond;
   - the gate and ramps over a fixture whose tracks are distinguishable;
   - a source with no audio track exports silence rather than failing;
   - the fiducial fixture **with audio** still matches the schedule's duration (rather than a third export).

Commit: `feat(media): export audio`.

## Task 5 — Preview gains the text bar

Preview draws the bar with `n / total = 1 / 1`. (The overlay-space switch already happened in Task 3.)

Commit: `feat(media): the text bar in preview`.

## Task 6 — Bus: compilation exports

1. The target list (All clips, each tag with its clip count and **frame-derived** length, the selected clip); compilation exports; **progress carrying frame counts, emitted on percent change**; cancel leaving finished targets alone; a missing source or recording refused up front naming the clip.
2. **E6's file rules:** `<project>/exports/` created on demand; `<label> - <project>.mp4` with `/` and `:` replaced; `.part` then rename; a re-run replaces.
3. **Delete** the save-picker path and `Command::ExportClip { path }` (see "Decisions"), and adapt the harness tests built on paths (`an_export_never_writes_over_its_game_video` goes; the missing-directory failure case needs a new trigger).
4. **Persist** resolution and quality into the existing `Preferences` fields.
5. **Harness:** export two targets; progress rises and completes; cancel; the refusals.

Commit: `feat(app): compilation exports on the bus`.

## Task 7 — UI: the export sheet

1. An **Export…** button opening a sheet: targets (all ticked but the clip), resolution and quality pickers, a run list (pending / progress / done), and a run line with the **finish time**, hidden until the rate is stable. Export and Cancel.
2. The clip's "Export video…" opens the sheet with that clip ticked.
3. **Delete the inline export row.**
4. A screenshot pass with a scratch project driven through callbacks; no camera; delete the scratch data.

Commit: `feat(app): the export sheet`.

## Task 8 — Closeout

1. Adversarial review of the Phase 8 diff; apply and backlog.
2. **Re-measure the Phase 7 preview gate** (30 fps, the A/V offset, the UI budget) now that the overlay is output-size, and record it.
3. `CLAUDE.md`: the export's audio rules (one pipeline per file, the priming drop, ramps on the emitted timeline, the unbounded audio appsrc) and the layer order.
4. The hands-on checklist items, in the Task 8 notes.

## Deliberately not in this phase

- HEVC, a combined single file, per-clip settings.
- Volume UI (#52), 2160p (#53), preview game audio (#54).
