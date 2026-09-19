# Linux Port — Phase 7: Clip Preview

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` ("The compositor decision", "Export frame driver", Phasing → Phase 7)
**Builds on:** Phase 5 (the export graph and frame schedule), Phase 6 (strokes), Phase 4 (recordings), Phase 3 (clip selection)
**Evidence:** the macOS inventory of `ClipPreviewBuilder.swift`, `PreviewCompositor.swift` and `ContentView.swift`'s preview flow; and the Phase 7 research measurements on the reference laptop.

---

## Goal

Select a clip and play it back inside the app **as it will export**: the game video edited by the coach's plays, freezes and skips, zoomed as they zoomed, with their webcam inset, their drawings, and both audio tracks. This is the first time the whole composite runs.

The scoreboard (Phase 9) and the text bar (Phase 8) join the same overlay later.

## Done when

1. **Play.** Selecting a clip and pressing Play (or Space) previews it: source video, zoom, webcam PiP and drawings, with commentary and source audio.
2. **Transport.** Space toggles, the scrubber seeks **frame-accurately** within the clip, skips work, and Esc closes the preview and returns to scanning.
3. **Fidelity.** What preview shows is what export burns in: the same frame schedule, the same zoom, the same overlay geometry, the same PiP rule (`show_pip`).
4. **Performance.** 1080p sources preview at 30 fps with no dropped frames on the reference laptop, and the app's own UI stays responsive.
5. **No waiting.** A preview starts in a few hundred milliseconds. There is no cache and no timeout.

---

## Measured facts (research, reference laptop)

- **The composite has headroom.** A 3-pad `glvideomixer` (base 1080p + 720p PiP + full-frame RGBA overlay) ran at **113 fps** free-running, and at exactly **30.04 fps with 0 dropped frames** when the sink synced to the clock, costing about **18% of one core**. At 720p output it is 14%.
  - **Measurement trap:** an overlay pad fed by `videotestsrc pattern=ball` measures the pattern generator (37 fps), not the mixer. Use `pattern=solid-color` or a real appsrc.
- **Not yet measured:** the mixer running on **Slint's shared GL context** while Skia draws the UI, and the user's real HEVC 1440p footage through the mixer. Both are Phase 7 gates.
- **The recording format** (H.264 + Opus in Matroska) decodes cleanly for the PiP.
- **macOS preview facts:**
  - AVPlayer strips custom compositors, so preview was rebuilt on the built-in one, which ignores transform ramps; zoom had to be stepwise per keyframe.
  - Preview capped its render size at a 1920 long side.
  - Preview audio was flat volumes with **no ramps**, so every freeze boundary clicked.
  - Preview scrubbed with **infinite seek tolerance**, because exact seeks rendered black on long-GOP HEVC.
  - The 50 ms poll and ~20 s timeout existed because building an AVFoundation composition is slow.
- **Corrections to older docs** (fold in while editing):
  - **BACKLOG #27(c) and parent spec lines 201 and 437 are stale:** macOS preview *does* honour `show_pip`, at build and live. Don't "fix" a non-bug.
  - **Parent spec line 389 says the overlay font is bundled in `video-coach-core`.** That contradicts CLAUDE.md, which bans a font or image crate there. The rasterizer and its font live in `video-coach-media`.

---

## Decisions

### P1. Preview is the export graph with a different tail

`video-coach-media/src/export/` becomes a shared composite builder. Everything up to and including `glvideomixer` is common; the tail differs:

| | Export | Preview |
|---|---|---|
| Tail | `glcolorconvert` → NV12 → `gldownload` → encoder → `mp4mux` → `filesink` | `glcolorconvert` → `gl_caps()` appsink → the existing `FrameMailbox` |
| Output size | 1920×1080 | 1280×720 (fixed) |
| GL context | a private surfaceless EGL display | **Slint's context**, from `Command::GlReady` |
| Pacing | as fast as possible | the sink syncs to the clock |

- **The output size becomes a parameter** of the builder rather than a constant.
- **`SharedGl` becomes injectable.** Export keeps its private display, which is what keeps the UI's vsync out of export. Preview must use Slint's context, or the texture handed to Slint is invalid.
- **Preview writes into the existing `FrameMailbox`,** so `video.rs` and `BorrowedOpenGLTextureBuilder` need no change. The bus guarantees that only one of {source player, preview} is PLAYING, so one mailbox suffices.
- **Fixed 1280×720 output,** with Slint scaling the texture to the window. That avoids renegotiating mixer caps on every resize and keeps preview and export geometry identical apart from one scale factor. macOS likewise capped preview.

### P2. Pacing and transport

- **The sink syncs to the clock.** The pump already stamps `pts = n/30`, so real-time pacing is free, and the pump throttles on the existing queue wait. Nothing sleeps in Rust.
  - The pump's busy-wait becomes a condvar, since a preview can sit paused for minutes.
- **The audio sink provides the clock,** so video follows audio (P4).
- **Pause** sets the pipeline to PAUSED.
- **Scrub and skip** use `appsrc stream-type=seekable` plus a `seek-data` handler: GStreamer's flush and base-time machinery does the work, and the pump only repositions its frame index.
  - **Preview scrubs frame-accurately.** macOS used infinite tolerance because exact seeks rendered black on long-GOP HEVC; `Decoder::seek` is accurate by construction at 10–22 ms on the user's footage.
  - The zoom control bindings are re-installed after a flushing seek if they don't survive it (to be confirmed during the task).
- **Esc closes the preview** and returns to scanning, as on macOS.

### P3. The overlay rasterizer

`video-coach-media` gains `overlay.rs`:

```rust
pub struct Overlay { /* tiny-skia pixmap pool */ }
impl Overlay {
    pub fn render(&mut self, clip: &Clip, record_time: f64, w: u32, h: u32) -> gst::Buffer; // premultiplied RGBA
}
```

- **Phase 7 draws strokes only.** Phase 8 adds the text bar and Phase 9 the scoreboard, without changing the shape.
- **Premultiplied-over** is configured on the mixer pad (`blend-function-src-rgb=one`, `dst=one-minus-src-alpha`), not by demultiplying in Rust.
- **The pixmap is pooled** (8.3 MB per 1080p frame).
- **Geometry stays in core** (`visible_strokes`, `zoom_at`, the layout ratios as pure functions); **pixels stay in media**. The font, when Phase 8 needs one, lives in media too.
- **Strokes are not zoom-transformed** and are normalized to the content rect, exactly as Phase 6 captures them.

### P4. Audio: a Rust PCM mixer

Two decode branches end in audio appsinks at F32LE/48k/2ch. Rust splices and mixes, and one appsrc feeds `audioconvert` → `autoaudiosink`.

- **Source audio plays only during `play` segments** (freezes are silent), which follows the frame schedule, so one timeline drives both video and audio.
- **Commentary audio is continuous** and 1:1 with record time.
- **5 ms linear ramps** at the start and end of every contiguous region on either track. Preview therefore loses the click macOS had at every freeze boundary.
- **Volumes** come from `preview_source_volume` and `preview_commentary_volume`, read per block, so a live change is a field write.
- **The ramp and splice maths are pure functions in core,** testable with no GStreamer.
- **Why not an element graph:** it would be a second timeline that must agree with the video pump exactly. The parent spec already rejected that for export, and Phase 8 reuses this mixer.

### P5. Control and lifecycle

- **Commands:** `OpenPreview(clip_id)` and `ClosePreview`.
- **Exclusivity:** opening a preview pauses and unloads the source player; closing it restores scanning at the position it had. Preview is refused while recording or exporting, and recording is refused while previewing.
- **No cache, no polling.** macOS's 50 ms poll and 20 s timeout existed because AVFoundation compositions are slow to build; opening two decode pipelines is not. A spinner covers the ~100–300 ms preroll.
- **Events:** `Event::Preview(PreviewStatus::{Opening, Playing, Paused, Closed})`, plus errors through the usual path.
- **A missing source or recording file** refuses with a clear message.
- **Deleting the previewed clip** closes the preview first (Phase 3 already clears the selection).

### P6. UI

- **Selecting a clip** shows the inspector, as today. **Play** (button or Space) opens the preview for the selected clip.
- **While previewing:** the transport shows the clip's own timeline and the scrubber spans the clip. The Clips list, inspector and sidebar stay usable, except for the actions the guard refuses.
- **A "Previewing <name>" indicator** with a Close button, and Esc closes.
- **Drawing is off in preview** (Phase 6 captures only while recording).

---

## Crate responsibilities

| Crate | Phase 7 contents |
|---|---|
| `video-coach-core` | The audio splice and ramp maths (pure); the PiP and overlay layout ratios as pure functions. |
| `video-coach-media` | The shared composite builder (output size and GL context as parameters); `overlay.rs` (tiny-skia strokes); the preview tail (appsink into the mailbox); the second decode branch for the recording; the PCM audio mixer; seek handling. |
| `video-coach-app` | Bus: `OpenPreview` / `ClosePreview`, exclusivity, `Event::Preview`, transport routed to the preview while open. UI: Play opens a preview, the preview indicator and Close, Esc, the spinner. |
| `video-coach-harness` | Open a preview with fixtures, play, seek, close; the audio mix over a known fixture. |

## Testing

- **Core:** the splice and ramp maths (sample counts, envelope shape, a segment shorter than a ramp, and the clamp at t=0); the layout ratios.
- **Media:**
  - the overlay rasterizer against a golden image (strokes at known positions, and the alpha);
  - a preview graph running headless into an appsink, checked for frame-exactness against the schedule, exactly as the Phase 5 fiducial does;
  - the audio mixer's output over a fixture whose two tracks are distinguishable (for example a tone against silence), checking the gate and the ramps.
- **Harness:** open, play, seek, close; refusals while recording or exporting.
- **Manual** (batched): preview a real clip and confirm drawings, zoom, PiP and audio all line up with what was recorded.

## Gates (parent spec, "Phase 7 gate")

1. **The shared-context gate, first.** Before audio and strokes, get a 3-pad mixer's output onto the screen through Slint's GL context and **measure the UI's frame time**. Export sidesteps this with a private display; preview can't.
2. **Real footage:** the user's HEVC 1440p clip previews at 30 fps with no dropped frames.

## Risks

1. **Sharing Slint's GL context** between the mixer and Skia. This is the phase's main risk; gate 1 exists for it. If it can't hold 30 fps, the fallback is a private context plus a system-memory frame path for preview only, at a measured cost.
2. **Three timelines** (frame index, appsrc segment, audio mixer position) must agree across pause and scrub. The `seek-data` approach keeps the hand-written state small.
3. **Spliced source audio** is new code with no Phase 5 precedent, and A/V drift is hard to see in a test. Pure-function tests plus a mixed-PCM harness test mitigate it.

## Deferred

- The text bar and the scoreboard in the overlay: Phases 8 and 9.
- Playback-rate control and looping: macOS had neither.
- Drawing during preview.
