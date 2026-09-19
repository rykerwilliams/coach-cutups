# Linux Port — Phase 5: Passthrough Export

**Date:** 2026-09-19
**Status:** Reviewed. Simplification and correctness passes are applied; the correctness pass tested the graph, the zoom mapping, headless EGL and llvmpipe on the reference laptop.
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` ("Media pipelines → Export", "Export frame driver", "Encoder selection", Phasing → Phase 5)
**Evidence:** `docs/superpowers/spikes/2026-09-19-export-graph.md`, plus the review's measurements, recorded below.

---

## Goal

Export one clip to an MP4 of the game video **as the coach steered it**: plays, freezes, skips and zooms, at 1920×1080 and 30 fps, in H.264.

This phase adds no overlays, no picture-in-picture, no commentary audio and no scoreboard. Phase 8 adds those to this same graph. This is the port's first output file, and the first test of `playback_segments` against a real decoder.

## Done when

1. A clip's context menu has **"Export video…"**. It opens a save dialog, with the clip's name and `.mp4` suggested in the project folder.
2. The export runs in the background. The transport shows a progress bar and **Cancel**, and the app stays usable.
3. The output:
   - plays in common players;
   - is 1920×1080 at 30/1, H.264 High, in an MP4 with its `moov` atom at the front;
   - lasts the clip's segment total rounded up to whole frames;
   - shows each freeze on **the last source frame with stream-time PTS ≤ the anchor**;
   - pans smoothly (sub-pixel) when zoomed.
4. On the reference laptop it runs at least 2× realtime on the user's footage, zero-copy, with the hardware encoder. The log names the decoder, the encoder and the upload path. This is a manual gate, not a test.
5. Cancel or an error leaves no partial file and never touches an existing file at the target path.

---

## Measured facts

From the spike and the review, on the reference laptop:

- **The graph and its speed.**
  - Decode with the GL upload **inside** the decode pipeline (the player's GL sink bin), feeding a pull `appsink`; a Rust pump; then `appsrc` → `gltransformation` → `glvideomixer` → `glcolorconvert` NV12 → `gldownload` → `queue` → `vah264lpenc`.
  - It is zero-copy (`DirectDmabufExternal` 307/307) at **107–113 fps**, 3.6× realtime on 1440p HEVC. The mixer costs nothing measurable.
  - The `queue` before the encoder is required. The readback before `vah264lpenc` is required (~4 ms per frame).
- **The GL display.**
  - Use **`GLDisplayEGL::new_surfaceless()`** (gstreamer-gl-egl `v1_24`). It works with and without `DISPLAY`, and stays zero-copy.
  - `GLDisplayEGL::new()` fails without a display server.
  - Plain `GLDisplay::new()` picks GLX and copies every frame.
- **llvmpipe** (`LIBGL_ALWAYS_SOFTWARE=1`, surfaceless, no DISPLAY) runs the **same graph**. That was verified end to end with VP8 decode and x264 encode.
- **The mixer.**
  - Its output size is the bounding box of its pads, and its frame rate comes from the input caps. Both must be **pinned** after the mixer: `width=1920,height=1080,framerate=30/1`.
  - A 60 fps source otherwise produced a 60/1 file with every frame doubled.
- **The zoom mapping,** with `gltransformation` before the mixer, where its output is the source resolution:
  - `scale-x = scale-y = s`, `translation-x = −pan_x·s`, `translation-y = −pan_y·s`. Translation is in units of the picture's width and height, applied after the scale; positive `translation-y` moves the image **down**.
  - *Corrected in Task 2.* Offered the choice, `gltransformation` doesn't render: it passes the frame through with an affine-transformation meta, and `glvideomixer` draws the transformed quad **unclipped**, so a zoomed 4:3 source spilled into its bars. The review measured that path, where `translation-y` has the opposite sign. The exporter strips that meta from `gltransformation`'s allocation answer, so it renders into its own source-sized texture and the zoom is clipped to the fit rect.
  - Verified on the render path: s = 0.25 and 0.5 with translations on 4:3 and 16:9, and s=2 with pan (0.2, −0.2) on a 4:3 source letterboxed into 1920×1080 (the media test), with the bars black.
- **Sub-pixel pans:** `gltransformation` is smooth (step std 0.005 px).
- **Frame identity:** 1800/1800 frames exact against a burned-in counter.
  - Source time is **stream time** (`segment.to_stream_time(pts)`). An edit list in an MP4 with B-frames puts raw PTS 2 frames ahead.
  - Seconds → ns **rounds** (`seconds_to_clock`).
- **Seek vs pull:** pulling a frame forward costs about 1.3 ms; a seek, which decodes from the keyframe before its target, costs 12 ms (camera footage) to 60–105 ms (a 2 s GOP). The forward-reuse threshold is **0.5 s**.
- *Corrected in the code review:* **an accurate seek can miss the frame.** The decoder drops every frame whose duration ends before the target, so a target in a gap between frames (VFR MKV/WebM, a dropped frame) got the frame after it. And past the video's end, where the audio runs on, it found no frame at all. The pump seeks to the keyframe at or before the target (`KEY_UNIT | SNAP_BEFORE`) and pulls forward to the last frame ≤ target; past the end, that is the video's last frame. Both cases are media tests.
- **Encoders.**
  - `vah264lpenc` is the only hardware H.264 encoder here, and it is **CQP-only**. Its properties are `rate-control`, `qpi`, `qpp` and `key-int-max`, and it emits no B-frames.
  - QP 22/26/30 gives 15.4/8.4/4.9 Mbps.
  - x264 `medium` runs at 0.39× realtime, so use `veryfast`.
  - Both give High profile. Pinning `profile=high` negotiates.
- **`mp4mux faststart=true`** writes the whole `mdat` to a temp file in `$TMPDIR`, which a crash leaks. `reserved-max-duration` instead writes `ftyp free moov free mdat` in place, with no temp file.
- **Recording during an export.** A live VA encode, standing in for the recorder, dropped from 371 to 273 frames in 14 s while an export ran; the export stayed at 111 fps. **The recording suffers, not the export.**
- **CI decode:** `openh264dec` (plugins-bad) decodes x264 High with B-frames, so CI can read the output back without libav.

---

## Decisions

### X1. Core owns the edit: a frame schedule

`video-coach-core/src/export.rs`:

```rust
pub const OUTPUT_FPS: u32 = 30;
pub struct FrameSpec { pub source_time: f64, pub zoom: Zoom }
pub fn frame_schedule(clip: &Clip, source_duration: f64) -> Vec<FrameSpec>;
```

- **Frames.** Output frame `n` exists at `t = n/30` for every `n` with `t` inside the segment total. Compute the count as `ceil(total·30 − 1e-6)`, so float noise such as `8.3·30 = 249.00000000000003` doesn't add a frame.
  - A segment gets a frame **if and only if it contains some `n/30`**. A segment shorter than a frame interval can still get one.
- **Lookup.** A forward walk over `playback_segments`, since output times only increase.
- **Play:** `source_time = source_start + (t − out_start)`.
- **Freeze:** `source_time` is the freeze's anchor, which is the Pause event's captured source time.
- **Zoom:** `zoom_at(events, t)`.
- **No play/freeze flag.** The pump answers "last frame with PTS ≤ `source_time`" from its cached current frame, so a repeated `source_time` re-pushes the same buffer at no cost. That one rule covers freezes, 25→30 fps duplication and 60→30 fps drops.
- **The clip's `source_index`** is constant, so it is passed once, not per frame.

`compilation_plan` and `ExportTarget` stay for Phase 8, which concatenates schedules.

### X2. Media owns the pixels: one GL graph everywhere

`video-coach-media/src/export/`:

```
decode:  filesrc ! decodebin3 (video pad selected by caps)
         ! [the player's gl_bin: glupload ! glcolorconvert] ! appsink (pull mode, RGBA GLMemory 2D)
encode:  appsrc (the decode caps rewritten to framerate=30/1, format=time)
         ! gltransformation name=zoom ortho=true
         ! glvideomixer name=mix background=black
         ! video/x-raw(memory:GLMemory),width=1920,height=1080,framerate=30/1
         ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 ! gldownload ! queue
         ! <encoder> ! h264parse ! video/x-h264,profile=high,stream-format=avc,alignment=au
         ! mp4mux reserved-max-duration=<schedule + margin> ! filesink location=<path>.part
```

- **There is one graph, and no software variant.** The exporter owns its GL display, so it doesn't depend on the UI. On a machine without a GPU, Mesa's llvmpipe runs the same graph, CI included. That way CI tests the shipping graph: its sub-pixel zoom, its mapping, its letterbox.
  - Without EGL at all, export fails with a clear error. It never silently falls back to a stair-stepping crop.
- **One GL display and context** (`GLDisplayEGL::new_surfaceless()`, plus a `GLContext` created from it) is shared by both pipelines. *Corrected in Task 2:* it is one per process, created on first use and never dropped. Every surfaceless display wraps the same `EGLDisplay`, and finalizing one calls `eglTerminate` for all, so exports running side by side failed as soon as one finished. Each pipeline's bus sync handler answers `NeedContext` for `gst.gl.GLDisplay` and `gst.gl.app_context`, so the GL memory crossing appsink → appsrc belongs to one context.
- **The decode builder reuses the player's `gl_bin`,** made `pub(crate)`, rather than duplicating it.
- **The letterbox** is the mixer pad's `xpos`/`ypos`/`width`/`height`: the source's fit rect inside 1920×1080, computed from the **decoded caps** (width, height and PAR). `SourceRef.display_aspect` is gate-only.
- **Zoom** uses the verified mapping above: a pure function in media, tested there. A buffer probe on `gltransformation`'s sink pad sets it, keyed on PTS, so the value matches the frame.
- **The pump,** on the exporter's thread, for each `FrameSpec`:
  - **Reuse:** if the cached current frame is still "last PTS ≤ `source_time`" (the next frame's PTS is > `source_time`, or the source is at EOS), re-push it.
  - **Pull:** if the target is ahead and within 0.5 s, pull forward.
  - **Seek:** otherwise, a flushing seek to the keyframe at or before the target (`KEY_UNIT | SNAP_BEFORE`), then pull forward. Not an accurate seek, which drops the wanted frame in a gap and finds nothing past the video's end (see Measured facts).
  - **Never seek backwards** to a target that is ≥ the current frame's PTS.
  - **PTS** is stream time, and seconds → ns go through `seconds_to_clock`.
  - **Push** `buffer.copy()` (a reference) with PTS `n/30` and duration `1/30`.
  - **Watch for errors** between pushes, since a failed encoder leaves a blocking `appsrc` hanging. Each pipeline's bus sync handler records the first `ERROR` as it is posted (and the encode side's `EOS`), and drops every message.
- **Output file:**
  - It is written to `<path>.part` and renamed to `<path>` only on success, so a cancel or error never destroys a previous file there.
  - `reserved-max-duration` puts `moov` first with no temp file.

### X3. Encoder and quality

- **Probe order:** `vah264lpenc`, then `x264enc`.
  - `vah264enc` and `nvh264enc` aren't present on any machine here, so their settings would be untested. They are added when someone can run them.
- **Quality is a quantizer.** This phase uses one fixed setting, **QP 24**:
  - VA: `rate-control=cqp qpi=24 qpp=24 key-int-max=60`;
  - x264: `pass=qual quantizer=24 speed-preset=veryfast key-int-max=60`. That is constant quality, which is smaller than constant QP at the same quality.
- **Phase 8** adds the quality and resolution picker (QP 28/24/20).
- **The parent spec's bitrate ladder is superseded:** the CQP-only encoder can't reach a bitrate target.

### X4. Control: an `Exporter` that owns its thread

`Exporter::start(job, on_msg) -> Result<Exporter, _>` in media owns its thread, the way `Recorder` does.

**Pump rules:**
- **Both pipelines** are created, used and torn down **on that thread**. (The GL display and context are the process's; see X2.)
- **The pump never blocks without a bound.**
  - `appsrc block=false`. While the queue is full, it waits in a short loop.
  - It pulls with short timeouts.
  - Each pass of either loop checks the cancel flag and pops the decode and encode buses for errors (~10 ms).
  - A blocking push never returns after a downstream error (measured), so this is required, not defensive.
- **Messages:** the thread sends `Progress(u8)` when the whole percent changes (deduplicated on the thread), and finally exactly one `Finished(Result<Done, ExportError>)`.
  - `ExportError` is `Cancelled` or `Failed(String)`.
  - `Done` carries the output path, the encoder name and the decode `Diagnostics`, reusing the player's type.
- **Cancel** sets an atomic flag. The thread notices within about 10 ms, takes its pipelines to NULL, deletes `.part` and sends `Finished(Err(Cancelled))`. There is no EOS.
- **`Drop`** cancels and joins, for shutdown.

**On the bus:**
- **`Command::ExportClip { id, path }`:**
  - refused, with an `Event::Error(CantExport)`, if the clip or its source is missing, the clip has no frames, the path is the source video, or an export is running;
  - dropped silently by the recording guard while recording, since the UI greys it out.
  - Otherwise the bus computes the schedule and starts the exporter.
- **`Command::CancelExport`** sets the flag, and nothing else.
- **The outcome.** The bus keeps the exporter until `Finished` arrives, then:
  - joins it (instantly);
  - emits `Event::Export(ExportStatus::{Done(PathBuf), Cancelled, Failed(msg)})`;
  - logs the diagnostics line.

  Because the thread's own result decides the outcome, a cancel that races a finished export reports `Done`, which is true. There is one sender and a FIFO channel, so there is no job id and no stale message.
- **Progress** is `Event::Export(ExportStatus::Running(u8))`.

**Recording and export never overlap** (a user decision, 2026-09-19).
- **Record is refused while an export runs:** `CantRecord("an export is running")`, and the UI greys Record out.
- **Export is refused while recording**, as above.
- The reason: sharing the VA engine measurably cost the **recording** frames. An export takes about 17 s per minute of clip on the reference laptop.

**Snapshot:** export reads only the source video, through a snapshot taken at start.

**Shutdown** drops the exporter, which cancels and joins it.

### X5. UI

- **Clip context menu:** "Export video…". It opens an `rfd` save dialog (default name, `*.mp4` filter), then sends `ExportClip`. A name typed with no extension gets `.mp4`; if that file exists, the export is refused (the dialog confirmed an overwrite of the name as typed). It is disabled while an export runs or while recording, and Record is disabled while an export runs.
- **Progress.** An `export-progress` property, hidden when negative, drives a small progress bar with **Cancel** in the transport. It is separate from the timed notice line, so notices can't overwrite it.
- **Completion:** "Exported to …" goes through the ordinary notice; an error goes through the usual dialog. The bar hides on any terminal outcome.

---

## Crate responsibilities

| Crate | Phase 5 contents |
|---|---|
| `video-coach-core` | `export.rs`: `OUTPUT_FPS`, `FrameSpec`, `frame_schedule`. |
| `video-coach-media` | `export/`: `Exporter` (the decode pipeline via the shared `gl_bin`, the pump, the encode pipeline, the encoder probe, the surfaceless EGL display and context sharing, the zoom mapping, `.part` handling, cancel, and a non-blocking pump). `gstreamer-gl-egl` (with `v1_24`) moves into media's dependencies. |
| `video-coach-app` | Bus: `ExportClip`, `CancelExport`, `Input::Export`, `Event::Export`, `UserError::CantExport`, refusing to record during an export, and dropping the exporter on shutdown. UI: the menu item, save dialog, progress bar and Cancel. |
| `video-coach-harness` | Export end to end, running on llvmpipe in CI. |
| CI | No change expected. `gstreamer1.0-gl` and `libgl1` already pull in `libegl-mesa0`, `mesa-libgallium` (llvmpipe) and `libgl1-mesa-dri` as hard dependencies on noble. With no `/dev/dri`, `new_surfaceless` falls back to llvmpipe by itself. `openh264dec` comes from plugins-bad. |

## Testing

- **Core:** `frame_schedule`:
  - the frame count, including float-noise totals (8.3 s → 249 frames);
  - a sub-frame segment that contains an `n/30` gets a frame, and one that doesn't gets none;
  - the play mapping;
  - freeze anchors;
  - skip jumps;
  - zoom per frame;
  - a clamp at the source end.
- **Media:**
  - **The zoom mapping,** as a pure test.
  - **The fiducial test**, run with the real graph (llvmpipe in CI, hardware locally):
    - **Source:** a fixture whose every frame carries its index as a **binary counter of big black and white blocks** (≥32 px at source resolution), generated with `appsrc`.
    - **Two sources:**
      - a **25 fps VP8/WebM** file;
      - a **60 fps H.264 MP4 with B-frames** from `x264enc`, which should exercise the edit-list stream-time trap. Assert that `qtdemux` gives it a non-zero segment start; if it doesn't, find another way to produce one.
    - **The clip:** plays, a freeze, skips, anchors off frame boundaries, and identity zoom.
    - **Check:** decode the output (`openh264dec`, or whatever decoder is present) and compare **every** frame's index with the schedule.
  - The output's shape:
    - 1920×1080, 30/1, the frame count;
    - `moov` before `mdat`;
    - a 4:3 source is letterboxed.
  - Cancel mid-export leaves no `.part` and no output.
  - A pre-existing file at the path survives a cancel.
  - An encoder error ends the export without hanging.
- **Harness:** `ExportClip` → `Running` → `Done`, and the file exists with the expected frame count; `CancelExport` → `Cancelled`, with nothing left; `ExportClip` during a recording is refused.
- **Manual** (batched):
  - export a real clip and watch it;
  - a slow zoom pan looks smooth;
  - the 2× gate with the log line (the `measure-media` skill);
  - Record is greyed out during an export.

## Risks

1. **llvmpipe on CI runners** is verified locally with `/dev/dri` hidden (`bwrap --tmpfs /dev/dri`), but not on a hosted runner. CI hasn't run on this branch yet (it runs on PRs and `main` only).
2. **Headless EGL on other machines.** `new_surfaceless` needs `EGL_MESA_platform_surfaceless`. Proprietary NVIDIA drivers may lack it; export then fails loudly (BACKLOG).
3. **CQP bitrate varies with content.** Accepted: YouTube re-encodes.

## Deferred

- Overlays, PiP, audio, the scoreboard, compilations, and the quality and resolution picker: Phase 8.
- HEVC output.
- NVIDIA and `vah264enc` encoder entries, until testable.
- Export on machines without surfaceless EGL (BACKLOG).
