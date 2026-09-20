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
   - Record all three numbers, plus the control, in "### Task 2 notes

**The composite gate PASSES. The UI budget's number does not survive contact
with the app and is re-proposed below.** Task 3 has not been started.

**Rig.** Reference laptop (i7-10610U, Intel UHD CML GT2, X11, GStreamer 1.24.2),
release build, the real app on Slint's Skia-OpenGL context. Source: the user's
`20260502121738_000004.MP4` (HEVC 1440p30, 30 min), read-only through a symlink.
Recording: a generated 30 s 1280x720 H.264 + Opus Matroska. Clip: a 30 s
schedule (900 frames) -- play from 120 s, a 4 s freeze, a zoom to 1.8x, a +45 s
skip, a second freeze to the end -- with five strokes, a `clearAll`, and
`show_pip` on. The preview was opened by a temporary one-shot timer in `main()`
sending `Command::OpenPreview` (there is no Preview button until Task 4); it has
been removed again. **The display was DPMS-off and was woken with `xset dpms
force on`;** runs taken in the first minute after waking are much worse (UI p95
18.8 ms) and are not the numbers below.

#### Composite rate and A/V alignment

Measured inside the pump: every 150 frames, the audio sink's position against
the PTS of the last frame the appsink delivered, both in their own segment time,
which starts at 0 on both branches.

| | Audio clock (as shipped) | System clock (`use_clock(SystemClock)`) |
|---|---|---|
| Media played, first probe to last | 25.000 s | 25.000 s |
| Wall (`CLOCK_MONOTONIC`) for it | 24.996 s | 25.000 s |
| **Steady rate** | **30.005 fps** | **30.000 fps** |
| **A-V offset, every probe over 30 s** | **+0.002 to +0.007 s** | **+0.002 to +0.004 s** |
| Dropped (of 900) | 1 | 1 |
| Audio glitches | none | none |

**30 fps is met and audio and picture are locked together** -- the offset never
leaves a fifth of a frame, and it does not grow. The clock makes no measurable
difference, so **the audio clock stays** (P2's reasoning is untouched) and
`use_clock` is not adopted.

**The earlier "29.0 fps" was a measurement artefact, and so was the "1 s behind
the commentary" that was inferred from it.** `PreviewStats::fps` spans the first
to the last delivered frame, and the *last seven* frames arrive about 1.1 s
after the rest: once the pump stops pushing, `glvideomixer` has no buffer for
the next output time and waits out the pipeline's latency before flushing what
it holds. Everything before that is dead on 30 fps. Two consequences:

- `PreviewStats::fps` reads ~29.1 fps on every clip and will keep doing so; it
  is the end-to-end number, not the playback rate. Either document it or measure
  the rate before the drain.
- **For Task 3:** the freeze on the last frame currently lands about a second
  late for the same reason. Sending EOS on both appsrcs when the schedule ends
  tells the aggregator its pads are done and should flush the tail at once --
  worth trying there, together with the pause that already follows.

#### UI frame time

| Run | p50 | p95 | max | frames |
|---|---|---|---|---|
| **Control: game video playing, no preview** | 2.86 | **4.54** | 27.7 | 2489 |
| Preview, eight settled runs | 2.80-3.03 | **4.18 / 4.32 / 4.61 / 4.65 / 4.69 / 5.48 / 6.25 / 8.01** | 11.9-46.5 | ~1500 each |
| Control: idle window | -- | -- | -- | **6 draws in 25 s** |

**The 4 ms figure cannot be a gate, and the idle control cannot be its
baseline.** It came from `p7-spec-review/shared.c`, which forces 60 Hz draws
over a handful of `glClear`s. Slint redraws on demand, so a genuinely idle
window draws six times in twenty-five seconds -- there is no distribution to
compare against. The app merely *scanning* the game video, which is what it does
all day, already sits at p95 4.54 ms with no composite anywhere in the process.

**Proposed budget, from the runs above:** *previewing costs the UI what scanning
costs it.* Concretely, in one session with `COACH_FRAME_STATS=1`, against a
scanning control taken in that same session:

- **p50 within 0.5 ms of the control's p50** -- observed +0.0 to +0.2 ms, and
  this is the signal that actually holds still; and
- **p95 within 4 ms of the control's p95** -- observed +0.1 ms at the median
  run and +3.5 ms at the worst of eight, where the spread is session noise
  rather than anything the preview does.

The control's own p95 is reported alongside, so a regression in the app's
drawing can't be laundered through the comparison.

#### One judgement call left

The preview's appsink runs with `qos=true`. With the audio as the clock,
dropping a late video frame keeps the picture with the words rather than letting
it slide behind, and it is the only way a dropped frame becomes observable
(`PreviewStats::dropped`). It also means "no dropped frames" is never quite
guaranteed: a settled machine drops 1 of 900 (at the skip), and a run taken
right after waking the display dropped 21. Turning `qos` off would report 0
always and hide the lateness in the frame rate instead.

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
