# Linux Port — Phase 2: Source Playback, Transport and Project Management

**Date:** 2026-09-19
**Status:** Reviewed — adversarial simplification and correctness passes applied (correctness claims tested on the reference laptop)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 2; Command bus; Media pipelines; Open question 2)
**Evidence:** `docs/superpowers/spikes/2026-09-19-seek-latency.md` (hardware gate passed; zero-copy requires `decodebin3` **and** EGL)
**Scope decision:** Phase 2 ships whole, not split (user, 2026-09-19).

---

## Goal

The first runnable Linux app. A coach opens or creates a project, adds one or more source videos, and scans them as one continuous timeline: play, pause, scrub, skip, adjust volume, and zoom/pan the picture. Video stays on the GPU from decoder to screen.

Clips, recording, drawing, scoreboard and export are out of scope; later phases build on the surfaces defined here.

## Done when

1. `cargo run -p video-coach-app` opens a window. With no project it offers **Open Project…**; opening a folder without `project.json` creates a project; opening a folder with an unreadable `project.json` refuses and changes nothing — not the file, not the currently open project.
2. **Add Source Video…** probes the file, rejects an aspect mismatch, a rotated video or a file without video, and appends it. Sources can be removed and reordered from the sidebar.
3. Multiple sources behave as one timeline: the readout, scrubber and skips operate on concatenated time, and playback continues into the next source at the end of each.
4. Space, arrows (±3 s, Shift ±10 s) and A/D work even after the scrubber or volume slider has been touched. Scrubbing previews live while dragging and lands frame-accurate on release.
5. Scrolling zooms about the cursor, dragging pans when zoomed, keys 1/2/3 and Ctrl+0 behave as specified, and the zoom indicator appears above 1×.
6. On every source load the app logs the decoder, the GL upload method and the GL platform, and on the reference laptop they read hardware / `DirectDmabufExternal` / EGL.

---

## Decisions

"macOS" means the reference implementation under `apple/`.

### D1. The player is one long-lived `playbin3` with injected sinks

A single `playbin3` is created per bus-thread lifetime. Switching sources changes only its `uri` (D4), so every handle to it stays valid.

- `video-sink`: a bin `glupload ! glcolorconvert ! appsink` with appsink caps `video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D`.
- `flags` = video | audio | soft-volume | native-video (`0x53`). Soft-volume is required: without it `volume` falls through to the PulseAudio stream volume, which outlives the pipeline and has no effect on a `fakesink`.
- Both sinks are **injected**. Production passes the GL bin and `autoaudiosink`. Tests pass a system-memory `appsink` for video and `fakesink sync=true` for audio, so media tests run headless with no GL, no display and no sound device.

Measured through `playbin3` on the reference laptop: DMABuf reaches `glupload` with flags `0x43`, `0x53` and the default. `playbin3` uses `decodebin3` internally and selects the video stream itself, which avoids the linked-the-audio-pad trap.

### D2. Slint renders with Skia over EGL — required, checked at runtime

On X11, Slint's default FemtoVG renderer uses GLX, and so does GStreamer by default. GStreamer 1.24 imports DMABuf into GL only through EGL. Measured on the reference laptop with the app's sink caps: `decodebin3` + EGL imports zero-copy at **651 fps**; `decodebin3` + GLX copies every frame through the CPU at **58 fps** (seek-latency spike, Correction 2). Slint's Skia OpenGL renderer uses EGL on both X11 and Wayland.

- Select at startup: `BackendSelector::new().backend_name("winit".into()).renderer_name("skia-opengl".into()).require_opengl_es()`.
- In `RenderingSetup`, require a non-null `eglGetCurrentContext()`. If it is null, **fail loudly** and name the renderer. Never continue on a copying path. (Slint issue #11169: Skia can silently fall back to software rendering when GL init fails.)

### D3. Frame delivery: Slint's `gstreamer-player` example, with the startup gate and preroll

The template is Slint v1.18.0 `examples/gstreamer-player/slint_video_sink/egl_integration.rs`.

- **Startup gate.** In `RenderingSetup` the UI wraps Slint's current EGL display and context (`GLDisplayEGL::with_egl_display`, `GLContext::new_wrapped`) and sends them to the bus as `Command::GlReady`. Until then the bus keeps the pipeline in NULL/READY. If it prerolls earlier, GStreamer creates its own context: GLX on X11, which copies every frame, and unshared with Slint either way. Tests, which inject a non-GL sink, skip the gate.
- **One sync handler**, installed once when the pipeline is created, reads the wrapped display and context from a shared slot. It answers `NeedContext` for `gst.gl.GLDisplay` and `gst.gl.app_context`, and forwards every other message to the bus thread's input channel.
- **Mailbox.** appsink `new_sample` **and `new_preroll`** set a `GLSyncMeta` sync point, store the buffer in a single-slot latest-wins mailbox, and request a redraw. Preroll matters because a frame reached by a seek while paused arrives as a preroll sample; the example wires only `new_sample`, so paused scrubbing would show nothing.
- **Draw.** In `BeforeRendering` (UI thread), take the pending buffer, wait on its sync meta, and map it with `GLVideoFrame::from_buffer_readable`. **Keep the mapped frame until the next one replaces it**, and hand `texture_id(0)` to `BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture`.
- **Aspect.** Don't force `pixel-aspect-ratio=1/1`. Compute the display size from the negotiated caps.
- **Teardown.** `RenderingTeardown` sends a synchronous request to the bus to take the pipeline to NULL and waits for the acknowledgement before returning, because GStreamer must stop using the context first. Re-setup after a window hide is **not** designed in Phase 2. Slint suspends only on hide under Wayland, and Phase 2's window is never hidden. See BACKLOG.

Cost accepted: one GPU pass for YUV→RGBA in `glcolorconvert`. Slint imports only 2D RGBA textures.

### D4. The app owns the concatenated timeline

In macOS, mpv's playlist was the concatenated timeline and `playlistPos` was read back as the truth. A cross-source seek issued `loadfile … replace` (`MPVSourcePlayer.swift:540-543`), which collapsed the playlist to one file. From then on the displayed position, the skip base, and the `sourceIndex` recorded on new clips and match events were all wrong in any multi-source project.

In the port, the player holds one source at a time, and the bus thread owns `current: (source_index, source_seconds)`.

- **Concat → source** is a pure function in core, `Project::locate(abs_seconds) -> (source_index, source_seconds)`, matching macOS `Workspace.sourceTime(at:)`:
  - it returns the first source with `abs < cumulative + duration`;
  - an instant exactly on a boundary belongs to the **next** source, at 0;
  - past the end, it clamps to `(last, last_duration)`;
  - with no sources, it returns `(0, 0)`.

  The inverse is the existing `Project::abs_seconds`.
- **Loading a source** follows a fixed sequence. Setting `uri` while paused does not switch files (it only queues the next file), and a seek sent before preroll is dropped (tested):
  1. go to READY;
  2. set `uri`;
  3. go to PAUSED and wait for `ASYNC_DONE`;
  4. seek and wait for `ASYNC_DONE`;
  5. restore PLAYING if it was playing.

  The bus sets `current.source_index` itself, with no `stream-start` bookkeeping.
- **Advance on EOS**, not gapless. When a non-final source reaches end of stream, the bus loads the next source at 0 through the same sequence and keeps playing. Measured load time is 26–48 ms after the first load, which is invisible when scanning. `playbin3`'s `about-to-finish` was rejected: a seek issued after it fires lands in the queued next file, and then playback stalls with no error (tested).
- **End of the last source.** The bus sets PAUSED, so the play button shows the true state. The last frame stays in the mailbox. (macOS: `keep-open=yes`.)
- Concat math uses the **stored** `SourceRef.duration_seconds`, which the parent spec makes the single duration authority. Advance is driven by EOS, not by the clock crossing the stored duration, so a mismatch between the probe and the decoder can shift the displayed boundary but never skips or repeats media.

### D5. The bus thread owns `Project` and publishes snapshots

Resolves parent-spec open question 2 and BACKLOG #21.

- **Ownership.** One bus thread owns `Project`, the project folder, the player, the skip coordinator and the seek slot (D8). Every mutation arrives as a `Command`. After each successful mutation the bus emits `Event::ProjectChanged(Arc<Project>)`, and the UI derives Slint properties from the latest snapshot.
- **One input channel.** The channel carries `enum Input { Cmd(Command), Gst(gst::Message) }`: the sync handler forwards GStreamer messages into it. The loop calls `recv_timeout` with the skip-debounce deadline, so there is no async runtime. UI updates go through `slint::invoke_from_event_loop`.
- **Persistence.** The bus writes `project.json` after each mutating command (add, remove, reorder, relink, rename, volume-release).
- **Pipeline handle.** The UI holds a clone of the pipeline handle for `query_position` only (parent spec's bus contract). It never changes pipeline state.

### D6. Project lifecycle

macOS set `self.folder` **before** reading (`Workspace.swift:164`). After a failed open, the next autosave wrote the old project over the `project.json` the app had just refused.

- **`OpenProject(folder)` reads first:**
  - `Ok`: commit folder and project together, reset zoom (D9) and the skip coordinator.
  - `MissingProjectJson`: create `Project::new(<folder name>)`, write it, then commit.
  - Any other error: change **nothing** and report it.
- **Restore last project on launch** (macOS parity):
  - The last successfully opened folder is stored in `$XDG_CONFIG_HOME/coach-cuts/state.json`, never in the project.
  - Restore opens an **existing** project only. It never creates one, because `store::write` would recreate a deleted or unmounted folder.
  - If the folder or its `project.json` is gone, or the read fails, forget the path and show the no-project state.
- **Entry points** (macOS parity):
  - transport-bar buttons **Open Project…** and **Add Source Video…** (the latter disabled with no project);
  - the empty-state cards;
  - Ctrl+O.

  There is no menu bar and no recents list (neither existed on macOS; BACKLOG).

### D7. Source list

- **Probe** with `gst_pbutils::Discoverer` at add and relink time. Reject a file with no video stream. Reject a file whose `image-orientation` tag is anything but `rotate-0`, with a clear message: macOS rotation-corrected its aspect, but the GL path doesn't rotate, and none of the user's footage is rotated (BACKLOG).
- **`SourceRef` gains `display_aspect: f64`** (width/height after pixel aspect ratio), stored at probe time and used **only by the aspect gate**. Rendering uses the live caps. `formatVersion` stays **7**, because no build has written a v7 file outside tests, so this amends an unshipped format rather than extending a shipped one.
- **Aspect gate** (macOS `aspectsMatch`, `Workspace.swift:290-293`):
  - Rule: both aspects > 0 and `|a − b| / max(a, b) < 0.005`.
  - Reference: the stored aspect of the **first source other than the one being added or relinked**. If there is no such source, there is no gate. This matches macOS's intent (`Workspace.swift:358-361`: a sole source can be relinked to a new aspect).
  - It applies on add and on relink, and works while other sources are missing. macOS's relink gate never ran.
- **Duplicates are allowed.** Reorder is an index permutation, so identical paths break nothing.
- **Remove** refuses if any clip **or match event** references the source. On success it decrements every higher `source_index` in clips and match events. macOS remapped clips only.
- **Reorder** is an index permutation applied to clips and match events.
- **Position survives list changes.** `current.source_index` goes through the same remap. The player reloads only when the current source was removed or relinked; otherwise only the concat offsets change. (macOS restarted at source 0, t = 0 after every change.)
- Remove, permute and `locate` are pure functions in core with tests.
- **Relink** replaces the path, display name, duration and aspect after the gate. Clip times are unchanged.
- **Missing sources.** Each path is checked at open and after every list change. If any source is missing, playback is disabled and the player area shows "Source video is missing: <name>" with **Relink…** for the first missing one, re-checked after each relink (macOS parity).

### D8. Transport

**One seek slot.** Every seek goes through a single-flight slot on the bus thread: an `in_flight` flag plus a `pending` target where the latest wins. That covers skips, scrub drags, scrub release, cross-source loads, EOS advance and position restore.

- **Completion is the first `ASYNC_DONE` after issuing.** `ASYNC_DONE` does not carry the seek's seqnum in GStreamer 1.24.2, and several flushing seeks can coalesce into one `ASYNC_DONE` (tested). A pause during a seek may be taken for completion, which is harmless because the next seek supersedes it.
- When a seek completes and a target is pending, the slot issues the pending target. That is the whole latest-wins rule.
- A cross-source target uses the load sequence from D4 inside the slot.

**Skip**
- ±3 s, or Shift ±10 s, on Left/Right and A/D.
- Driven by the ported `SkipCoordinator` over **concat** time, with `clip_duration = total_source_duration`. The target is clamped to `total − 0.05` before `locate`.
- **Coordinator reset** happens on user context switches only: scrub release, list mutation, project open. Loads the slot performs to fulfil a skip must **not** reset it, or presses made during a cross-source burst are lost.
- macOS's stuck coordinator (a late completion dropped without a reset, `ContentView.swift:712-715`) cannot happen here, because every completion reaches the coordinator through the slot.

**Scrub** (new; macOS seeked only on release)
- While dragging: `KEY_UNIT` seeks through the slot, latest wins. Measured at 2.9 ms median on the user's footage and 37 ms on 4K with a long GOP.
- On release: one `ACCURATE` seek, clamped to `total − 0.05` (macOS `seekTo`).

**Play/pause:** Space and the button.

**Readout**
- Shows concat-absolute `current / total`, formatted like macOS `formatDurationHMS`: floored; `H:MM:SS` when hours > 0, else `M:SS`; `0:00` for values that are non-finite or ≤ 0.
- While a seek is pending or the scrubber is being dragged, it shows the **target**.
- Otherwise a 30 Hz Slint timer combines `query_position` with the latest published `current.source_index`, keeping the last value if the query fails. The bus publishes a new index before it leaves READY during a load.

**Volume**
- Slider 0…1 mapped as `x³` (cubic, matching mpv's perceptual curve) onto `playbin3`'s `volume`.
- The value survives `uri` changes, so it is set once.
- Persisted to `scan_volume` on slider **release**, not on every tick (macOS saved on every tick).

**Non-finite input** (BACKLOG #28): positions and gesture coordinates that aren't finite are dropped at the UI before they become commands.

### D9. Zoom

The math is already in core (`Zoom`, Phase 1). Phase 2 adds rendering and input.

**Rendering**
- The video `Image` sits in a `clip: true` container.
- Its `x`/`y`/`width`/`height` are set from `Zoom::transform(frame_w, frame_h, area_w, area_h)`: `tx`, `ty`, `frame_w·a`, `frame_h·d`.
- This gives continuous, sub-pixel zoom on the GPU, and it updates while **paused**. A GStreamer-side transform would need a new frame to be pushed before a changed zoom showed.
- Slint has no translate property, so geometry is the mechanism. Export (Phase 8) drives `gltransformation` from the same `Zoom::transform`.

**Letterbox.** The player area is fitted with `Zoom::IDENTITY.transform(...)`. There is no aspect-locked frame as on macOS, so:
- the cursor is normalized to the **content rect** before `zoomed_to_cursor`, and clicks in the bars clamp to its edge;
- drag-pan deltas are divided by content-rect size × scale.

**Inputs**
- **Scroll:** `scale × 1.1^(−dy/60)`, anchored on the cursor. On X11, winit can't distinguish a wheel from a touchpad (both arrive as line deltas), so a proportional factor keeps touchpads from running away. An unmodified touchpad scroll zooms rather than pans.
- **Drag** with the primary button when scale > 1: pan, after a 4 px threshold.
- **Keys:** `1` goes to identity; `2` and `3` change scale by −0.25 and +0.25 about the cursor; `Ctrl+0` goes to identity.
- **Pinch:** none on Linux, because winit 0.30 delivers pinch only on macOS/iOS (BACKLOG).

**No snapping in Phase 2.**
- Snapping every event traps slow gestures at a notch (the macOS bug).
- Snapping the ±0.25 key steps traps key `2` at 10×, because 9.75 is within 3% of 10.
- The key steps land on notches anyway.
- The indicator's ticks show the notches. `Zoom::snapped` stays in core for later use.

**No throttle.** macOS throttled zoom to 20 Hz with no trailing flush (`Workspace.swift:88-124`), which could drop a gesture's final value or swallow `Cmd+0`. Nothing on the scan path needs a throttle.

**Lifetime.** Zoom isn't persisted and survives seeks and source changes. It **resets on project open**, which macOS's comment describes but never implemented.

**Indicator** (macOS parity)
- `"%.2f×"` over a 140 px track, with ticks at the snap notches.
- Position is `log2(scale) / log2(10)`.
- Hidden at 1×, with a 180 ms fade.

### D10. Keyboard

A root `FocusScope` with `capture-key-pressed` handles shortcuts **before** focused children. It yields when the project-name `LineEdit` has focus. Without it, Slint's `Slider` consumes Left/Right/Home/End after the scrubber or volume slider has been touched, and arrows would change volume instead of skipping.

| Key | Action |
|---|---|
| Space | play / pause |
| Left, A | skip −3 s (Shift: −10 s) |
| Right, D | skip +3 s (Shift: +10 s) |
| 1 / 2 / 3 | zoom identity / −0.25 / +0.25 about the cursor |
| Ctrl+0 | zoom identity |
| Ctrl+O | Open Project… |

- Letters match case-insensitively: Shift+A arrives as `"A"`.
- Keys match by character, not physical key. On AZERTY, A/D move and the digits need Shift. Accepted (BACKLOG).
- Recording (R), event tagging (E + 1/2/3) and the clip shortcuts belong to later phases.

### D11. Window layout

Minimum size 1100 × 700; title "Coach Cuts".

- **Left sidebar (240 px):**
  - editable project name, saved on submit;
  - Sources list: name, `M:SS` duration, missing marker, a remove button (disabled with a tooltip while the source is referenced), and drag to reorder.
- **Center:** the player area on black, letterboxed. Empty-state cards over it, in this order:
  1. no project → Open Project…
  2. no sources → Add Source Video…
  3. missing source → Relink…
- **Bottom transport bar:** Open Project…, Add Source Video…, play/pause, scrubber, `current / total`, volume.
- **Zoom indicator:** overlaid top-center of the player area.
- **Errors:** one modal dialog for open, add and relink failures, with a specific message for each case: aspect mismatch, rotated video, no video stream, unreadable project, legacy format.

### D12. Diagnostics and decoder selection

- On every source load, log the selected decoder, the `glupload` uploader and the GL platform, and show them in the log.
- The parent spec's rank-raising decoder probe is **deferred to Phase 11 packaging**: on the target distro, hardware decoders already outrank software by default (seek-latency spike, finding 3).

---

## Crate responsibilities

| Crate | Phase 2 contents |
|---|---|
| `video-coach-core` | `Project::locate`; remove/permute remaps over clips, match events and the current index; `SourceRef::display_aspect`; the aspect predicate. Still no media dependency. |
| `video-coach-media` | The `playbin3` player (load sequence, seek slot, EOS handling, volume), sink injection, Discoverer probe, the sync handler, the frame mailbox, diagnostics. |
| `video-coach-app` | Slint UI, the bus thread with its `Input`/`Command`/`Event` types, the rendering-notifier bridge, input handling, the last-project state file. |
| `video-coach-harness` | Headless tests over the bus with injected non-GL sinks. |

**Dependencies**
- `slint` 1.18 with `backend-winit` and `renderer-skia-opengl`.
- `gstreamer`, `-video`, `-app`, `-gl`, `-gl-egl` and `-pbutils` 0.25, with feature `v1_24` and **no higher**, because a higher feature raises the minimum GStreamer past the system's 1.24.2.
- Workspace `rust-version` goes to **1.92**, which Slint 1.18 and gstreamer-rs 0.25 require.

---

## Testing

**Core** (no GStreamer)
- `locate`: an instant on a boundary belongs to the next source; past-end clamping; the empty list.
- Remove and permute over clips, match events and the current index, including refusal while the source is referenced.
- The aspect predicate at the 0.005 edge, and choosing the reference aside from the source being relinked.

**Media** (GStreamer, no GL)
- Fixtures are generated per run with `videotestsrc`/`audiotestsrc` into VP8/Vorbis WebM, which needs only the base and good plugin sets.
- Assertions use the prerolled or sampled buffer's PTS from the injected `appsink`.
- Tests:
  - load and preroll;
  - an ACCURATE seek lands within one frame;
  - the slot issues only the latest pending target;
  - completion arrives once per slot flight;
  - cross-source load;
  - EOS advances to the next source and leaves the last one PAUSED;
  - a seek in the final second of a source stays in that source;
  - volume;
  - probe values;
  - rejection of files with no video and of rotated files.

**Harness** (bus end to end)
- Opening a folder with no `project.json` creates a project.
- Opening a corrupt project refuses and **keeps the previously open project and folder** (macOS bug regression).
- Restoring a folder that no longer exists doesn't recreate it.
- Add with a mismatched aspect is rejected.
- Remove and reorder remap clips, match events and the current position.
- A burst of skips followed by a scrub release never leaves the coordinator stuck (macOS bug regression).
- A skip burst across a source boundary lands on the accumulated target.

**Manual** (reference laptop, recorded in the plan's closeout)
- The "Done when" list, including arrows after touching the sliders and the diagnostic reading hardware / `DirectDmabufExternal` / EGL.
- Tune the skip burst window by feel (BACKLOG #30).

**CI**
- The `workspace` job currently only builds. It must install the GStreamer dev packages, the base and good plugins, and the fontconfig/freetype/xkbcommon dev packages, then run the media and harness tests headless.
- The `core` job still runs without GStreamer.
- The UI isn't exercised in CI (there's no display); the app crate's non-UI logic is unit-tested.

---

## Risks

1. **Skia build.** `renderer-skia-opengl` pulls in `skia-safe`, which normally downloads prebuilt binaries. The plan's first task installs the dev headers and confirms the build on the reference laptop before anything depends on it.
2. **Wayland.** The reference laptop runs X11. Before closeout, verify on a Wayland session that EGL is used and that frames display.
3. **Frame pacing.** appsink syncs to the pipeline clock while Slint renders on vsync: expect up to one vsync of jitter. That's acceptable for scanning.
4. **Boundary gap.** EOS advance leaves a 26–48 ms hold on the last frame at each source boundary during playback. If it's noticeable in practice, revisit gapless playback with a transition-safe design.

## Deferred (→ BACKLOG)

- GL re-setup after a window hide (Wayland).
- Recents list, and a menu bar.
- Pinch-to-zoom.
- Rotated sources.
- Physical-key bindings for A/D.
- NVIDIA proprietary driver verification (Phase 11).
