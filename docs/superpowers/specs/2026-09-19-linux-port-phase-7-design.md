# Linux Port — Phase 7: Clip Preview

**Date:** 2026-09-19
**Status:** Reviewed (simplify and correctness passes applied; the GL topology, the composite and the decode branches were measured on the reference laptop)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` ("The compositor decision", "Export frame driver", Phasing → Phase 7)
**Builds on:** Phase 5 (the composite graph, the frame schedule, the pump), Phase 6 (strokes), Phase 4 (recordings), Phase 3 (clip selection)
**Evidence:** the macOS inventory of `ClipPreviewBuilder.swift` and `ContentView.swift`; the Phase 7 research and review measurements, recorded below.

---

## Goal

Select a clip and play it back inside the app: the game video edited by the coach's plays, freezes and skips, zoomed as they zoomed, with their webcam inset, their drawings, and the commentary audio.

**Game audio, its splice and its ramps move to Phase 8,** where the export mixer forces them anyway. The scoreboard (Phase 9) and the text bar (Phase 8) join the same overlay later.

## Done when

1. **Play.** Selecting a clip and pressing Play (or Space) previews it: source video, zoom, webcam PiP, drawings and commentary audio.
2. **Transport.** Space toggles, the scrubber seeks **frame-accurately** within the clip, skips work, and Esc closes the preview and returns to scanning.
3. **Shared path.** Preview is built from the same composite builder as export; Phase 8 changes only the tail. Nothing preview-specific describes geometry.
4. **Performance.** The user's HEVC 1440p footage previews at 30 fps with no dropped frames, and the UI's frame time stays inside the budget in "Gates".
5. **No waiting.** A preview starts in a few hundred milliseconds. There is no cache, no polling and no timeout.

---

## Measured facts (reference laptop)

**The composite,** a 3-pad `glvideomixer` (base + PiP + full-frame RGBA overlay), 1440p HEVC source, frames reaching Slint through the existing mailbox and `BorrowedOpenGLTextureBuilder`, with the UI drawing at 60 Hz:

| Preview output / overlay raster | Composite | UI frame time p50 / p95 / max |
|---|---|---|
| 720p / 1080p overlay | 30.01 fps | 0.90 / 6.65 / 37.7 ms |
| **720p / 720p overlay** | **30.00 fps** | **0.75 / 2.65 / 10.9 ms** |
| 1080p / 1080p overlay | 30.01 fps | 1.53 / 10.1 / 46.9 ms |
| Control (no pipeline) | — | 0.76 / 1.34 / 6.1 ms |

- **Sharing Slint's GL context is the *better* topology.** A private surfaceless display measured 0.97 / **28.3** / 45.2 ms, about 4× worse at p95, before adding a system-memory copy. The bottleneck is the 15 W iGPU, not the context. **There is no private-context fallback.**
- **Rasterize the overlay at the output size.** A 1080p overlay into a 720p preview costs 2.5× the p95 and 3.5× the max.
- **`glupload` takes a system-memory RGBA buffer per frame at 30 fps** without trouble. `glvideomixer`'s `blend-function-dst-rgb` already defaults to `one-minus-src-alpha`, so only `blend-function-src-rgb=one` needs setting.
- **An audio appsink on a pumped decode branch deadlocks.** With `decodebin3` feeding a GL video appsink (max-buffers 2) plus an audio appsink, pulling only video stalled after 15 frames (0.2 s). Draining audio first, then pulling video, ran clean (3651 video + 5711 audio buffers in 5 s). **This is why the source branch stays video-only in Phase 7** (Phase 8's mixer must adopt the drain-first rule).
- **A pipeline seek fires `seek-data` on *every* seekable appsrc, on the seeking thread,** not the pump's, and pushes after `FLUSH_STOP` with stale PTS are silently accepted (zero `FLUSHING` returns).
- **PAUSED stops the pump** through backpressure on its own (0 pushes in 2 s).
- **Accurate seeks** on the user's footage: 8.8 / 20.5 ms (median/worst), plus one ~15 ms composite pass, so a scrub tick costs 25–40 ms.
- **macOS preview** capped its render at a **1920** long side, scrubbed with infinite tolerance (exact seeks rendered black on long-GOP HEVC), had **no ramps** (a click at every freeze), and its 50 ms poll and 20 s timeout existed only because AVFoundation compositions are slow to build.
- **Stale doc corrected:** the parent spec's line 437 still repeats "preview ignores `showPiP`". Line 201 and BACKLOG #27 are already fixed.

---

## Decisions

### P1. Preview is the export graph with a different tail

`video-coach-media/src/export/` becomes a shared composite builder. Everything up to and including `glvideomixer` is common; the tail differs:

| | Export | Preview |
|---|---|---|
| Tail | NV12 → `gldownload` → encoder → `mp4mux` → `filesink` | `glcolorconvert` → `gl_caps()` appsink → the shared `FrameMailbox` |
| Output | 1920×1080 | **1280×720** (measured: the best UI frame time) |
| GL | a private surfaceless display | **Slint's context**, from `Command::GlReady` |
| Pacing | as fast as possible | the sink syncs to the clock |

- **Output size and GL context become parameters** of the builder. `SharedGl` becomes injectable.
- **`FrameMailbox` is hoisted** out of `SourcePlayer` into a standalone shared type, since both the player and preview fill it. The bus guarantees only one is PLAYING, and **closing a preview clears the mailbox**, so the last frame doesn't stay on screen.
- **The zoom probe is reused verbatim** from Phase 5: a sink-pad buffer probe keyed on PTS, which is flush-proof. Phase 5 uses no control bindings, so there is nothing to re-install after a seek.

### P2. The two branches, and the clock

- **The source branch is pumped, video-only.** `Decoder::frame_at` and the Phase 5 pump feed `appsrc` → `gltransformation` (zoom) → mixer pad 0, stamped `pts = n/30`. No audio appsink: it deadlocks a pumped branch (measured).
- **The recording branch plays natively.** `filesrc ! decodebin3` → its video to the PiP mixer pad, its audio to `volume` → `autoaudiosink`.
  - **Record time *is* output time.** `playback_segments` emits `out_duration` in the recording's timeline, so frame `n` is at `t = n/30` in that same timeline. The recording is therefore 1:1 with the output clock and needs **no pump, no re-timestamping and no appsink**. The mixer aligns the pads by running time.
  - That identity is also why `zoom_at(events, t)` and the overlay's `record_time` are simply `n/30`.
- **The audio sink provides the clock,** so the composite follows the commentary, which is the track the coach hears.
- **The PiP pad exists only when `show_pip`.** `glvideomixer` waits indefinitely on every pad, so an unused pad would stall it. When the recording's video ends before the schedule does, the pump stops too (the schedule's length is the recording's duration).
- **Volume** comes from `preview_commentary_volume`, applied to the `volume` element, so a live change is a property set.

### P3. Transport, pause and scrub

- **The sink syncs to the clock**, so real-time pacing is free. The pump throttles on the appsrc queue; **PAUSED stops it through backpressure** (measured), so there is no sleep loop and no condvar beyond a wake on seek and close.
- **Scrub and skip** use `appsrc stream-type=seekable` plus a `seek-data` handler:
  - `seek-data` arrives on the **seeking thread**, not the pump's, so the frame index and a **seek generation counter** live behind one mutex. The pump re-reads the generation under the lock before each push, because a stale push after `FLUSH_STOP` is otherwise accepted silently (measured).
  - The recording branch seeks natively in the same pipeline seek.
- **Preview scrubs frame-accurately** (25–40 ms a tick). macOS used infinite tolerance because exact seeks rendered black on long-GOP HEVC.
  - **Audio during a drag:** the commentary is muted while the scrubber is held and unmuted on release, since each tick flushes the audio sink.
- **Position comes from the pump's frame index** (`n / 30`), published with the preview's events. The existing `PositionHandle` is the source player's pipeline and reports nothing while preview is open.
- **Esc closes the preview.**

### P4. The overlay rasterizer

`video-coach-media` gains `overlay.rs`:

```rust
pub fn render_overlay(clip: &Clip, record_time: f64, w: u32, h: u32) -> gst::Buffer; // premultiplied RGBA
```

- **Phase 7 draws strokes only**, from core's `visible_strokes`. Phase 8 adds the text bar and Phase 9 the scoreboard.
- **Rasterized at the output size** (measured).
- **No pool.** A fresh buffer per frame, drawn into with `tiny_skia::PixmapMut::from_bytes` over the mapped `gst::Buffer`, is sub-millisecond at 720p. A pooled pixmap would need destroy-notify recycling to avoid overwriting a frame still queued in the mixer.
- **Premultiplied-over** on the mixer pad: set `blend-function-src-rgb=one` (the destination function already defaults correctly).
- **Geometry stays in core** (`visible_strokes`, `zoom_at`, the layout ratios); **pixels stay in media**, and so does the font when Phase 8 needs one.

### P5. Control and lifecycle

- **Commands:** `OpenPreview(clip_id)` and `ClosePreview`.
- **Exclusivity:** the source player is **paused, not unloaded**, so closing a preview is a no-op restore. Preview is refused while recording or exporting, and recording and export are refused while previewing.
- **Events:** `Event::Preview(Option<Uuid>)` (the clip being previewed, or closed) plus the existing `Event::Playing`. There is no preview-specific play state and no spinner: the preroll is 100–300 ms.
- **No cache, no polling.** macOS needed both only because building an AVFoundation composition was slow.
- **A missing source or recording file** refuses with a clear message.
- **Deleting the previewed clip** closes the preview first.

### P6. UI

- **Play** (button or Space) opens a preview of the selected clip; the transport then drives the preview, using the existing scrubber and readout over the clip's own duration.
- **The picture binds identity zoom, and the live stroke layer is hidden, while previewing.** The preview texture already has zoom and strokes baked in, so the scan-time zoom transform and Phase 6's overlay would apply them twice.
- **A "Previewing <name>" indicator** with a Close button, and Esc.
- **Drawing is off in preview.**

---

## Crate responsibilities

| Crate | Phase 7 contents |
|---|---|
| `video-coach-core` | The overlay and PiP layout ratios as pure functions. (The audio splice and ramps move to Phase 8.) |
| `video-coach-media` | The shared composite builder (output size and GL context as parameters); `overlay.rs`; the preview tail into the mailbox; the natively-played recording branch; seek handling with the generation counter; `FrameMailbox` hoisted. |
| `video-coach-app` | Bus: `OpenPreview` / `ClosePreview`, exclusivity, `Event::Preview`, position from the pump, transport routed to the preview. UI: Play opens a preview, identity zoom and no stroke layer while previewing, the indicator, Close, Esc. |
| `video-coach-harness` | Open, play, seek, close; the refusals. |

## Testing

- **Core:** the layout ratios.
- **Media:**
  - the overlay rasterizer against a golden image (strokes at known positions, and premultiplied alpha);
  - **one** composite test: a PiP pad rect and an overlay composited over a synthetic solid base, checking geometry and alpha. The frame-exactness of the schedule and pump is already covered by Phase 5's fiducial, through the same code.
- **Harness:** open, play, seek, close; refusals while recording or exporting; the position published while previewing.
- **Manual** (batched): preview a real clip; drawings, zoom, PiP and commentary line up with what was recorded.

## Gates

1. **The composite on screen, first**, before the overlay and the PiP work is finished: 30 fps composite with no dropped frames, and a **UI frame time p95 ≤ 4 ms** (the measured 720p/720p figure is 2.65 ms, against a 1.34 ms idle control).
2. **The user's HEVC 1440p footage** previews at 30 fps with no dropped frames. The review's prototype already meets both; the gate is that the real implementation does.

## Risks

1. **Three positions must agree** (the pump's frame index, the appsrc segment, and the natively-playing recording) across pause and scrub. The generation counter keeps the hand-written state to one mutex.
2. **Mixer pad starvation:** every pad must receive frame `n` before the pump advances, and the PiP pad must not exist when `show_pip` is false.
3. **A stale push after a flush** is accepted silently, so the generation check is load-bearing, not defensive.

## Deferred

- **Game audio in preview, the splice and the 5 ms ramps: Phase 8,** with the export mixer. Phase 8 must adopt the drain-first rule for audio appsinks measured above.
- The text bar and scoreboard in the overlay: Phases 8 and 9.
- Playback-rate control and looping: macOS had neither.
- Drawing during preview.
