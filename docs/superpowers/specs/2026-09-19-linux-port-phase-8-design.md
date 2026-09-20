# Linux Port — Phase 8: Full Export

**Date:** 2026-09-19
**Status:** Reviewed (simplify and correctness passes applied; the mixer pads, caps changes, AAC alignment, text scaling and throughput were measured on the reference laptop)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` ("Media pipelines → Export", "Export audio", "The compositor decision", Phasing → Phase 8)
**Builds on:** Phase 5 (the export tail and pump), Phase 7 (the composite, overlay, PiP and layout), Phase 3 (tags)
**Evidence:** the macOS inventory of `CompilationExporter.swift`, `ExportSheet.swift`, `ExportProgress.swift` and `CompilationCompositor.swift`; the Phase 8 research and review measurements below.

---

## Goal

Export **compilations**: all clips, one tag's clips, or a single clip, each as one MP4, with the webcam picture-in-picture, the drawings, the text bar and mixed audio burned in, at a chosen resolution and quality, with honest progress and a working cancel.

## Done when

1. **Targets.** An export sheet lists **All clips**, one row per tag, and the selected clip; all are ticked by default except the single clip. Each ticked target produces one MP4.
2. **The picture.** Each frame carries the game video with zoom, the webcam PiP (when the clip's `show_pip` is on), the drawings, and the text bar `"<n> / <total> | <name> | tag1, tag2"`, where `<total>` is that target's clip count.
3. **The audio.** Game audio during play segments only, commentary throughout, 5 ms fades at every region edge, and **no systematic offset** between picture and sound.
4. **Quality.** Resolution (720p / **1080p**) and quality (Low / **Medium** / High) are chosen in the sheet and persist. Quality is a quantizer.
5. **Progress.** Per-target progress in exact frames, a rate, time left and a finish time, and **Cancel works**.
6. **Files.** `<project>/exports/<label> - <project>.mp4`, written as `.part` and renamed.

---

## Measured facts (reference laptop)

- **Throughput,** 600 frames of the user's HEVC 1440p source, QP 24, with a real trim and the output duration asserted: **3 pads 48.8–49.4 fps, 4 pads 47.7–49.7 fps at 1080p** — the extra pad is free, and the composite runs ~1.6× realtime. 2160p measured 0.56× realtime.
- **Per-pad `zorder` is honoured,** and a small RGBA strip placed at the bar's rect composites correctly under a full-frame overlay.
- **Mid-stream caps changes are accepted** by every appsrc; `gltransformation` and `glvideomixer` renegotiate cleanly. **Geometry is the race, not caps:** setting pad rects from the pushing thread applied them **up to `QUEUED` (4) frames early**, so the last frames of an entry took the next entry's layout. Keying geometry to the buffer's PTS in a pad probe fixed it (the pattern `install_zoom` already uses).
- **An unfed mixer pad stalls everything:** a requested pad that never receives a buffer produced **0 output frames** and backed up the base appsrc, with no error (`wait_for_room` has no deadline). EOS on that pad mid-run is safe: without `repeat-after-eos` the PiP disappears, with it the last frame holds.
- **AAC priming shifts the sound:** a tone at exactly 1.000 s came back at **1.0214 s** through `avenc_aac`, and the audio track is 1024 samples (21.3 ms) longer than the video, with `media_time = 0` in the edit list. `voaacenc` is worse (+33.4 ms, and it drops samples).
- **`avenc_aac` is in `gstreamer1.0-libav`, which CI does not install.** `aacparse` is in plugins-good.
- **`mp4mux` tolerates differing track durations** and `reserved-max-duration` alone puts `moov` before `mdat`.
- **Text scales by ratio** (0.674 of the width at 720p, 1080p and 2160p), and `Family::SansSerif` resolves with only the embedded TTF loaded. But a realistic 143-character line **wraps to two lines at every resolution**, and the second line lands outside the bar, over the picture. macOS clipped to the bar rect. Cost: 1.64 ms at 1080p, including re-shaping every frame.
- **Measurement trap:** `glvideomixer` ignores `identity eos-after=N` and runs to the demuxer's segment end (600 "frames" took 88.9 s and produced an unreadable file). Every number above comes from a real trim with the output duration asserted.
- **macOS facts:** Quality was a no-op and `ExportSettings.bitrate` had no production caller; ramps were interior-only and never on the mic; freezes are silent; volumes come from the preview preferences; export was sequential and had **no cancel**; `<total>` counts that target's clips; macOS had no 2160p and exported HEVC.

---

## Decisions

### E1. One export path: a single clip is a target

`ExportTarget` gains `Clip(Uuid)` beside `AllClips` and `Tag(String)`.

- **`compilation_schedule(project, target) -> Compilation` replaces `frame_schedule`,** which is deleted along with the single-source `ExportJob`. Phase 5's per-clip export becomes a one-entry compilation, so there is one job type, one progress model and one cancel.
- **It is built on `compilation_plan`,** not a second walk over the clips, so the plan's duration and the frame count can't disagree.
- **Entries are quantized to whole output frames:** each entry's schedule is `frame_schedule`'s walk, and the next entry starts at the next frame boundary. That keeps "record time is output time" exact.
- **`FrameSpec { entry, source_time, zoom }`.** `source_index` and `record_time` are derived from the entry and the frame index at the one call site, rather than stored three times.
- **`Entry { clip_id, source_index, recording, start_frame, frames, text, show_pip }`,** where `text` is `"<n> / <total> | <name> | tags"` with empty parts collapsed and `<total>` the target's clip count.

### E2. Three pads, fixed geometry except the base

| Pad | z | Content |
|---|---|---|
| 0 | 0 | the pumped source frame through `gltransformation` (zoom), at **that entry's** fit rect |
| 1 | 1 | the webcam PiP |
| 2 | 2 | the overlay: drawings, the bar's background and its glyphs, at the **output size** |

- **One overlay layer, not two.** Strokes are mapped into the entry's fit rect **inside** the output-size overlay (line width still from the picture's height), and the bar is drawn in output space. That keeps Phase 7's rule (strokes belong to the picture, chrome to the frame) without splitting a layer across two z-levels.
- **The PiP sits above the bar, not over it:** its bottom margin becomes `bar height + margin`, one constant in `layout.rs`. macOS split the bar's background and glyphs across two layers precisely because its PiP overlapped the bar; moving the PiP removes the need.
- **The PiP pad is fed every frame, always.** A clip with `show_pip` off, or a recording that is missing or video-less, pushes a 1×1 transparent RGBA. An unfed pad stalls the export silently (measured). A recording that runs short **holds its last frame** (`repeat-after-eos`).
- **Geometry is keyed to PTS in pad probes,** never set from the pushing thread: with `QUEUED = 4` a direct set lands up to four frames early (measured). Only pad 0's rect and caps change per entry; pads 1 and 2 are fixed for the run.
- **One `Decoder` per distinct source**, alive for the whole compilation; one per entry's recording, opened and closed with the entry.

### E3. Audio

- **One audio-only pipeline per file** (each source video, each recording), ending in an appsink at F32LE/48k/2ch. This avoids Phase 7's measured deadlock by construction, so the drain-first rule isn't needed; audio decode runs ~90× realtime.
- **Rust mixes per output block,** interleaved with the video pump: audio for frame range [n, n+k) is pushed **at or ahead of** those video frames, never behind.
  - **The audio appsrc is effectively unbounded** and the pump never waits for room on it. Measured: bounding it at 0.27 s deadlocks the pump, because the encoder keeps the muxer ~0.43 s behind the pushed video; pushing audio 1 s behind stalls the video appsrc (`QUEUED` is only 0.13 s); pushing it all after the last frame stalls it too. The right bound depends on the encoder's latency, so there is no tuned constant.
- **The game's audio plays only during `play` segments.** Each play segment seeks its source's audio pipeline to the segment's source time, rather than holding decoded audio (F32/48k/2ch is 384 KB/s, so an hour-long match would be ~1.4 GB).
  - Those seeks are **flushing and ACCURATE**, which measured 1–11 ms and sample-exact. They must not reuse `Decoder::seek`'s `KEY_UNIT | SNAP_BEFORE`, which lands early.
  - **A file with no audio track contributes silence**, rather than failing or stalling the muxer.
- **5 ms linear fades at the start and end of every contiguous region on either track,** clamped at t=0 — the parent spec's uniform rule. macOS clicked at every clip join and mic start, and no macOS test pinned that.
- **AAC priming is compensated by dropping the first 1024 samples** of the mixed stream (21.3 ms, measured).
  - Shifting the timestamps and clamping at zero **does nothing**: the encoder re-derives its output times by counting samples from the first buffer, so the shift is absorbed and the tone still lands at 1.0214 s. Dropping the samples puts it at exactly 1.000 s and the track's duration back to the video's.
  - **The ramps are computed on the emitted timeline,** after the drop, or the first region starts mid-fade — a click, which is what the ramp rule exists to remove.
  - A known-tone fixture pins it: a tone at 1.000 s must decode back within a millisecond.
- **Volumes** come from `preview_source_volume` and `preview_commentary_volume`, both defaulting to 1.0. There is no UI for them yet (backlog).
- **The splice, gain and ramp maths are pure functions in core.**

### E4. Quality and resolution

- **Resolution: 720p or 1080p (default).** **2160p is dropped:** it runs at 0.56× realtime and only upscales the user's 1440p footage. `Resolution::R2160` stays in the format for later.
- **Quality is a quantizer:** Low/Medium/High → QP 28/24/20, for both `vah264lpenc` and `x264enc pass=qual`. The macOS bitrate table is not ported.
- Both persist in the existing `Preferences` fields.

### E5. Progress, ETA and running

- **Sequential**, one target at a time: a single export already saturates the GPU.
- **Progress is exact frames,** reported as `frames_done` of `frames_total` per target, not a percent (the UI derives the percent).
- **`ExportRun`** holds the targets with their frame counts, plus a rate over a trailing window:
  - remaining wall time = remaining frames ÷ rate, for the current and the pending targets alike (they share an output size, so one rate applies);
  - **no ETA until the rate is stable** (macOS's ≥5 samples and ≥2 s, which is about rate stability, not progress accuracy);
  - macOS's monotonic clamp is **not** ported: it existed for AVFoundation's overshoot.
- **Cancel works.** It stops after the current frame, deletes the `.part`, and leaves already-finished targets alone.
- **A missing source or recording is refused up front, naming the clip,** rather than failing a long run halfway.

### E6. Files

`<project>/exports/`, created on demand; `<label> - <project>.mp4` with `/` and `:` replaced; `.part` then rename. Re-running replaces the file. **There is no folder picker**: the exports folder is fixed, and the finished state offers to open it.

### E7. Preview gains the text bar

Preview draws the text bar with `n / total = 1 / 1`, so the picture keeps matching export.

**Preview's game audio is deferred** (BACKLOG #54). Phase 7 made the recording's native branch the pipeline clock and the only volume-controlled element, so adding a second, pumped audio track there means a `audiomixer` pad that stalls the graph if it is ever unfed, a feedback loop between the pump and the clock it is paced by, seek and EOS handling for a third appsrc, and a scrub mute that currently only silences the commentary. Export is what gets sent to players, so it takes the audio work; preview keeps commentary-only playback until that is designed on its own.

### E8. UI

An **Export…** button opens a sheet:
- **Targets:** All clips, each tag with its clip count and total length, and the selected clip; all ticked by default except the clip.
- **Resolution** and **Quality** pickers.
- **A run list:** one row per target — pending, a progress bar, or done with its encode time and average fps.
- **A run line:** "<M:SS> of video left · ETA <M:SS> (finishes at 3:42 PM)", hidden until the rate is stable.
- **Export** and **Cancel**. The old per-clip menu item opens the sheet with that clip ticked.

---

## Crate responsibilities

| Crate | Phase 8 contents |
|---|---|
| `video-coach-core` | `compilation_schedule` / `Compilation` / `Entry` on top of `compilation_plan`; the text line; the audio splice, gain and ramp maths; `ExportRun` (the rate window and projection); the bar's layout ratios and the PiP's raised margin. |
| `video-coach-media` | The three-pad export tail with PTS-keyed geometry; the PiP decoder and its transparent filler; the bar and glyph rendering (cosmic-text plus a vendored TTF, `Wrap::None` with a tail ellipsis); the audio pipelines, the mixer plumbing, the AAC branch and its priming shift; resolution and quality parameters. |
| `video-coach-app` | Bus: compilation exports, the target list, frame-count progress, cancel. UI: the export sheet and run list. |
| `video-coach-harness` | A compilation export end to end. |
| CI | Add **`gstreamer1.0-libav`** for `avenc_aac`. |

## Testing

- **Core:** `compilation_schedule` (entry order, whole-frame quantization, derived record time, a tag target, an empty target); the text line; the audio splice (sample counts, the ramp envelope, a region shorter than a ramp, silence during freezes); `ExportRun`'s rate gate and projection.
- **Media:**
  - **A multi-clip fiducial:** two counter fixtures of different sizes and frame rates as one compilation, with every output frame's counter checked. This catches per-entry geometry and concatenation errors, including the four-frame-early race.
  - The three-pad composite over a synthetic base: the PiP rect (above the bar), strokes mapped into the fit rect on a non-16:9 entry, the bar, and premultiplied alpha.
  - **An A/V alignment test:** a tone at a known time decodes back within a millisecond (this is what catches AAC priming; a gate-and-ramp test passes while being 21 ms late).
  - A long text line is clipped to one line with an ellipsis.
  - `show_pip` off, and a recording shorter than its entry.
  - The output's duration matches the schedule (the mixer trap).
- **Harness:** export two targets; progress rises and completes; cancel leaves the finished target alone; a missing source is refused naming the clip.
- **Manual** (batched): export a real compilation and watch it.

## Risks

1. **Audio alignment** is the subtlest thing here: the priming shift is measured, but the tone test is what keeps it honest.
2. **Per-entry geometry** on pad 0, which the fiducial's differently-sized sources exist to catch.
3. **Text rendering** is new; glyph placement and clipping need pixel tests.

## Deferred

- HEVC output; a combined single-file export across targets; per-clip export settings.
- **UI for the two volumes** (they are read from the preferences, which default to 1.0).
- 2160p (kept in the format, not offered).
