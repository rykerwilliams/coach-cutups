# Linux Port — Phase 7 Plan (Clip Preview)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-7-design.md` (decisions P1–P6)
**Status:** Draft, pre-review.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the whole workspace.

**Known facts. Don't re-derive these.** Everything in the spec's "Measured facts", plus:
- **Prototypes to read** (in `/tmp/claude-1000/-home-rajah-git-coach-cutups/e3d8d025-26c5-4369-a3ee-3c564dfbf895/scratchpad/`):
  - `p7-spec-review/shared.c`: the wrapped-EGL-context composite that produced the gate numbers, following `video.rs:109-151` and `:49-92`;
  - `p7-spec-review/{seekable.py,branches.py,branches2.py,clk.py}`: the seek-data, deadlock and clock experiments;
  - `p7-research/`: the earlier 3-pad benchmark.
- **Phase 5 code to reuse:** `export/decode.rs` (`Decoder::frame_at`, `PULL_AHEAD`, the `KEY_UNIT|SNAP_BEFORE` seek), `export/encode.rs` (the pump, `QUEUED`, `install_zoom`'s PTS-keyed probe, the mixer and its pad geometry), `export/mod.rs` (`SharedGl`, the `Watch`).
- **App code:** `video.rs` wraps Slint's EGL context and draws the mailbox frame; `bus/mod.rs`'s `GlReady` carries the display and context; `main.rs` polls `PositionHandle` at 30 Hz.
- **Do not** add an audio appsink to the pumped source branch (measured deadlock).
- **`glvideomixer`** waits indefinitely on every pad; `blend-function-dst-rgb` already defaults correctly.

---

## Task 1 — The composite on screen (gate 1)

The riskiest piece first, and nothing else in this task.

1. **Hoist `FrameMailbox`** out of `SourcePlayer` into a standalone shared type, used by both the player and preview.
2. **Parameterize the composite builder** in `video-coach-media/src/export/`: output size, and a GL context that is either export's private surfaceless display or an injected one.
3. **A preview tail:** `glcolorconvert` → `gl_caps()` appsink with `sync=true`, filling the mailbox.
4. **A minimal preview path:** the source branch only (pumped, video, zoom), no PiP and no overlay, no audio, opened from a temporary code path.
5. **Measure the gate** on the laptop with the user's HEVC 1440p file (read-only): the composite's fps and dropped frames, and the UI frame time p50/p95/max, against the spec's budget (p95 ≤ 4 ms). Use the prototype's method.
6. **If the gate fails, stop and report.** Don't start Task 2.

Write "### Task 1 notes" with the numbers.

Commit: `feat(media): shared composite builder with a preview tail`.

## Task 2 — The full preview graph

1. **The recording branch,** played natively in the same pipeline: `filesrc ! decodebin3` → video to the PiP mixer pad (only when `show_pip`), audio through `volume` → `autoaudiosink`, which provides the clock.
2. **`overlay.rs`:** `render_overlay(clip, record_time, w, h) -> gst::Buffer`, tiny-skia strokes into a mapped buffer at the output size, premultiplied, onto mixer pad 2 with `blend-function-src-rgb=one`.
3. **Seeking:** `appsrc stream-type=seekable` plus a `seek-data` handler; the frame index and a seek generation behind one mutex; the pump re-reads the generation under the lock before each push.
4. **Position:** the pump publishes `n / 30`.
5. **Tests** (media):
   - the overlay against a golden image (stroke positions, premultiplied alpha);
   - one composite test: the PiP pad rect and the overlay over a synthetic solid base.
6. **Measure again** with the full 3-pad graph on the user's footage, and record it in the notes.

Commit: `feat(media): clip preview graph`.

## Task 3 — Bus, UI and harness

1. **Bus:** `OpenPreview(clip_id)` / `ClosePreview`; `Event::Preview(Option<Uuid>)`; exclusivity (the source player paused, not unloaded; refused while recording or exporting, and both refused while previewing); transport routed to the preview; position from the pump; the mailbox cleared on close.
2. **UI:** Play opens a preview of the selected clip; the scrubber and readout span the clip; **identity zoom and the live stroke layer hidden while previewing**; a "Previewing <name>" indicator with Close; Esc closes; the commentary is muted while the scrubber is held.
3. **Harness:** open, play, seek, close; refusals while recording and exporting; the position published while previewing.
4. **Screenshot pass:** a scratch project with a fixture source and a clip whose recording is a generated file; drive it through callbacks with no input injection and no camera; screenshot the preview with its PiP and a stroke. Delete the scratch data, and kill only your own PID.

Commit: `feat(app): preview a clip`.

## Task 4 — Closeout

1. Adversarial review of the Phase 7 diff; apply the fixes and backlog any deferrals.
2. `CLAUDE.md`: one paragraph on the shared composite builder (preview vs export tails, the GL context rule, and the measured UI budget).
3. The hands-on checklist items, in the Task 4 notes.

## Deliberately not in this phase

- Game audio, the splice and the ramps: Phase 8 (with the drain-first rule for audio appsinks).
- The text bar and scoreboard: Phases 8 and 9.
- Playback rate, looping, drawing in preview.
