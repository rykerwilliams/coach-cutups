# Linux Port — Phase 7 Plan (Clip Preview)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-7-design.md` (decisions P1–P6)
**Status:** Reviewed. Simplification and correctness passes are applied; the branches, the clock, seeking and EOS were measured on the reference laptop.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task.

**Decisions settled before execution** (they were flagged by review):
- **"No private GL context" is an app rule, not a test rule.** The composite takes its GL context as a parameter; the app passes Slint's, and tests and the harness pass the existing surfaceless `SharedGl::get()`, exactly as export does. Otherwise preview could never run headless.
- **Opening a preview is explicit.** A **Preview** button in the clip inspector and a "Preview clip" context-menu item. **Space keeps meaning "play the game video" until a preview is open**; while one is open the transport drives it. Otherwise Space would stop scanning whenever a clip happened to be selected, which is also how recording starts.
- **At the end of the schedule the preview pauses on its last frame** and stays open, with the position at the end. It does not auto-close and does not run on into the recording's tail.

**Known facts. Don't re-derive these.** The spec's "Measured facts", plus these, all measured:
- **`autoaudiosink` does become the clock** with a pumped appsrc on another branch (`GstPulseSinkClock`).
- **A recording shorter than the schedule is fine:** the mixer keeps compositing past its EOS and pacing holds 1:1.
- **An unlinked `decodebin3` video pad does not stall its branch,** so `show_pip = false` is safe.
- **One pipeline seek reaches both branches** and `seek-data` fires once, on the seeking thread.
- **The pipeline can't preroll until the pump pushes,** so never block on `get_state` after `set_state(PLAYING)`. `Encoder::start` already gets this right.
- **Buffer stride matches** `tiny_skia::PixmapMut::from_bytes` for RGBA (`w*4` is already 4-aligned). Adding a `VideoMeta` is free insurance.
- **`clip.recording_duration` is a stored value, not the `.mkv`'s media duration.** They can differ by frames. The graph tolerates it; don't write code that assumes they're equal.
- **Scratch prototypes to read:** `scratchpad/p7-spec-review/shared.c` (the gate prototype, but see Task 2: it decodes natively and has no `gltransformation`), `p7-spec-review/{seekable,branches,branches2,clk}.py`, `p7-plan-review/{a2,b_clock,c_mix,d_seek}.py`.

---

## Task 1 — Core ratios and the overlay rasterizer

No GStreamer graph, no GL. Independently verifiable.

1. **Core:** the overlay and PiP layout ratios as pure functions (the parent spec's ratio table), with tests.
2. **`video-coach-media/src/overlay.rs`:** add `tiny-skia` to that crate (core stays media-free) and implement:

   ```rust
   pub fn render_overlay(clip: &Clip, record_time: f64, w: u32, h: u32) -> gst::Buffer; // premultiplied RGBA
   ```

   - **`w`/`h` are the picture rect's size, not the output size.** Strokes are normalized to the content rect (the letterboxed picture at 1×), and `line_width` to its height. Rasterizing at the output size would stretch strokes across the bars on any non-16:9 source. This also closes BACKLOG #20's "revisit at Phase 7".
   - Draw `visible_strokes(clip, record_time)` with round caps and joins, into a mapped `gst::Buffer` via `PixmapMut::from_bytes`, and attach a `VideoMeta`.
3. **Tests, on invariants rather than a golden PNG** (tiny-skia's anti-aliasing isn't a stable contract):
   - the pixel at a stroke's centre is the stroke colour, and a point far from any stroke is transparent;
   - `r, g, b ≤ a` on every pixel (premultiplied);
   - a cleared or expired stroke draws nothing;
   - the line width scales with the rect's height.

Commit: `feat(media): stroke overlay rasterizer`.

## Task 2 — The composite on screen (gate)

Rename `media/src/export/` to `media/src/composite/`, with `export.rs` and `preview.rs` tails over the shared `decode.rs`, pump and geometry, and `ExportError` becoming `CompositeError` (export's public API keeps its names). CLAUDE.md permits the adjacent rename; preview failures must not surface as "export failed".

1. **The mailbox instance is owned by the bus** and passed into both `SourcePlayer::new` and the preview builder. `video.rs` binds to that one instance, so a preview that made its own would put nothing on screen.
2. **The preview tail:** `glcolorconvert` → `gl_caps()` appsink with `sync=true`, filling the mailbox.
3. **The GL context is a parameter:** Slint's from `GlReady`, or `SharedGl::get()` for tests.
4. **The graph is the real one, all three pads** (the gate must measure what ships):
   - pad 0: the pumped source branch through `gltransformation` (which renders a **source-sized** texture per frame before the mixer downscales);
   - pad 1: the natively played recording's video, only when `show_pip`;
   - pad 2: **a second `appsrc`** carrying the overlay buffer.
   - **Both appsrcs are pushed by one pump loop with identical PTS per frame**, or the mixer starves.
   - The overlay pad and pad 0 use the same `fit_rect`, so the overlay lands on the picture, not the output.
   - Pin the overlay branch's caps to RGBA end to end, with a comment: GStreamer's RGBA means straight alpha, and the premultiplied data only works because of `blend-function-src-rgb=one`.
5. **Land `OpenPreview` / `ClosePreview` on the bus now**, minimally, so nothing temporary is committed for the gate.
6. **The gate, with the method spelled out:**
   - **Instrumentation:** time the UI's draw in `video.rs` behind an env var (`COACH_FRAME_STATS=1`), printing p50/p95/max over the run; count composite frames from the appsink and dropped frames from the sink's QoS messages.
   - **Input:** the user's HEVC 1440p file (read-only), a schedule of about 30 s with plays, a freeze and a skip, `show_pip` on with a generated recording, and strokes present.
   - **Pass:** the composite sustains 30 fps with no dropped frames, and the UI's frame time **p95 ≤ 4 ms** (idle control 1.34 ms).
   - Record all three numbers, plus the control, in "### Task 2 notes".
   - **If it fails, stop and report.** Don't start Task 3.

Commit: `feat(media): clip preview composite`.

## Task 3 — Seeking, pausing and the end of the clip

1. **Both appsrcs are `stream-type=seekable`,** each with a `seek-data` handler. The frame index and a **seek generation** live behind one mutex covering both; the pump re-reads the generation under the lock before each push, since a stale push after `FLUSH_STOP` is accepted silently.
2. **Pause** through the pipeline state; the pump stops on backpressure.
3. **End of schedule:** the pump finishes, the preview pauses on the last frame, the position reads the end, and `Event::Playing(false)` is emitted. The recording's tail does not play on.
4. **Position:** an `Arc<AtomicU64>` frame index the pump stores; the existing 30 Hz `tick` reads it. No new 30 Hz event, and one position path.
5. **Volume:** `preview_commentary_volume` on the `volume` element, as a live property set; muted on the first `ScrubMove` while previewing and restored on `ScrubRelease`.
6. **Tests** (media, surfaceless GL, short 720p clips so CI stays quick):
   - one composite test: the PiP pad rect and the overlay over a synthetic solid base, checking geometry and alpha;
   - a seek lands on the right frame and the pump doesn't push a stale one.

Commit: `feat(media): preview seeking, pause and end-of-clip`.

## Task 4 — Bus, UI and harness

1. **Bus:** exclusivity (the source player paused, not unloaded; preview refused while recording or exporting, and both refused while previewing); the transport routed to the preview while open, **bypassing `SkipCoordinator` and `skip_range`**, which are defined over concat source time; the mailbox cleared on close; a missing source or recording file refused with a clear message; deleting the previewed clip closes the preview first.
2. **UI:** a **Preview** button in the clip inspector and a "Preview clip" context-menu item; the transport drives the preview while open; **identity zoom and the live stroke layer hidden while previewing**; a "Previewing <name>" indicator with Close; Esc closes.
3. **Harness:** open, play, seek, close; the refusals; the position published while previewing. The harness passes `SharedGl::get()` for the context.

Commit: `feat(app): preview a clip`.

## Task 5 — Closeout

1. Adversarial review of the Phase 7 diff; apply the fixes and backlog any deferrals.
2. `CLAUDE.md`: a paragraph on the composite module (the two tails, the GL context rule, the overlay's picture-rect space, and the measured UI budget).
3. The hands-on checklist items, in the Task 5 notes.

## Deliberately not in this phase

- Game audio, the splice and the ramps: Phase 8 (which must drain audio appsinks before pulling video).
- The text bar and the scoreboard: Phases 8 and 9.
- Playback rate, looping, drawing in preview.
