# Linux Port — Phase 5: Passthrough Export

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` ("Media pipelines → Export", "Export frame driver", "Encoder selection", Phasing → Phase 5)
**Evidence:** `docs/superpowers/spikes/2026-09-19-export-graph.md`, measured on the reference laptop with the user's HEVC 1440p30 footage.

---

## Goal

Export one clip to an MP4 of the game video **as the coach steered it**: plays, freezes, skips and zooms, at 1920×1080, 30 fps, H.264.

This phase adds no overlays, no picture-in-picture, no commentary audio and no scoreboard. Phase 8 adds those to this same graph. It is the port's first output file, and the first test of `playback_segments` against a real decoder rather than a unit test.

## Done when

1. A clip's context menu offers **"Export video…"**. It opens a save dialog, with the clip's name and `.mp4` suggested in the project folder.
2. The export runs in the background, and the transport's notice line shows "Exporting '<name>' — 42%" with a Cancel button. The app stays usable.
3. The output:
   - plays in common players;
   - is 1920×1080 at 30 fps, H.264 High in MP4 with `faststart`;
   - lasts exactly the clip's segment total;
   - shows each freeze on **the last source frame with PTS ≤ the anchor**;
   - pans smoothly (sub-pixel) when zoomed.
4. On the reference laptop it runs at least 2× realtime on the user's footage, zero-copy, with a hardware encoder. The log names the decoder, the encoder and the upload path.
5. Cancel, or an error, deletes the partial file.

---

## Measured facts (spike)

- **The graph.** The hybrid graph (decode pipeline → `appsink` → Rust pump → `appsrc` → GL → encoder) stays zero-copy under EGL. The upload path was `DirectDmabufExternal` for 450/450 frames. It ran at **108 fps (3.6× realtime)** on 60 s of 1440p HEVC with 2 s play / 1 s freeze segments and seeks.
  - Decode, pump and GL alone reach 303 fps, so the encoder bounds the export.
  - A `queue` before the encoder is required (71 → 108 fps).
- **Where to upload.** GL upload **inside the decode pipeline** (`… ! glupload ! glcolorconvert ! appsink` with RGBA GLMemory caps, the player's own sink) measured the same as a DMABuf boundary (106 vs 108 fps), and it negotiates on its own. A DMABuf boundary needs a VideoMeta-advertising appsink, and hangs a blocking `appsrc` when negotiation fails.
- **The encoder input needs a readback.** `vah264lpenc` won't take GL's output, so `glcolorconvert ! gldownload` to NV12 is required (~4 ms/frame at 1080p).
- **Freezes** re-push the held buffer with a new PTS. That is accepted, and costs the same per frame as play.
- **Sub-pixel pans.** `gltransformation` (float scale and translation) is smooth: step std 0.005 px. Integer `videocrop` stair-steps. `translation-x` is a fraction of the output width, applied after scale. The Y sign is unverified.
- **Frame identity.** 1800/1800 frames were exact against a burned-in counter, with "last frame with PTS ≤ s" computed on **stream time**.
  - Raw PTS is wrong: an MP4 with B-frames carries an edit list, and raw PTS runs 2 frames ahead.
  - Seconds → ns must **round** (`seconds_to_clock`, already fixed in the player).
- **Seek vs pull.** Pulling a frame forward costs about 1.3 ms. An accurate seek costs 12 ms (the camera footage) to 60–105 ms (a 2 s-GOP file). A forward-reuse threshold of **0.5 s**, not 2 s.
- **Encoders.** `vah264lpenc` is the only hardware H.264 encoder here, and it is **CQP-only**:

  | QP | Bitrate | PSNR |
  |---|---|---|
  | 22 | 15.4 Mbps | 43.6 dB |
  | 26 | 8.4 Mbps | 41.4 dB |
  | 30 | 4.9 Mbps | 39.3 dB |

  x264 `medium` runs at 0.39× realtime, so use **`veryfast`**. There is no `vapostproc` on this driver.

---

## Decisions

### X1. Core owns the edit: a frame schedule

Add a pure function to `video-coach-core/src/export.rs`:

```rust
pub const OUTPUT_FPS: u32 = 30;
pub struct FrameSpec { pub source_index: usize, pub source_time: f64, pub hold: bool, pub zoom: Zoom }
pub fn frame_schedule(clip: &Clip, source_duration: f64) -> Vec<FrameSpec>;   // one per output frame
```

- **Build:** one cumulative walk over `playback_segments(clip, dur)` builds the flat segment list. Frame `n` has output time `t = n / 30`; binary-search it (parent spec).
- **Play:** `source_time = source_start + (t − out_start)`, and `hold = false`.
- **Freeze:** `source_time` is the freeze anchor, and `hold = true`: repeat the previous frame if it is the same anchor.
- **Zoom:** `zoom_at(events, record_time = t)`.
- **Frame count:** `ceil(total_segment_duration × 30)`. A segment shorter than one frame interval gets no frame.
- **Fully unit-tested** in core: no media.

A clip is one `PlanEntry`. `compilation_plan` and `ExportTarget` stay for Phase 8, which concatenates schedules.

### X2. Media owns the pixels: `Exporter`

`video-coach-media/src/export/`:

```
decode (one per source used):
  filesrc ! parsebin ! decodebin3-or-explicit video decoder ! glupload ! glcolorconvert
          ! appsink caps=video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D
encode:
  appsrc(RGBA GLMemory, 30/1, is-live=false, format=time)
          ! gltransformation name=zoom ortho=true
          ! glvideomixer name=mix background=black   (one pad now: letterboxed fit rect in 1920×1080; Phase 8 adds pads)
          ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 ! gldownload ! queue
          ! <encoder> ! h264parse ! video/x-h264,stream-format=avc,alignment=au
          ! mp4mux faststart=true ! filesink
```

**The pump** runs on the exporter's own thread. For each `FrameSpec`:
- **Hold:** re-push the held buffer.
- **Otherwise:** fetch the frame from the decoder with the **last stream-time PTS ≤ `source_time`**:
  - pull forward if the target is ahead of the decoder and within **0.5 s**;
  - otherwise do an accurate seek (`seconds_to_clock`).
- **Push** `buffer.copy()` (a ref, not a pixel copy) with PTS `n/30` and duration `1/30`.

**Zoom** is applied by setting `gltransformation`'s scale and translation from a buffer probe on its sink pad, keyed on PTS, so the value matches the frame. The mapping from core's `Zoom` to `gltransformation`'s parameters is one pure function, tested against `Zoom::transform`, including the unverified Y sign.

**Letterbox:** the mixer pad's `xpos`/`ypos`/`width`/`height` are the source's fit rect inside 1920×1080, from the probed display aspect. The background is black.

**The GL context is the exporter's own:** a headless EGL display (`GLDisplayEGL`), never the UI's. That keeps the UI's vsync and paint stalls out of export (BACKLOG #36), and lets export run in the harness.

**Source time is stream time:** `segment.to_stream_time(pts)`.

**The software variant** (for CI, where there is no GPU): the same pump, with the decode ending `videoconvert ! appsink` and the encode using `videoconvertscale` + `compositor` + `x264enc`. It is selected like the player's `SinkKind` (`ExportKind::{Gl, Software}`). Its zoom crop is integer-only, which is fine for CI.

### X3. Encoder and quality

- **Probe order:** `vah264lpenc`, then `vah264enc`, then `nvh264enc`, then `x264enc`, taking the first that exists.
- **Quality is a quantizer, not a bitrate.** The Intel hardware encoder only does constant QP. This phase uses one fixed setting, "medium", which is **QP 24**:
  - VA: `rate-control=cqp qpi=24 qpp=24`;
  - nvh264enc: `rc-mode=constqp qp-const=24`;
  - x264: `pass=quant quantizer=24 speed-preset=veryfast`.
- **Phase 8** adds the Low/Medium/High picker (QP 28/24/20) and 720p/2160p.
- **The parent spec's bitrate ladder is superseded:** the CQP-only fact makes it unreachable on the reference hardware.
- **GOP:** `key-int-max=60` (2 s).

### X4. Control: an export job owned by the bus

- **`Command::ExportClip { id, path }`:** the bus builds the schedule from its project snapshot and the clip, resolves the source paths, and spawns an `Exporter` thread. One export at a time; a second is refused.
- **Events:** `Event::Export(ExportStatus::{Running { fraction, name }, Done { path }, Failed(String), Cancelled})`.
  - `fraction = frames_pushed / total_frames`, emitted at most about 5 times per second.
- **`Command::CancelExport`** stops the pump, sends EOS, sets NULL, and deletes the partial file. Errors delete it too.
- **Missing sources:** export needs the clip's source present. A missing one refuses with an error.
- **The app stays usable.** Export works on a snapshot (the schedule and paths), so editing, deleting or recording meanwhile doesn't affect it.
  - A delete of that clip can't pull the file out from under export, because export reads the **source** video, not the recording.
  - Recording while exporting shares the VA engine. That is allowed; it is expected to slow the export.
- **Shutdown** cancels a running export: the partial file is deleted and the thread joined.

### X5. UI

- **Clip context menu:** "Export video…". It opens an `rfd` save dialog with a default name and filter `*.mp4`, then sends `ExportClip`.
- **The notice line** shows the status while running, with a small **Cancel** button. It shows "Exported to …" or the error when finished, and clears after the usual timeout.
- Disabled while an export runs. A second export waits for the first to finish.

---

## Crate responsibilities

| Crate | Phase 5 contents |
|---|---|
| `video-coach-core` | `export.rs`: `OUTPUT_FPS`, `FrameSpec`, `frame_schedule`, and the `Zoom` → `gltransformation` parameter mapping (pure). |
| `video-coach-media` | `export/`: `Exporter` (the decode pipelines, pump, encode pipeline, encoder probe, headless EGL context), the `Gl` and `Software` kinds, cancel, and progress. |
| `video-coach-app` | Bus: `ExportClip`, `CancelExport`, `Event::Export`, and cancel on shutdown. UI: the menu item, save dialog, progress notice and Cancel. |
| `video-coach-harness` | An export end to end with the software kind. |

## Testing

- **Core:**
  - `frame_schedule`:
    - frame count;
    - play mapping;
    - a freeze anchor and hold;
    - skip jumps;
    - a sub-frame segment gets no frame;
    - zoom per frame;
    - a clip that starts on a freeze (the first frame is not a hold without a predecessor);
    - a clamp at the source end.
  - The zoom parameter mapping against `Zoom::transform`, at the centre and at the extreme pans.
- **Media** (software kind, generated fixtures, CI-safe):
  - **The fiducial test:**
    - A fixture where every source frame encodes its index in pixels, e.g. a grid of black and white blocks or a gray level per frame.
    - Export a clip with plays, a freeze and skips, then decode the output and check **every** output frame's index against the schedule's expectation.
    - This is the test that catches off-by-one frames. The spike's version was exact 1800/1800.
  - The output's duration, frame count, 1920×1080, 30/1 and H.264 in MP4.
  - Cancel mid-export deletes the file.
  - A missing source is an error.
- **Media, GL kind** (runs where a GPU and EGL exist; skipped otherwise):
  - the same fiducial test;
  - the log shows the zero-copy upload path;
  - fps at least 2× realtime on a generated 1080p file.
- **Harness:** `ExportClip` → Running events → Done, the file exists and has the right duration; `CancelExport` → Cancelled, and no file.
- **Manual** (batched): export a real clip, watch it, and check a slow zoom pan for smoothness.

## Risks

1. **The `gltransformation` Y sign and parameter semantics** are only half-verified. The pure mapping plus the GL fiducial test (a zoomed quadrant) settle it.
2. **Headless EGL on other machines.** On the reference laptop, `GLDisplayEGL` works without a window. Elsewhere it might need `surfaceless` or `gbm`. If the GL kind fails to initialise, fall back to the software kind and log it.
3. **CQP bitrate varies with content.** A long freeze-heavy clip is small; a busy pan is large. That is accepted, and export targets YouTube, which re-encodes.

## Deferred

- Overlays, PiP, audio, the scoreboard, per-tag and all-clips compilations, the quality and resolution picker: Phase 8.
- HEVC output: parent spec, "not in the first cut".
