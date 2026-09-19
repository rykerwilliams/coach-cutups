# Linux Port — Phase 2: Source Playback, Transport and Project Management

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 2; Command bus; Media pipelines; Decoder selection; Open question 2)
**Evidence:** `docs/superpowers/spikes/2026-09-19-seek-latency.md` (Phase 2 hardware gate — passed)
**Scope decision:** Phase 2 ships whole, not split into playback-first and zoom/multi-source-second (user, 2026-09-19).

---

## Goal

The first runnable Linux app. A coach can open or create a project, add one or more source videos, and scan them as one continuous timeline: play, pause, scrub, skip, adjust volume, and zoom/pan the picture. Video stays on the GPU end to end.

Nothing about clips, recording, drawing, scoreboard or export is in scope. Those phases build on the surfaces defined here.

## What "done" looks like

1. `cargo run -p video-coach-app` opens a window. With no project it offers **Open Project…**; opening an empty folder creates a project; opening a folder with an unreadable `project.json` refuses and changes nothing on disk.
2. **Add Source Video…** probes the file, rejects an aspect mismatch or a duplicate, and appends it. Sources can be removed and reordered from the sidebar.
3. Multiple sources play as one timeline: the position readout, scrubber, skips and auto-advance at the end of a source all operate on the concatenated time.
4. Space, arrows (±3 s, Shift ±10 s) and A/D work as in the macOS app; scrubbing previews live while dragging and lands frame-accurate on release.
5. Scroll-wheel zoom anchors on the cursor, drag pans when zoomed, keys 1/2/3 and Ctrl+0 behave as specified; the zoom indicator appears above 1×.
6. The app logs, at startup and on every source load, the selected decoder, the caps feature entering GL and the GL platform — and all three read hardware / `memory:DMABuf` / EGL on the reference laptop.

---

## Decisions

Each records what it replaces and why. "macOS" means the reference implementation under `apple/`.

### D1. The player is `playbin3` with an app-supplied GL video sink

`playbin3` (which uses `decodebin3` internally, satisfying the parent spec's "decodebin3 required" rule) with:

- `video-sink` = a bin: `glupload ! glcolorconvert ! appsink` where appsink caps are `video/x-raw(memory:GLMemory), format=RGBA, texture-target=2D`;
- `audio-sink` = `autoaudiosink`; volume through `playbin3`'s `volume` property;
- `flags` = video | audio | native-video (no soft-colorbalance, no deinterlace, no text).

Measured on the reference laptop: DMABuf reaches `glupload` through `playbin3` with both default and minimal flags. What `playbin3` removes compared with a hand-built `filesrc ! decodebin3` graph: stream selection (it selects the video stream itself, which closes the "linked the audio pad" trap), the audio branch and its queues, the volume element, and gapless switching to the next file via `about-to-finish`.

The video-sink bin is **injected**: production passes the GL bin; tests pass `fakesink`, so the media crate's tests run headless with no GL and no display.

### D2. Slint renders with Skia over EGL — required, and checked at runtime

Slint's default renderer (FemtoVG on winit) selects **GLX** on X11. GStreamer 1.24's `glupload` imports DMABuf only through `EGL_KHR_image_base`, so under GLX the hardware decoder silently falls back to system memory — the 6× throughput / 5× seek regression the seek spike measured. Slint's **Skia OpenGL** renderer uses EGL on both X11 and Wayland.

- Select at startup: `BackendSelector::new().backend_name("winit").renderer_name("skia").require_opengl_es()`.
- In `RenderingSetup`, confirm `eglGetCurrentContext()` is non-null. If not, **fail loudly** with a message naming the renderer — never continue on a copying path. (Slint issue #11169: Skia can silently fall back to software rendering when GL init fails.)

### D3. Frame delivery follows Slint's official `gstreamer-player` example, with three fixes

The example (Slint v1.18.0, `examples/gstreamer-player/slint_video_sink/egl_integration.rs`) is the template:

1. In `RenderingSetup`, wrap Slint's current EGL display and context (`GLDisplayEGL::with_egl_display`, `GLContext::new_wrapped`).
2. A bus **sync** handler answers `NeedContext` for `gst.gl.GLDisplay` and `gst.gl.app_context` with the wrapped objects, and forwards every other message to the player's owner over a channel.
3. appsink `new_sample` sets a `GLSyncMeta` sync point, stores the buffer in a single-slot latest-wins mailbox, and requests a redraw.
4. In `BeforeRendering`, take the pending buffer, wait on its sync meta, map it with `GLVideoFrame::from_buffer_readable`, **keep the mapped frame alive until the next one replaces it**, and hand `texture_id(0)` to `BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture`.

Fixes over the example:

- **Preroll.** The example wires only `new_sample`, so a frame reached by a seek while paused never displays. Wire `new_preroll` into the same mailbox. Scrubbing a paused frame is the core interaction.
- **Re-setup.** On Wayland, hiding the window destroys it and `RenderingSetup` later arrives with a **new** context. Re-wrap the context and re-answer `NeedContext` on every `RenderingSetup`; tear the pipeline's GL state down on `RenderingTeardown`.
- **Aspect.** Don't force `pixel-aspect-ratio=1/1` in the appsink caps; compute display size from the negotiated caps' PAR.

Cost accepted: one GPU pass for YUV→RGBA in `glcolorconvert`. Slint imports only 2D RGBA textures (`GL_TEXTURE_EXTERNAL_OES` support was proposed and not merged).

### D4. The app owns the concatenated timeline; the player plays one source at a time

macOS let mpv's playlist be the concat, and read `playlistPos` back as truth. A cross-source seek issued `loadfile … replace`, which **collapsed the playlist to one file** (`MPVSourcePlayer.swift:540-543`); afterwards the displayed position, the skip base, and the `sourceIndex` recorded on new clips and match events were all silently wrong in any multi-source project.

The port inverts ownership. The player holds exactly one source loaded; the **player owner** tracks `current: (source_index, source_seconds)` and is the only authority for it.

- **Concat → source** is a pure function in core: `Project::locate(abs_seconds) -> (source_index, source_seconds)`, matching macOS `Workspace.sourceTime(at:)`: the first source with `abs < cumulative + duration`; an instant exactly on a boundary belongs to the **next** source at 0; past the end clamps to `(last, last_duration)`; no sources → `(0, 0)`. Inverse is the existing `Project::abs_seconds`.
- **Seeking to another source** loads that source (`uri` change) and seeks within it; the owner updates `current` on the resulting `stream-start`.
- **Auto-advance** uses `about-to-finish` to queue the next source's URI gaplessly; the owner advances `current.source_index` on `stream-start`. On the last source, playback pauses on its final frame (macOS: `keep-open=yes`).
- Concat math uses the **stored** `SourceRef.duration_seconds` from the add-time probe (parent spec: it is the single duration authority). Advance is driven by the player reaching end of stream, not by the clock crossing the stored duration, so a probe/decoder duration disagreement shifts the boundary display by at most that disagreement and never skips or repeats media.

### D5. `Project` ownership: the bus owns it and publishes snapshots

Resolves parent-spec open question 2 and BACKLOG #21.

- A single **bus thread** owns `Project`, the project folder, the player, and the skip coordinator. Every mutation arrives as a `Command`; the bus emits `Event`s, including `ProjectChanged(Arc<Project>)` after each successful mutation.
- The UI keeps the latest `Arc<Project>` and derives Slint properties from it. No locks around `Project`; no partial updates.
- **Threading is a std thread plus `std::sync::mpsc`**, not an async runtime: nothing in Phase 2 is I/O-bound enough to justify one. GStreamer bus messages reach the bus thread through the sync handler's forwarding channel; UI updates go out through `slint::invoke_from_event_loop`.
- **The one exception**, from the parent spec's bus contract: the UI thread holds a clone of the pipeline handle for `query_position`, used for the position readout (D8) and, from Phase 4, for caller-captured play/pause anchors. It never changes pipeline state.
- `Command` is `Debug`, not serde (parent spec).

### D6. Project lifecycle fixes the macOS open bug

macOS set `self.folder` **before** reading (`Workspace.swift:164`). A failed open left the app pointing at the new folder while still holding the old project, and the next autosave wrote the old project over the `project.json` the app had just refused to touch.

- `OpenProject(folder)`: read first. On `Ok`, commit folder and project together. On `MissingProjectJson`, create `Project::new(folder name)`, write it, then commit. On any other error, change **nothing** — neither the folder nor the project — and report the error.
- **Last-project restore** on launch (macOS parity): remember the folder path after a successful open; on launch, attempt it; if it fails, forget it and show the no-project state. The path is stored in `$XDG_CONFIG_HOME/coach-cuts/state.json`, never in the project.
- **Recents are dropped from Phase 2.** The parent spec listed them, but macOS never had them; restore-last covers the common case. (BACKLOG.)
- **File menu:** Open Project…, Add Source Video…, Quit. The transport-bar buttons and empty-state cards from macOS stay too.

### D7. Source list: identity, gates and remaps

- **Probe** with `gst_pbutils::Discoverer` at add and relink time: duration, and display width/height after PAR. A file with no video stream is rejected.
- **`SourceRef` gains `display_width` and `display_height`.** The aspect gate and the letterboxed content rect both need them, and storing them removes macOS's race where the reference aspect was computed asynchronously after a rebuild and a quick second add skipped the gate. `formatVersion` stays **7**: no build has ever written a v7 file outside tests, so this is an amendment to an unshipped format, not an additive change to a shipped one.
- **Aspect gate** (macOS `aspectsMatch`, `Workspace.swift:290-293`): both aspects > 0 and `|a − b| / max(a, b) < 0.005`. The reference is the **first source in the list**, from its stored dimensions — so it applies on add **and relink**, and works while other sources are missing. (macOS's relink gate never ran, because the reference aspect was nil whenever a source was missing.)
- **Duplicates are rejected**: a source whose resolved path equals an existing source's. macOS allowed them, which broke reorder's identity map.
- **Remove** refuses if any clip **or match event** references the source. On success it removes the source and decrements every higher `source_index` in clips **and match events**. macOS remapped clips only.
- **Reorder** is expressed as an index permutation, not by path identity, and remaps clips and match events through it.
- Both remaps are pure functions in core with tests; the bus calls them.
- **Relink** replaces path, display name, duration and dimensions after the aspect gate; clip times are unchanged.
- **Missing sources**: at open and after any list change, check each path exists. If any is missing, playback is disabled and the player area shows "Source video is missing: <name>" with **Relink…** for the first missing one (macOS parity), re-evaluated after each relink.
- **Position is preserved across list changes.** macOS restarted at source 0, t = 0 after every add, remove, reorder or relink. The owner instead re-locates the current source by path and restores its local time; if that source is gone, it goes to 0.

### D8. Transport

- **Play/pause**: Space and the button. Volume is applied from `scan_volume` whenever a source loads, not only on button presses (macOS re-applied it on one of the two play paths).
- **Skip**: ±3 s, Shift ±10 s, on Left/Right and A/D. Driven by the ported `SkipCoordinator` over **concat** time with `clip_duration = total_source_duration`, and the target clamped to `total − 0.05` before `locate`.
- **Seek completion** is the `ASYNC_DONE` whose seqnum matches the seek event's. Superseded completions are ignored.
- **The coordinator is reset on every generation change** — scrub release, source load, list mutation — not only on clip selection. macOS dropped late completions without clearing the coordinator's in-flight seek (`ContentView.swift:712-715`), after which skip keys armed debounces forever and never seeked.
- **Scrubbing is live** (new; macOS seeked only on release). While dragging, issue `KEY_UNIT` seeks, latest-wins with at most one in flight; on release, one `ACCURATE` seek. Evidence: KEY_UNIT seeks measured 2.6 ms median on the user's footage, 37 ms on 4K long-GOP.
- **Position readout**: concat-absolute `current / total`, formatted like macOS `formatDurationHMS` — floored, `H:MM:SS` when hours > 0 else `M:SS`, `0:00` for non-finite or ≤ 0. The UI polls `query_position` on a 30 Hz Slint timer and combines it with the owner's `current.source_index` (published in events).
- **Volume**: slider 0…1 mapped with GStreamer's **cubic** stream-volume format, approximating mpv's perceptual curve instead of switching to linear loudness. Persisted to `scan_volume` **on slider release**, not per tick (macOS saved on every drag tick).

### D9. Zoom

The math is already in core (`Zoom`, Phase 1). Phase 2 adds rendering and input.

- **Rendering is a Slint transform**, not GStreamer. The video `Image` sits in a `clip: true` container; `transform-scale` and a translation derived from `Zoom::transform` give continuous, sub-pixel zoom on the GPU. This is the only mechanism that updates while **paused** — a GStreamer-side `gltransformation` would need a new frame pushed to show a changed zoom. The single implementation is `Zoom::transform`; export (Phase 8) drives `gltransformation` from the same function. A test pins that both derive identical rects.
- **Letterbox.** The player area is the window's content area, and the video is fitted with `Zoom::IDENTITY.transform(...)`. Unlike macOS there is no aspect-locked player frame, so **the cursor is normalized to the content rect** before `zoomed_to_cursor` (the `Zoom::source_point` contract). Clicks in the letterbox bars clamp to the content edge.
- **Inputs:**
  - scroll wheel: `scale × (1 ± 0.1)` per notch, anchored on the cursor (macOS mouse-wheel behavior);
  - drag with the primary button when scale > 1: pan, after a 4 px threshold;
  - `1`: identity; `2` / `3`: `scale ∓ 0.25` anchored on the cursor; `Ctrl+0`: identity;
  - pinch: only if Slint 1.18 exposes a gesture handler for it — verified in the plan's first task, otherwise out of scope.
- **No throttle.** macOS throttled zoom to 20 Hz at the workspace (`Workspace.swift:88-124`) with no trailing flush, so a gesture's final value could be dropped and `Cmd+0` could be swallowed. Nothing in the scan path is expensive enough to need it; the Phase 6 recorder dedupes on its own.
- **Snap on commit, not per event.** macOS snapped every event with a 3% window, so any single event changing scale by less than 3% snapped back to the notch and slow gestures could never leave it. The port snaps discrete steps (keys; wheel notches, which move ≥ 10%) immediately, and continuous gestures once at gesture end.
- **Lifetime:** zoom is not persisted and survives seeks and source changes (macOS parity). It **resets on project open** — the behavior macOS's own comment describes and never implemented.
- **Indicator** (macOS parity): `"%.2f×"` over a 140 px track with ticks at the snap notches, position `log2(scale) / log2(10)`, hidden at 1×, 180 ms fade.

### D10. Keyboard

| Key | Action |
|---|---|
| Space | play / pause |
| Left, A | skip −3 s (Shift: −10 s) |
| Right, D | skip +3 s (Shift: +10 s) |
| 1 / 2 / 3 | zoom identity / out 0.25 / in 0.25 at cursor |
| Ctrl+0 | zoom identity |
| Ctrl+O | Open Project… |

- Shortcuts do nothing while a text field (the project-name field) has focus.
- **A and D are matched by character, not physical key**, unlike macOS's keycodes: Slint key events carry text. On AZERTY the keys move; arrows are unaffected. Accepted.
- Recording (R), event tagging (E + 1/2/3), clip menu shortcuts and Esc's clip/preview behaviors belong to later phases and are absent.

### D11. Window layout

Minimum 1100 × 700, title "Coach Cuts".

- **Left sidebar** (240 px): project name (editable, saved on submit), and a Sources list — name, `M:SS` duration, missing marker, remove button (disabled with a tooltip when referenced), drag to reorder.
- **Center**: the player area on black, letterboxed. Empty-state cards over it, in order: no project → Open Project…; no sources → Add Source Video…; a missing source → Relink….
- **Bottom transport bar**: play/pause, scrubber, `current / total`, volume.
- **Zoom indicator** overlaid top-center of the player area.
- **Errors**: one modal dialog for open, add and relink failures, each with a specific message (aspect mismatch, duplicate, no video stream, unreadable project, legacy format).

No right-hand detail column in Phase 2; it arrives with clips (Phase 3).

---

## Crate responsibilities

| Crate | Phase 2 contents |
|---|---|
| `video-coach-core` | `Project::locate`; source-list remaps (remove, permute) over clips and match events; `SourceRef` dimensions; duplicate and aspect-gate predicates. Still no media dependency. |
| `video-coach-media` | `SourcePlayer` (playbin3 wrapper: load source, play/pause, seek with seqnum, volume, preroll/sample mailbox, `about-to-finish`), injected video sink, source probe (Discoverer), GL context wrapping and `NeedContext` handling, startup diagnostics (decoder, caps feature, GL platform). |
| `video-coach-app` | Slint UI, the bus thread and its `Command`/`Event` types, the frame-to-`Image` bridge in the rendering notifier, input handling, last-project state file. |
| `video-coach-harness` | Headless integration tests over the bus with fixtures and an injected `fakesink`. |

Dependencies: `slint` 1.18 with `backend-winit` and `renderer-skia-opengl`; `gstreamer`, `-video`, `-app`, `-gl`, `-gl-egl`, `-pbutils` 0.25 with feature `v1_24` and **no higher** (a higher feature raises the pkg-config floor past the system's 1.24.2).

---

## Testing

- **Core** (no GStreamer): `locate` including the boundary-belongs-to-next rule, past-end clamping and empty list; remove/permute remaps over clips and match events, including the refusal when referenced; duplicate and aspect predicates at the 0.005 edge.
- **Media** (GStreamer, no GL): fixtures generated per test run with `videotestsrc ! x264enc` into a temp dir — short, distinct sources whose frames encode their own timestamp. Tests: load and preroll; ACCURATE seek lands within one frame; seek completion fires once per seek via seqnum and not for superseded seeks; `about-to-finish` advances gaplessly; volume; Discoverer probe values; a file with no video stream is rejected.
- **Harness** (bus end to end, `fakesink`): open empty folder creates; open corrupt refuses and **leaves the previous project and folder in place** (the macOS bug, as a regression test); add with aspect mismatch and duplicate rejected; remove and reorder remap clips and match events; position preserved across a reorder; a burst of skips followed by a scrub release never leaves the coordinator stuck (the macOS bug, as a regression test); cross-source seek updates `current.source_index`.
- **Zoom**: the rect derived for Slint equals `Zoom::transform`'s content rect for letterboxed 16:9 and 4:3 sources.
- **Manual on the reference laptop**, recorded in the plan's closeout: the "done" list above, with the startup diagnostic showing hardware decode, `memory:DMABuf` and EGL, and the gate script still passing.

CI: the `workspace` job installs GStreamer dev packages and `x264` for fixtures and runs media and harness tests headless. The `core` job still runs with no GStreamer. The app crate's UI is not exercised in CI (no display); its non-UI logic is unit-tested.

---

## Risks

1. **Skia build.** `renderer-skia-opengl` pulls `skia-safe`, which normally downloads prebuilt binaries at build time and otherwise needs a from-source toolchain (clang, ninja, python). The plan's first task confirms the build on the reference laptop before anything depends on it.
2. **GL context lifecycle on Wayland.** Hide/show gives a new context (D3). Covered by re-setup on every `RenderingSetup`; verified manually on a Wayland session before closeout, because the reference laptop runs X11.
3. **Frame pacing.** appsink syncs to the pipeline clock while Slint renders on vsync, so expect up to one vsync of jitter and dropped duplicates within a vsync. Acceptable for scanning; revisit only if visible.
4. **`about-to-finish` edge cases.** Setting the next URI must happen inside the signal; a seek racing the transition must win. Tested in media tests.
5. **Slint gesture support.** If pinch isn't exposed, touchpad users get scroll-to-zoom and drag-to-pan only.

## Deferred

- Recents list (D6) → BACKLOG.
- Pinch-to-zoom if Slint lacks it (D9) → BACKLOG.
- Physical-key bindings for A/D (D10) → BACKLOG, only if a non-QWERTY user reports it.
- NVIDIA proprietary driver verification → Phase 11 packaging.
