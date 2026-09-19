# Linux Port — Phase 2 Plan

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-2-design.md` (decisions cited as D1–D12)
**Status:** Reviewed. Simplification and correctness passes are applied; the correctness pass built the dependency set and tested the GStreamer claims on the reference laptop.

**Goal:** everything on the spec's "Done when" list works on the reference laptop.

**Execution.** `CLAUDE.md` names `superpowers:subagent-driven-development`, but it isn't installed. Instead, each task runs in a fresh subagent that is given this plan, the spec and `CLAUDE.md`, and no chat history. After each task the orchestrator runs the `verify` skill and commits that task on its own. CI must stay green at every commit, so each task updates CI when it adds a new requirement.

**Environment.** The reference laptop runs Ubuntu 24.04, GStreamer 1.24.2 with dev headers installed, an Intel UHD GPU, and X11. Anything that needs a display runs there. Screenshots (`import -window root /path.png`) can be read back to check what's on screen.

**Known facts. Don't re-derive these.**
- **Build.** A clean build takes about 6 minutes of wall time and 34 CPU-minutes. `skia-bindings` downloads prebuilt binaries, so neither clang nor ninja is needed.
- **Slint API.**
  - `PointerScrollEvent` has `modifiers`.
  - `FocusScope`'s `capture-key-pressed` runs before the focused element.
  - `DragArea`/`DropArea` are stable in 1.18.
  - Skia uses EGL on Linux.
  - With `default-features = false`, the required compat feature is `compat-1-18`.
- **GStreamer 1.24.2 behavior.**
  - A flushing seek in PAUSED or PLAYING posts `ASYNC_DONE`.
  - PAUSED→PLAYING posts none; PLAYING→PAUSED posts one.
  - A READY→uri→PAUSED load posts its **own** `ASYNC_DONE`, before any seek.
  - `ASYNC_DONE` carries no seek seqnum.
  - An appsink whose samples are never pulled **never posts EOS**.
  - WebM can't carry `image-orientation`.
  - `glupload`'s uploader can't be queried; its sink-pad caps show which one is used: `memory:DMABuf` means zero-copy, plain `video/x-raw` means copied.
- **Edition 2021.** The workspace is edition 2021, so code copied from Slint's example must not use let-chains.

---

## Task 0 — Toolchain, CI, and the zero-copy gate

1. **Workspace `Cargo.toml`.**
   - Set `resolver = "3"`, which is MSRV-aware. Without it, `gstreamer` 0.25.3 locks `kstring` 2.0.5, which needs Rust 1.96.
   - Set `rust-version = "1.92"`.
   - Add these workspace dependencies:
     - `slint` 1.18 with `default-features = false` and features `std`, `backend-winit`, `renderer-skia-opengl`, `compat-1-18`;
     - `slint-build` 1.18;
     - `gstreamer`, `-video`, `-app`, `-gl`, `-gl-egl` and `-pbutils` at 0.25, each with feature `v1_24` and nothing higher;
     - `glutin_egl_sys` 0.7, for `eglGetCurrentContext`/`eglGetCurrentDisplay`, loaded through Slint's `get_proc_address` as in Slint's example;
     - `rfd` at the current 0.17.x with its default xdg-portal backend, which needs no GTK packages.

   `video-coach-core` gets none of these.
2. **CI** (`.github/workflows/rust.yml`).
   - `workspace` job:
     - apt-installs `libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good libfontconfig1-dev libfreetype-dev libxkbcommon-dev libegl-dev libgl-dev`;
     - adds `Swatinem/rust-cache`;
     - pins Rust 1.92, since this is the job that compiles Slint and GStreamer, so this is where the pin checks something;
     - runs fmt, clippy and `cargo test --workspace`. No test may open a window.
   - `core` job: unchanged, still with no GStreamer.
3. **Spike** `crates/video-coach-app/examples/zero_copy_spike.rs`. It stays in the repo as a diagnostic.
   - Select the Skia renderer (D2).
   - In `RenderingSetup`, assert `eglGetCurrentContext()` is non-null, wrap the context, and answer `NeedContext` from a sync handler. The handler returns `BusSyncReply::Drop` for everything, forwarding non-context messages.
   - Play a path given as an argument through `playbin3` with the D1 GL sink bin, drawing each frame via `BorrowedOpenGLTextureBuilder`.
   - Log the decoder factory, the caps on `glupload`'s sink pad, and the GL platform.
4. **Sub-pixel check.** In the spike, move the `Image` very slowly by setting fractional `x` and `width` values, and confirm with before/after screenshots that it doesn't visibly step. Record the result here.
5. **Gate.** Run the spike on `~/Downloads/phone_Videos/20260502121738_000004.MP4` with `GST_DEBUG=glupload:6`. The gate passes only if all of these hold:
   - the decoder is hardware;
   - the caps into `glupload` are `memory:DMABuf`, and the debug log shows `DirectDmabufExternal`;
   - the platform is EGL;
   - a screenshot shows the frame;
   - playback runs without dropped-frame warnings.

   **If the gate fails, stop and report. Don't start Task 1.**

Commit: `chore(app): Phase 2 toolchain, CI and zero-copy spike`.

### Task 0 notes (2026-09-19, reference laptop)

- **Gate: passed.** Measured with `GST_DEBUG=glupload:6 zero_copy_spike 20260502121738_000004.MP4 --quit-after 15`.
  - Decoder: `vah265dec`.
  - Caps into `glupload`: `video/x-raw(memory:DMABuf), format=DMA_DRM, 2560x1440, drm-format=NV12:0x0100000000000002`.
  - Uploader: `Changing uploader from None to DirectDmabufExternal`, and it stayed there. There were 434 `DirectDmabufExternal returned 1` uploads in about 14.5 s, which is 30 fps, and no `Raw Data`. The single `Dmabuf Passthrough` line is the first candidate being tried before it is rejected.
  - Platform: Slint's context is `EGL`/`GLES2`. The context `glupload` uses is `EGL` with an `EGL` display.
  - Playback produced no QoS messages and no warnings, and exited cleanly at `--quit-after`.
  - Screenshot: yes, the camera frame is visible. The root-window screenshot showed only the Cinnamon screensaver, which was active on `:0`. Grabbing the spike's own window with `import -window <id>` works through it, because the compositor redirects windows.
- **Sub-pixel check: `x` steps and `width` doesn't.** Screenshots were taken while paused on the preroll frame, comparing mean absolute luma over the interior.
  - `x` = 0.25 is identical to `x` = 0 (difference 0.000), and `x` = 0.5 is identical to `x` = 1. The Skia renderer rounds the origin to whole physical pixels whenever the transform is translate-only (`i-slint-renderer-skia` `itemrenderer.rs:474-512`, `pixel_align_origin_auto_restore`, used by `draw_image_impl`).
  - `width` = 800.25 and 800.5 each differ from both 800 and 801, so scaling is continuous.
  - Consequence for D9: zoom scale is continuous, but pan and zoom offsets move in steps of 1 physical pixel. The spec's claim of "continuous, sub-pixel zoom" is only half true. Whether a 1 px step is visible during slow pans is for the human to judge in Task 7.
- **Build.** The first build of the spike took 4 min 36 s of wall time and 28.5 CPU-minutes.
  - The workspace also passes clippy on Rust 1.92 with `--locked`. `kstring` is held at 2.0.2, as the resolver note predicted.
  - `cargo test --workspace` passed: 152 tests, all in core.
- **Surprises.**
  - The deps the spike needs are `[dev-dependencies]` of `video-coach-app` until Task 5 uses them for real. `rfd` and `slint-build` are workspace-only for now.
  - Task 5 says to delete this example once the window works, but step 3 above says it stays as a diagnostic. Decide which in Task 5.

## Task 1 — Core additions (no GStreamer)

Work in `crates/video-coach-core`, following the `port-swift-module` skill.

- **`SourceRef.display_aspect: f64`** (D7): required and camelCase. Update fixtures; `formatVersion` stays 7.
- **`Project::locate(abs) -> (usize, f64)`** (D4), following the spec's four rules. Tests cover each rule, plus a round trip with `abs_seconds` away from boundaries.
- **`Project::source_is_referenced(i) -> bool`**: true if any clip or match event references the source. `remove_source` uses it, and so does the UI's disabled remove button.
- **`Project::remove_source(i, current: usize) -> Result<Option<usize>, SourceReferenced>`**:
  - it refuses while the source is referenced;
  - it decrements higher `source_index` values in clips and match events;
  - it returns the remapped current index, or `None` if the current source was removed.
- **`Project::move_source(from, to, current: usize) -> usize`**: moves one source and remaps clips, match events and the current index. A move is always a valid permutation, so no validation is needed.
- **`Project::check_aspect(candidate, excluding: Option<usize>) -> Result<(), AspectMismatch>`**:
  - the reference is the first source other than `excluding`; if there's no such source, there's no gate;
  - the rule is: both aspects > 0 and `|a−b| / max < 0.005`;
  - test the 0.005 edge in both directions, and the sole-source relink.
- **`Zoom::content_fraction(cursor_x, cursor_y, frame_w, frame_h, area_w, area_h) -> (f64, f64)`** in `zoom.rs`, next to `transform`:
  - maps a cursor in window-area coordinates to fractions of the letterboxed content rect, clamped to `[0, 1]`;
  - Phase 6 strokes will reuse it;
  - tests: 16:9 and 4:3 frames in a 16:9 area, including a cursor in the letterbox bars.
- Tests for `remove_source` (refusal, remap, current removed) and `move_source` (forward, backward, current moved).

Commit: `feat(core): Phase 2 locate, source remaps, aspect gate, content fraction`.

## Task 2 — Media: fixtures and probe

Work in `crates/video-coach-media`.

- **`pub mod fixtures`**. Every function writes into a `&Path` directory the caller supplies, so `tempfile` stays a dev-dependency. There's no feature flag.
  - `webm(dir, name, secs, w, h, fps, keyint)`: `videotestsrc` and `audiotestsrc ! vp8enc`/`vorbisenc ! webmmux`. Set `samplesperbuffer` so the audio matches the video duration exactly; at 44.1 kHz and 30 fps that's `samplesperbuffer=1470`. Uses the base and good plugins only.
  - `rotated_mp4(dir)`: `videotestsrc ! jpegenc ! taginject scope=global tags="image-orientation=rotate-90" ! qtmux`. Verified: Discoverer reports this tag.
  - `audio_only(dir)`: a file with no video stream.
- **`probe(path) -> Result<Probe, ProbeError>`** using `Discoverer`.
  - `Probe { duration_seconds, display_aspect }`, where the aspect includes PAR.
  - Errors are `NoVideo`, `Rotated(String)` and `Unreadable(String)`.
  - Orientation is read from `DiscovererInfo::tags()` (the global tag list) through a pure `check_orientation(tag: Option<&str>)`, which is unit-tested on its own.
  - Tests: duration within one frame of the fixture; aspect within a **relative** 0.1%, since PAR rounding in WebM gives 1.7771 for 16:9; each error case.

Commit: `feat(media): test fixtures and source probe`.

## Task 3 — Media: the player and its seek slot

`crates/video-coach-media/src/player.rs`. The load sequence, the seek slot and EOS handling all live here (spec crate table). The bus sees only concat time.

- **Video sink.** `video_sink(kind: SinkKind) -> (gst::Element, FrameMailbox)`.
  - `SinkKind::Gl` builds the D1 bin, `glupload ! glcolorconvert ! appsink(GLMemory RGBA 2D)`.
  - `SinkKind::System` builds `appsink(video/x-raw)` for tests.
  - Both wire `new_sample` and `new_preroll` into the same single-slot, latest-wins `FrameMailbox`: a buffer, its `VideoInfo` (which includes PAR), and a redraw callback. Because samples are always pulled, EOS always arrives.
- **Constructor.** `SourcePlayer::new(video_sink, audio_sink, on_message: impl Fn(gst::Message) + Send + Sync + 'static)`.
  - It creates one `playbin3` with flags `0x53` and installs a sync handler.
  - The sync handler answers `NeedContext` from a private context slot and passes every other message to `on_message`. It returns `BusSyncReply::Drop`, so nothing piles up on the async bus.
- **`set_gl_context(display, context)`** fills the context slot. A player built with a GL sink refuses to go beyond READY until it has been called (D3).
- **Seek machinery.** `seek_to(uri: &str, secs: f64, accurate: bool, origin: Origin) -> Result<()>`, where `Origin` is `Skip`, `Scrub` or `System`.
  - Flight states: `Idle`, `Loading { target }`, `Seeking { target }`, plus a latest-wins `pending` slot.
  - A different `uri` starts a load: READY, set `uri`, PAUSED. On the load's `ASYNC_DONE` the player issues the seek. In the `Loading` state it accepts `ASYNC_DONE` only when `state(ClockTime::ZERO)` returns `(Success, Paused, VoidPending)`, which guards against a stale one.
  - On the seek's `ASYNC_DONE`: the seek is complete; issue the pending target if there is one.
  - A request made while busy **replaces** `pending`. Report the displaced request's origin, so a displaced `Skip` can reset the coordinator.
  - If a seek call fails, or media isn't loaded, complete that flight immediately with `SeekFailed`.
  - `set_playing(bool)` records `want_playing`. It's applied once the flight is idle, so there's no flash of frame 0 during a load.
  - `clear()` drops the flight and the pending target.
- **`handle(&gst::Message) -> Vec<PlayerEvent>`**. Events:
  - `SeekDone { origin }` and `SeekDisplaced { origin }`;
  - `SeekFailed { origin }`;
  - `Loaded { diagnostics }`;
  - `Eos`: **ignored while a flight is busy**, because an EOS posted before a flushing seek is stale;
  - `Error(String)`: also clears the flight.
- **`Diagnostics`** is a plain struct holding the decoder factory, the caps on `glupload`'s sink pad and the GL platform. The bus logs it.
- **Volume.** `set_volume(linear)` sets `volume = x³`.
- **Position.** `position_handle() -> PositionHandle`: a clonable, `Send` wrapper that can only call `query_position` (D5).
- **Tests.** Use `SinkKind::System` with `fakesink sync=true` for audio, and the Task 2 fixtures. Assertions use the mailbox sample's PTS and dimensions, never the pixel format: the laptop picks `vavp8dec` (NV12) while CI uses `vp8dec` (I420).
  - Load and preroll.
  - An ACCURATE seek lands within one frame.
  - Latest-wins: three quick requests produce two flights, and the middle one is reported as displaced.
  - Exactly one `SeekDone` per flight.
  - A cross-source load lands on its target, and the dimensions change between fixtures of different sizes.
  - A seek in the final second stays in the source.
  - `Eos` arrives at the end.
  - A failed seek doesn't wedge the slot.
  - The volume value is `x³`.

Commit: `feat(media): playbin3 source player with internal seek slot`.

## Task 4a — Bus: project and sources, with harness tests

`crates/video-coach-app/src/bus/`, which must be constructible **without Slint**: `Bus::spawn(sinks: SinkKind, events: Box<dyn Fn(Event) + Send>) -> BusHandle`.

- **Input channel.** One `std::sync::mpsc` channel carries `enum Input { Cmd(Command), Gst(gst::Message) }`. The player's `on_message` wraps each message as `Input::Gst`. The loop uses `recv_timeout` with the skip-debounce deadline.
- **Commands.**
  - Project: `OpenProject(PathBuf)`, `RestoreLastProject`, `RenameProject`.
  - Sources: `AddSource(PathBuf)`, `RemoveSource(usize)`, `MoveSource { from, to }`, `RelinkSource(usize, PathBuf)`.
  - Transport (4b): `TogglePlay`, `Skip { delta }`, `ScrubMove { abs }`, `ScrubRelease { abs }`, `SetVolume { value, commit }`.
  - Lifecycle: `GlReady { display, context }`, `Shutdown { ack }`. `Shutdown` goes to NULL, acks and exits; it also serves as `RenderingTeardown`.
- **Events.**
  - `ProjectOpened(Arc<Project>)`, sent on every successful open; the UI resets zoom on it (D9).
  - `ProjectChanged(Arc<Project>)`.
  - `Position { source_index, target_abs: Option<f64> }`, with the target in concat seconds.
  - `Playing(bool)`.
  - `Missing(Vec<bool>)`, one entry per source.
  - `Error(UserError)`, where `UserError` is `AspectMismatch`, `Rotated`, `NoVideo`, `UnreadableProject`, `LegacyProject`, `TooNewProject`, `SourceReferenced` or `Io`.
- **Project lifecycle (D6).**
  - Read first, then commit. On `MissingProjectJson`, create a project; on any other error, leave everything unchanged.
  - After any open: apply `scan_volume` to the player, clear the seek slot and reset the coordinator.
  - The state file `$XDG_CONFIG_HOME/coach-cuts/state.json` is a tiny module. Restore opens existing projects only.
  - Write `project.json` after each mutating command.
- **Sources (D7).** Probe, then gate, then remap, using Task 1 and Task 2.
  - Check each source's path for existence after every change and emit `Missing`.
  - If the current source is removed: reload at the same index clamped to the new length, at 0.
  - If the current source is relinked: reload at the same source time.
  - Moves and removals of other sources change offsets only, with no reload.
  - While any source is missing, refuse to play or seek.
- **Harness tests** (`crates/video-coach-harness/tests/`):
  - opening a folder with no project creates one;
  - opening a corrupt project refuses and keeps the previous project and folder;
  - restoring a folder that no longer exists doesn't create it;
  - an aspect mismatch is rejected;
  - a rotated source is rejected;
  - remove and move remap clips, match events and the current position;
  - a referenced source can't be removed;
  - a missing source blocks play.

  Use polling with timeouts, never sleeps.

Commit: `feat(app): bus with project lifecycle and source management`.

## Task 4b — Bus: transport, with harness tests

- **Skip.** `SkipCoordinator` over concat time, with `clip_duration = (total − 0.05).max(0.0)`, so its own clamp is the D8 clamp.
  - Each coordinator seek is `locate`d and issued with `Origin::Skip`.
  - `SeekDone { Skip }` calls `seek_completed()`.
  - `SeekDisplaced { Skip }` and `SeekFailed { Skip }` reset the coordinator.
- **Scrub.** `ScrubMove` issues KEY_UNIT seeks with `Origin::Scrub`. `ScrubRelease` resets the coordinator, then issues one ACCURATE seek clamped to `total − 0.05`.
- **Resets.** Reset the coordinator and clear the player slot on list mutation, open and `Error`. Never reset on a load that fulfils a skip.
- **EOS.**
  - On a non-final source: load the next source at 0 with `Origin::System`, and keep playing.
  - On the last source: `set_playing(false)` and emit `Playing(false)`.
- **Position.** Publish a new `source_index` before a load leaves READY. While a flight is busy, `target_abs` is set.
- **Volume.** `SetVolume` applies immediately and persists only when `commit` is set.
- **GL gate.** `GlReady` calls `player.set_gl_context`.
- **Harness tests:**
  - a skip burst across a source boundary lands on the accumulated target;
  - a skip burst followed by a scrub release never sticks, and the next skip works;
  - EOS advances to the next source and keeps playing;
  - EOS on the last source leaves the player paused;
  - a seek in the final second of a source stays in that source;
  - position is preserved when an earlier source is removed.

Commit: `feat(app): bus transport — skip, scrub, EOS advance`.

## Task 5 — Window with live video

This moves the spike into the app, so every later task can check its work by eye.

- **Startup.**
  - Select the backend as in D2.
  - Spawn the bus with `SinkKind::Gl`.
  - Open the project given as a command-line argument, if there is one (`cargo run -p video-coach-app -- <folder>`); otherwise send `RestoreLastProject`.
- **Rendering notifier.**
  - `RenderingSetup`: assert EGL, wrap the context, send `GlReady { display, context }`.
  - `BeforeRendering`: take the mailbox buffer, wait on its `GLSyncMeta`, map it with `GLVideoFrame::from_buffer_readable`, **keep** it, and set the `Image` source.
  - `RenderingTeardown`: send `Shutdown` and wait for the ack.
- **Minimal UI.** A black player area with the video, and keys Space, Left/Right, A/D and Shift wired to the bus. The FocusScope comes in Task 6.
- **Manual check, with screenshots:**
  - play and pause;
  - a paused skip shows the new frame (preroll);
  - a cross-source skip in a two-source project;
  - the log shows the decoder, `memory:DMABuf` caps and EGL.

  Keep the spike example as a diagnostic (Task 0 decision); it isolates the zero-copy path from the app.

Commit: `feat(app): window with zero-copy video`.

### Task 5 notes (2026-09-19, reference laptop)

- **Shape.** `src/main.rs` selects the backend (D2), spawns the bus with `SinkKind::Gl`, and sends `OpenProject(<arg>)` or `RestoreLastProject`. Bus events reach the UI thread through `slint::Weak::upgrade_in_event_loop`. `src/video.rs` is the renderer bridge: `RenderingSetup` wraps the EGL context and sends `GlReady`, `BeforeRendering` takes the mailbox frame, waits on its `GLSyncMeta`, maps it and keeps it mapped until the next one, and `RenderingTeardown` calls `BusHandle::shutdown`. `ui/app.slint` holds the player area and a plain `FocusScope` `key-pressed` handler, which Task 6 replaces with the `capture-key-pressed` root scope. Slint and the EGL crates are now normal dependencies of `video-coach-app`, because Cargo can't give a binary its own dependencies. The library doesn't use them, and `cargo test --workspace` passes with `DISPLAY` unset.
- **Manual check.** The test project had two sources, `20260314131843_000042.MP4` (HEVC 1920x1080 at 60 fps, 11.58 s) followed by `20260711105130_000010.MP4` (HEVC 2560x1440, 1242.17 s). Keys were sent to the window by id with python-xlib `XSendEvent`, since xdotool isn't installed.
  - Log for each load: `decoder Some("vah265dec")`, `glupload caps Some("video/x-raw(memory:DMABuf), format=(string)DMA_DRM, … drm-format=(string)NV12:0x0100000000000002")`, `GL platform Some("egl")`. Slint's context was `EGL`/`GLES2`.
  - Screenshots: a frame appears at startup; paused skips of +3 s show new preroll frames; play advances the picture; playback crosses from source 0 to source 1 by EOS advance (`loaded source 1`, position restarting at 0 and running at 1.0×); a paused state stays still (screenshot difference 0.0); a paused skip back (`Left`, then `a`) across the boundary shows source 0 again. Closing the window while playing exits with status 0. Launching with no argument restored the last project.
  - **Not exercised: Shift.** Synthetic key events don't carry modifier state into winit, which tracks modifiers through XKB, so the ±10 s path went untested.
- **Monitor off throttles playback.** With the laptop's monitor DPMS-off, playback crawled at about 0.1× in both the app and the Task 0 spike (26 `DirectDmabufExternal` uploads in 8 s). With `vblank_mode=0` it ran at 1.0× (221 uploads in 8 s). The likely cause is that the vsync-blocked swap on Slint's context also throttles GStreamer's GL work on the shared context. The mechanism wasn't confirmed. That's harmless with the screen on, but a UI thread that stalls in swap slows decoding.

## Task 6 — Sidebar, transport bar, dialogs, keyboard

- **Layout (D11).**
  - Sidebar:
    - project-name `LineEdit`, sending `RenameProject` on submit;
    - Sources list with duration and a missing marker, from `Missing`;
    - remove button, disabled with a tooltip when `source_is_referenced`;
    - drag-to-reorder via `DragArea`/`DropArea`, sending `MoveSource`.
  - Empty-state cards:
    - no project → Open…;
    - no sources → Add…;
    - a missing source → Relink…, for the first missing one.
  - Transport bar:
    - Open… and Add… (Add is disabled with no project);
    - play/pause, following `Playing`;
    - scrubber;
    - `current / total` readout;
    - volume slider.
  - One error dialog, with a message per `UserError`.
- **File pickers.** Use `rfd::AsyncFileDialog`, spawned with `slint::spawn_local` and parented to the window. Never use the blocking dialog on the event loop.
- **Readout and scrubber.**
  - A 30 Hz `Timer` computes concat time: `PositionHandle` plus the last `source_index`, or `target_abs` when it's set.
  - The timer drives both the readout and the scrubber, except while the scrubber is being dragged.
  - Format with an app-crate `format_hms`, matching macOS `formatDurationHMS`. Unit tests: 0, a negative value, NaN, 59.9, 3599.9, 3600.
- **Controls.**
  - Scrubber: moving it sends `ScrubMove`, releasing it sends `ScrubRelease`.
  - Volume: changes send `SetVolume { commit: false }`, release sends `commit: true`. The slider's initial value comes from the snapshot's `scan_volume`.
- **Keyboard (D10).**
  - A root `FocusScope` with `capture-key-pressed` handles every shortcut, and yields while the name `LineEdit` has focus.
  - Letters match case-insensitively. `Ctrl+O` opens.
- **Input hygiene.** Drop non-finite values before building a command (BACKLOG #28).

Commit: `feat(app): sidebar, transport bar, dialogs and keyboard`.

### Task 6 notes (2026-09-19, reference laptop)

- **Shape.** `ui/app.slint` holds the layout (D11): a 240 px sidebar (project-name `LineEdit`, Sources `ListView` whose rows are a `DropArea` around a `DragArea`, with a `×` remove button carrying a `Tooltip`), the player area with three mutually exclusive `EmptyCard`s, the transport bar, and a modal error overlay. One root `FocusScope` handles every shortcut in `capture-key-pressed` **and** swallows the same keys in `capture-key-released`: a Slint `Slider` fires `released` on an arrow key's *release*, which would otherwise have sent a `ScrubRelease` (and reset the skip burst) after every skip. Shortcuts yield while the name field has focus; `Return` in it renames and hands focus back. While the error dialog is up, every key is swallowed and `Return`/`Escape` dismiss it. Keys `1`/`2`/`3` and `Ctrl+0` are reserved: they call `zoom-reset`/`zoom-step`, which Task 7 wires.
- **Rust side.** `src/main.rs` turns callbacks into commands (dropping non-finite values) and applies events to a thread-local `UiState` (snapshot, missing flags, source index, target), because `upgrade_in_event_loop` closures must be `Send`. A 30 Hz `slint::Timer` sets the scrubber and the `current / total` readout from the scrubber itself while dragging, else `target_abs`, else `query_position` + the last source index (keeping the last value when the query fails). `src/pickers.rs` runs `rfd::AsyncFileDialog` (XDG portal) under `slint::spawn_local`, parented via Slint's `raw-window-handle-06` feature, one picker at a time. `format_hms` and `sentence` (capitalizes a `UserError`'s `Display` text for the dialog) live in the library's `format` module with unit tests. The source list shows durations with `format_hms`, so an hour-plus source reads `H:MM:SS` rather than the spec's `M:SS`. No bus change was needed.
- **Manual check.** Same two-source project as Task 5, `vblank_mode=0`, `XDG_CONFIG_HOME` in the scratchpad, keys by `XSendEvent`. Focus was moved by a temporary hook (since removed), because synthetic pointer events don't reach winit (it reads XInput2; a sent `ButtonPress` did nothing).
  - Layout renders; the sidebar lists both sources with `0:11` and `20:42`; the readout reads `0:00 / 20:53` and advances while playing (0:03 → 0:06 → 0:08); the play button follows `Playing`.
  - **Scrubber focused** (proved by `End` reaching it: `ScrubRelease 1253.75`): `Right Right d` and `Left` each logged `Skip`, and no `ScrubRelease` followed their releases. **Volume focused** (proved by `Home`: volume 0, committed on key release, `scanVolume: 0.0` in `project.json`): `Right Right` skipped instead. Relaunching showed the volume slider at 0, from the snapshot.
  - **Name field focused:** `End x d space a Right Return` typed `xd a` into the name without any skip, `Return` saved it to `project.json`, and the next `Right` skipped.
  - Empty states: a folder without `project.json` shows "No source video added"; no project (fresh config) shows "No project open" with Add disabled; a project whose first source path is gone shows "Source video is missing: gone.MP4" with Relink…, the row marked Missing and Play disabled. A corrupt `project.json` shows the error dialog ("The project file is unreadable: …") over the no-project state; `Escape` dismissed it.
  - **Picker:** `Open Project…` (invoked by the hook while playing) showed the portal's "Open Project Folder" chooser, `WM_TRANSIENT_FOR` the app window and modal; the readout and picture kept advancing behind it; `Escape` cancelled it.
  - Cross-source: skipping past 11.58 s loaded source 1 and the readout continued in concat time.
  - **Not verified:** drag-to-reorder, clicking buttons and dragging sliders with a real pointer, the tooltip, Shift ±10 s and `Ctrl+O` (synthetic events carry no modifier state), and choosing a file in a picker. Task 8's manual pass covers them.

## Task 7 — Zoom

D9. Zoom state lives in the UI and isn't persisted.

- **Geometry.**
  - The frame's display size comes from the mailbox `VideoInfo`, including PAR.
  - The content rect is `Zoom::IDENTITY.transform(...)`.
  - The `Image`'s geometry comes from `zoom.transform(...)` inside `clip: true`.
  - The cursor is mapped through `Zoom::content_fraction` (Task 1).
- **Input.**
  - Ctrl+scroll: `scale × 1.1^(dy/60)` about the cursor.
  - Plain scroll: pan by `delta / (content size × scale)`; a no-op at 1×.
  - Primary-button drag: pans when scale > 1, after a 4 px threshold.
  - Keys `1`, `2`, `3` and `Ctrl+0`.
  - Always `clamped()`. No snapping, no throttle.
- **Reset.** Return to identity on `ProjectOpened`.
- **Indicator.** Use the D9 constants.
- **Manual check.** Ctrl+scroll and two-finger pan on the touchpad, and zoom while paused.
- **Pixel snapping** (Task 0 finding). Skia rounds a translate-only image position to whole physical pixels, while width scales continuously. Judge by eye whether a slow pan steps visibly. If it does, try giving the `Image` a non-translate transform (for example `transform-scale`) so Skia stops snapping. Record the outcome.

Commit: `feat(app): zoom and pan`.

## Task 8 — Closeout

- **Manual checklist** on the reference laptop: every "Done when" item, plus arrows after touching each slider, and the boundary hold during multi-source playback (spec Risk 4). Record the results in this plan. Screenshots stay in the scratchpad, not the repo.
- **Gate script.** `scripts/linux-gate-check.sh` still passes on the camera footage.
- **Wayland** (Risk 2). Check it if a Wayland session is available; otherwise record that it wasn't checked.
- **BACKLOG #30.** Tune `DEFAULT_BURST_WINDOW` by feel, and record the result.
- **`CLAUDE.md`.** Document how to run the app and that it needs the Skia renderer.
- **Review.** Adversarial review of the shipped code (`adversarial-review` skill), then apply, `verify` and commit.

## Deliberately not in this phase

- Clips, tags and undo (Phase 3).
- Capture (Phase 4).
- Export (Phases 5 and 8).
- Drawing (Phase 6).
- Preview (Phase 7).
- Scoreboard (Phase 9).
- Transcription (Phase 10).
- The spec's Deferred list.
