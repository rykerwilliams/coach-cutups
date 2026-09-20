# Linux Port — Phase 8: Full Export

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` ("Media pipelines → Export", "Export audio", "The compositor decision", Phasing → Phase 8)
**Builds on:** Phase 5 (the export tail, the pump, the schedule), Phase 7 (the composite, the overlay, the PiP and layout), Phase 3 (tags)
**Evidence:** the macOS inventory of `CompilationExporter.swift`, `ExportSheet.swift`, `ExportProgress.swift` and `CompilationCompositor.swift`; the Phase 8 research measurements, recorded below.

---

## Goal

Export **compilations**: every clip, or every clip carrying a tag, as one MP4 each, with the webcam picture-in-picture, the drawings, the text bar and mixed audio burned in, at a chosen resolution and quality, with honest progress and an ETA.

This is the last piece of the coach's loop: record, review, export, send.

## Done when

1. **Targets.** An export sheet lists **All clips** plus one row per tag, all checked by default. Each selected target produces one MP4.
2. **The picture.** Each output frame carries: the game video with zoom, the webcam PiP (when `show_pip`), the drawings, and the text bar `"<n> / <total> | <name> | tag1, tag2"`.
3. **The audio.** The game's audio plays during play segments only, the commentary throughout, both at the preview volumes, with 5 ms fades at every region edge, so no boundary clicks.
4. **Quality.** Resolution (720p / **1080p** / 2160p) and quality (Low / **Medium** / High) are chosen in the sheet and persist. Quality is a quantizer, not a bitrate.
5. **Progress.** The sheet shows per-target progress, the current rate, time left and a finish time, and **Cancel works**.
6. **Files.** `<project>/exports/<label> - <project>.mp4`, written through `.part` and renamed.

---

## Measured facts (reference laptop)

**Composite throughput,** 600 frames from the user's HEVC 1440p source, QP 24, one pipeline:

| Pads | 720p | 1080p | 2160p |
|---|---|---|---|
| Mixer, 1 pad + encode | 126 fps | 77 fps | 26 fps |
| + overlay pad | 107 | 60 | 17 |
| + PiP pad (3 pads) | 85 | **52 (1.74× realtime)** | **17 (0.56×)** |

- **The mixer itself is free;** a full-frame RGBA overlay pad costs about 23% at 1080p, 35% at 4K.
- **Audio costs about 6%** of the 1080p composite. Decoding audio runs at ~90× realtime, and AAC encoding at ~13.5×.
- **`mp4mux` takes AAC beside the video** with `avenc_aac ! aacparse`.
- **Text** (cosmic-text + tiny-skia, 68 characters): **1.64 ms** at 1080p, 6.46 ms at 4K. Re-shaping every frame costs 0.26 ms, so no caching is needed. Clearing the RGBA layer costs 0.67 ms at 1080p.
- **Fonts:** scanning system fonts costs 309 ms and varies by distro; one embedded TTF costs 24 ms. `DejaVuSans.ttf` is 741 KiB.
- **2160p is 0.56× realtime** and upscales the only footage the user has.
- **Measurement trap:** `glvideomixer` ignores `identity eos-after=N` and keeps emitting to the demuxer's segment end (150 input frames produced a 4m23s file). Cut fixtures with a real trim, and assert the output's duration.
- **macOS facts worth keeping:**
  - Quality was a **no-op**: the preset ignored it and `ExportSettings.bitrate` had no production call site. Don't port the bitrate table.
  - Ramps were only at interior boundaries within an entry, never at clip joins and never on the mic. No test pinned that.
  - Freezes are silent by design.
  - Volumes come from the **preview** preferences.
  - Export was sequential with one reused exporter, and **had no cancel**.
  - Filenames collide and overwrite.

---

## Decisions

### E1. Core builds the compilation schedule

`frame_schedule` grows into a compilation:

```rust
pub struct FrameSpec {
    pub source_index: usize,
    pub source_time: f64,
    pub zoom: Zoom,
    pub entry: usize,        // which clip in the compilation
    pub record_time: f64,    // t − entry_out_start, for strokes and the PiP
}
pub fn compilation_schedule(project: &Project, target: &ExportTarget) -> Compilation;
// Compilation { frames: Vec<FrameSpec>, entries: Vec<Entry> }
// Entry { clip_id, source_index, recording: String, out_start_frame: u64, text: String, show_pip: bool }
```

- It walks `compilation_plan`'s entries in `sort_index` order, concatenating each clip's segments onto one output clock.
- **`record_time` is output time minus the entry's start** (the parent spec's rule), which drives the strokes, the PiP frame and the zoom.
- **The text line** is `"<n> / <total> | <name> | tag1, tag2"`, with empty parts collapsed (macOS parity).
- Fully unit-tested in core: no media.

### E2. The video graph gains three pads

The export tail becomes the preview's shape, at full resolution and offline:

| Pad | z | Content |
|---|---|---|
| 0 | 0 | the pumped source frame through `gltransformation` (zoom), placed at that **entry's** fit rect |
| 1 | 1 | the text bar's **background** (a small RGBA strip, bar height only) |
| 2 | 2 | the webcam PiP, pumped from the entry's recording |
| 3 | 3 | the overlay: drawings **and** the bar's glyphs, full-frame RGBA |

- **Why two overlay layers.** macOS drew the bar's background below the PiP and its glyphs above it. With one layer either the bar tints the bottom quarter of the PiP, or the PiP hides drawings that fall under it. The background strip is cheap (bar height, not full frame), so both rules are kept.
- **The PiP is pumped, not played natively.** Export is offline and each entry has its own recording, so a `Decoder` per entry's recording feeds the PiP appsrc, stamped with the output frame's PTS. Preview's native-playback trick only works for a single clip in real time.
- **Per-entry geometry.** The fit rect, the appsrc caps and the overlay size are computed **per entry**, not from the first sample, since a compilation can span sources of different sizes. Pads are re-placed at entry boundaries.
- **One `Decoder` per distinct source**, kept alive across the compilation, so a tag whose clips interleave two sources doesn't reopen files.

### E3. Audio: one audio-only pipeline per file, mixed in Rust

- **Each source video and each recording gets its own audio-only pipeline** ending in an appsink at F32LE/48k/2ch.
  - This sidesteps the Phase 7 deadlock (an audio appsink on the pumped video branch stalls) **by construction**, so the drain-first rule isn't needed. Audio decode is ~90× realtime, so the extra pipelines cost about 1%.
- **Rust mixes** per output block: the game's audio only during `play` segments, the commentary for the whole of each entry, each at its preview volume (`preview_source_volume`, `preview_commentary_volume`).
- **5 ms linear fades at the start and end of every contiguous region on either track** (the parent spec's uniform rule), clamped at t=0. macOS ramped only inside an entry, so clip joins and every mic start clicked. Nothing in its tests pinned that, so the better rule wins, and this phase writes the first real ramp test.
- **The mixed stream** goes through one appsrc → `audioconvert` → `avenc_aac bitrate=192000` → `aacparse` → `mp4mux`.
- **The splice and ramp maths are pure functions in core,** tested with no GStreamer.

### E4. Quality and resolution

- **Resolution:** 720p, **1080p** (default), 2160p. The output size parameterizes the composite, as Phase 7 made it.
- **Quality is a quantizer:** Low/Medium/High → **QP 28/24/20** for `vah264lpenc`, and the same numbers as `x264enc pass=qual quantizer=`. `ExportSettings.bitrate` is **not** ported: it never reached an encoder on macOS.
- **Both persist** in `Preferences`, whose `Resolution` and `Quality` fields already exist and have no consumers yet.
- **2160p is allowed but slow** (0.56× realtime) and upscales the user's 1440p footage. The ETA tells the truth rather than the UI refusing.

### E5. Progress, ETA and running

- **Sequential**, one target at a time. A single export already saturates the GPU (GL and VA encode contend: 87 fps without the encoder, 52 with), so parallel targets would not help.
- **`ExportProgress` ports** from macOS:
  - `RollingRate` over a 30 s window, with its "no ETA until the rate is stable" rule (≥5 samples and ≥2 s);
  - `RunProjection`'s `totalSecondsRemaining`, per-item remaining and finish time;
  - **dropping** the clamp for AVFoundation's progress overshoot: the port's progress is exact frame counts.
  - Its tests port too.
- **Cancel works,** unlike macOS. It stops after the current frame, deletes the `.part` and leaves earlier targets' finished files alone.

### E6. Files

- **`<project>/exports/`**, created on demand.
- **`<label> - <project>.mp4`**, with `/` and `:` replaced (macOS parity). `All clips` is the label for the all-clips target.
- **Written as `.part` and renamed,** so a cancelled or failed export leaves nothing and an overwrite is atomic.
- Overwriting an existing file is expected: re-running an export replaces its output.

### E7. UI

An **Export…** button in the transport opens a sheet:
- **Targets:** All clips, then each tag with its clip count and total length, all checked by default.
- **Resolution** and **Quality** pickers, and the output folder with a Change… button.
- **Run list:** one row per target with its state (pending, a progress bar, or done with its encode time and average fps).
- **A run line:** "<M:SS> of video left · ETA <M:SS> (finishes at 3:42 PM)", suppressed until the rate is stable.
- **Export** and **Cancel**.
- Recording and preview are refused while an export runs, as now.

---

## Crate responsibilities

| Crate | Phase 8 contents |
|---|---|
| `video-coach-core` | `compilation_schedule` and `Compilation`; the text line; the audio splice, gain and ramp maths; `ExportProgress` (`RollingRate`, `RunProjection`); the bar's layout ratios. |
| `video-coach-media` | The export tail's four pads with per-entry geometry; the PiP decoder; the bar background and glyph rendering (cosmic-text plus an embedded TTF); the audio pipelines, the mixer's plumbing and the AAC branch; quality and resolution parameters. |
| `video-coach-app` | Bus: compilation exports, the target list, progress and ETA events, cancel. UI: the export sheet, the run list, the pickers. |
| `video-coach-harness` | A compilation export end to end with fixtures. |

## Testing

- **Core:**
  - `compilation_schedule`: entry order, `record_time` per entry, the frame count, a tag target, an empty target;
  - the text line, including collapsed empty parts;
  - the audio splice: sample counts, the ramp envelope, a region shorter than a ramp, the clamp at t=0, and freezes being silent;
  - `RollingRate` and `RunProjection`, ported.
- **Media:**
  - **A multi-clip fiducial:** two counter fixtures at different sizes and frame rates, exported as one compilation, with every output frame's counter checked against the schedule. This is the test that catches per-entry geometry and concatenation errors.
  - The four-pad composite over a synthetic base: the PiP rect, the bar background under the PiP, glyphs and strokes above it, and premultiplied alpha.
  - The audio: a mixed output where the two tracks are distinguishable (a tone against silence), checking the gate and the ramps.
  - Output shape per resolution, and that the file's duration matches the schedule (the mixer trap above).
- **Harness:** export two targets; progress rises and completes; cancel leaves the finished target alone and no `.part`; refusals.
- **Manual** (batched): export a real compilation and watch it.

## Risks

1. **2160p at 0.56× realtime.** Accepted; the ETA is honest.
2. **Per-entry geometry** is the most likely source of a subtle bug, which is why the fiducial uses two differently-sized sources.
3. **Text rendering** is new (cosmic-text plus an embedded font). Its cost is measured and small, but glyph placement needs a pixel test.
4. **A long compilation** holds one decoder per source plus one per entry's recording. Recordings are opened and closed per entry, so only the source decoders accumulate.

## Deferred

- HEVC output.
- A combined single-file export across targets (macOS wrote one file per target, and so does this).
- Per-clip export settings.
