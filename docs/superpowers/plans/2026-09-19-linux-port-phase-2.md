# Linux Port — Phase 2 Plan

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-2-design.md` (decisions are cited as D1–D12)
**Status:** Draft, pre-review

**Goal:** the spec's "Done when" list, on the reference laptop.

**Execution:** `CLAUDE.md` names `superpowers:subagent-driven-development`, which isn't installed. Instead, each task runs in a fresh subagent. The subagent is given this plan, the spec and `CLAUDE.md`, and nothing else from chat history. The orchestrator runs the `verify` skill after each task and commits each task on its own.

**Environment:** reference laptop (Ubuntu 24.04, GStreamer 1.24.2, Intel UHD, X11). Anything that needs a display runs here. The laptop can capture screenshots (`import -window root`, or `gnome-screenshot -f`) and read them back to check what's on screen.

---

## Task 0 — Toolchain and zero-copy spike (gate)

Everything after this task assumes Skia builds, the EGL context can be shared, and a frame shows up through the zero-copy path. So this task proves all three before anything is built on them.

1. **Dev packages.** Check with `dpkg-query`. If any are missing, **stop and ask the user to run**:
   `sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libfontconfig1-dev libfreetype-dev libxkbcommon-dev libwayland-dev libegl-dev libgl-dev`
   Never run `sudo` yourself.
2. **Workspace.** Set `rust-version = "1.92"`. Add workspace dependencies:
   - `slint` 1.18 with `default-features = false`, features `std`, `backend-winit`, `renderer-skia-opengl`, `compat-1-2`;
   - `slint-build` 1.18;
   - `gstreamer`, `gstreamer-video`, `gstreamer-app`, `gstreamer-gl`, `gstreamer-gl-egl`, `gstreamer-pbutils` 0.25, each with feature `v1_24` and nothing higher.

   `video-coach-core` gets **none** of these.
3. **Skia build.** `cargo build -p video-coach-app` with a minimal Slint window. If `skia-safe` can't fetch prebuilt binaries and needs a source build (clang, ninja, python), stop and report which tools are missing.
4. **Spike binary** `crates/video-coach-app/examples/zero_copy_spike.rs`. Throwaway, but kept in the repo as a diagnostic. It must:
   - select the Skia renderer (D2) and assert that `eglGetCurrentContext()` is non-null in `RenderingSetup`;
   - wrap the context and answer `NeedContext` from a sync handler (D3);
   - run `playbin3` on a path given as an argument, with the D1 GL sink bin, and draw the latest frame into an `Image` through `BorrowedOpenGLTextureBuilder`;
   - log the decoder, the `glupload` uploader (from `GST_DEBUG=glupload:6`, or by querying the element) and the GL platform.
5. **Also probe** the three Slint facts later tasks depend on, and record the answers in this plan's Task 0 notes (append them):
   - Does `PointerScrollEvent` carry modifiers?
   - Does `FocusScope`'s `capture-key-pressed` intercept keys before a focused `Slider`?
   - Does an `Image` with fractional `x`/`width` inside `clip: true` render without visible stepping during a slow pan?
6. **Gate:**
   - Run the spike on `~/Downloads/phone_Videos/20260502121738_000004.MP4`.
   - The log must show a hardware decoder, uploader `DirectDmabufExternal` and platform EGL.
   - A screenshot must show the frame.
   - Steady playback must run without dropped-frame warnings.
   - **If the gate fails, stop.** Report the failure; don't start Task 1.

Commit: `chore(app): Phase 2 toolchain and zero-copy spike`.

## Task 1 — Core additions (no GStreamer)

`crates/video-coach-core`, following the `port-swift-module` skill's rules where they apply.

- **`SourceRef.display_aspect: f64`** (D7). Required field, camelCase. Update fixtures and tests. `formatVersion` stays 7.
- **`Project::locate(abs_seconds) -> (usize, f64)`** (D4), with the four rules from the spec. Tests cover each rule, and round-trip with `abs_seconds` away from boundaries.
- **Source-list remaps** (D7), as pure functions over `&mut Project` plus the current position:
  - `remove_source(index, current) -> Result<Option<(usize, f64)>, RemoveError>`: refuses while any clip or match event references the source; decrements higher indices in clips and match events; returns the remapped current position, or `None` if the current source was removed.
  - `permute_sources(new_order: &[usize], current) -> (usize, f64)`: validates that `new_order` is a permutation, then remaps clips, match events and current.
- **Aspect gate** (D7):
  - `aspects_match(a, b)`: both > 0 and `|a−b| / max < 0.005`.
  - `aspect_reference(project, excluding: Option<usize>) -> Option<f64>`: the first source other than `excluding`.
  - Test the 0.005 edge in both directions.
- **`SeekSlot<T>`** (D8), new file `seek_slot.rs`. A single-flight slot where the latest request wins:
  - `request(t) -> Option<T>` returns what to issue now;
  - `completed() -> (Option<T>, Option<T>)` returns the seek that finished and the next one to issue;
  - `clear()`.

  Pure and generic. Tests: idle request issues immediately; requests during flight keep only the latest; completion issues the pending one; completion when idle is a no-op.
- **`format_hms(seconds) -> String`** (D8 readout), matching macOS `formatDurationHMS`. Tests: 0, a negative value, NaN, 59.9, 3599.9, 3600.

Commit: `feat(core): Phase 2 timeline location, source remaps, seek slot`.

## Task 2 — Media: fixtures and probe

`crates/video-coach-media`.

- **`fixtures` module**, behind `#[cfg(any(test, feature = "test-fixtures"))]`. It writes short VP8/Vorbis WebM files into a temp dir with `videotestsrc`/`audiotestsrc ! vp8enc/vorbisenc ! webmmux`, which needs only the base and good plugin sets.
  - Parameters: duration, width × height, frame rate, and a keyframe interval.
  - A second helper writes a file tagged with `image-orientation=rotate-90` (via `taginject`), and a third writes one with no video stream.
- **`probe(path) -> Result<Probe, ProbeError>`** using `Discoverer` (D7). `Probe` holds `duration_seconds` and `display_aspect` (width/height × PAR).
  - Errors: `NoVideo`, `Rotated(tag)`, `Unreadable(String)`.
  - Tests: values within one frame of the fixture parameters, and each error case.

Commit: `feat(media): test fixtures and source probe`.

## Task 3 — Media: the player

`crates/video-coach-media/src/player.rs`: the `SourcePlayer` from D1/D4.

- **Construction.** `SourcePlayer::new(video_sink: gst::Element, audio_sink: gst::Element, gl: Option<GlSlot>, messages: Sender<gst::Message>)`:
  - creates the one `playbin3` with flags `0x53`;
  - installs the sync handler, which answers `NeedContext` from `GlSlot` when present (D3) and forwards everything else to `messages`.
- **`GlSlot`** is shared, write-once-per-setup storage for the wrapped `GLDisplay` and `GLContext`. The type lives here; filling it is Task 7.
- **`gl_video_sink() -> (gst::Element, FrameMailbox)`** builds the D1 bin, with `new_sample` and `new_preroll` feeding a single-slot latest-wins `FrameMailbox` (D3).
  - The mailbox holds a `gst::Buffer` and its `VideoInfo`, plus a redraw callback.
  - Mapping to a texture is the app's job (Task 7).
- **Load sequence** (D4) as an explicit step-driven API, so the bus thread can drive it from `ASYNC_DONE` messages without blocking:
  - `begin_load(uri)` goes to READY, sets `uri`, then goes to PAUSED;
  - `seek(seconds, accurate: bool)` issues a flushing seek, `KEY_UNIT` or `ACCURATE`;
  - `set_playing(bool)`.
- **Volume** (D8): `set_volume(linear_0_1)` applies `x³` to `volume`.
- **`position_handle() -> PositionHandle`**: a clonable `Send` wrapper that allows only `query_position` (D5's single exception).
- **Diagnostics** (D12): after each load reaches PAUSED, report the decoder factory name, the `glupload` uploader where one exists, and the GL platform from `GlSlot`.
- **Tests** (injected `appsink` + `fakesink sync=true`, fixtures from Task 2):
  - load and preroll yields a sample;
  - an ACCURATE seek lands within one frame (sample PTS);
  - EOS arrives on the message channel;
  - a URI change through the load sequence switches files: the preroll sample's source dimensions change between two fixtures of different sizes;
  - the volume value is `x³`.

Commit: `feat(media): playbin3 source player with injected sinks`.

## Task 4 — App: the bus thread

`crates/video-coach-app/src/bus/`. It must be constructible **without Slint** (plain Rust), so the harness can drive it.

- **Types.**
  - `enum Input { Cmd(Command), Gst(gst::Message) }` on one `std::sync::mpsc` channel (D5).
  - `Command`: `OpenProject`, `RestoreLastProject`, `AddSource`, `RemoveSource`, `ReorderSources`, `RelinkSource`, `RenameProject`, `Play`, `Pause`, `TogglePlay`, `Skip { delta }`, `ScrubMove { abs }`, `ScrubRelease { abs }`, `SetVolume { value, commit: bool }`, `GlReady`, `Teardown { ack: Sender<()> }`, `Shutdown`.
  - `Event`: `ProjectChanged(Arc<Project>)`, `Position { source_index, target: Option<f64> }`, `Playing(bool)`, `SourceMissing(Option<usize>)`, `Error(UserError)`, `Diagnostics(..)`.
  - Events go out through a `Box<dyn Fn(Event) + Send>` sink: the app wraps `invoke_from_event_loop`, and tests collect into a `Vec`.
- **Loop.** `recv_timeout` against the skip-debounce deadline. When it expires, call `burst_ended()` and drive the decision.
- **Project lifecycle** (D6):
  - read-first open, then commit;
  - create on `MissingProjectJson`;
  - leave everything unchanged on any other error;
  - restore opens existing projects only;
  - the state file is `$XDG_CONFIG_HOME/coach-cuts/state.json`, read and written by one small module.
  - Write `project.json` after each mutating command (D5), and write volume only when `commit`.
- **Sources** (D7): add, relink, remove and reorder, calling Task 2's probe and Task 1's gate and remaps. Check each path for existence after every change and emit `SourceMissing`.
- **Transport** (D4, D8):
  - `current` position, and `SeekSlot<SeekTarget { abs, accurate, origin: Skip | Scrub | System }>`;
  - a cross-source target runs the load sequence first, advanced by `ASYNC_DONE` messages;
  - the first `ASYNC_DONE` after an issue completes the in-flight seek;
  - a completed `Skip` origin calls `seek_completed()` on the coordinator.
- **Coordinator reset** on scrub release, list mutation and project open. Never on loads that fulfil a skip.
- **EOS:** advance to the next source at 0 and keep playing; on the last source, set PAUSED.
- **Clamping:** skip targets and scrub-release targets are clamped to `total − 0.05`.
- **GL gate** (D3):
  - no pipeline state change beyond READY until `GlReady`, when the player was built with a GL sink;
  - on `Teardown`, go to NULL and send the ack.

Commit: `feat(app): bus thread owning project, sources and transport`.

## Task 5 — Harness tests

`crates/video-coach-harness/tests/`. Drive Task 4's bus with the injected non-GL sinks and Task 2 fixtures, and assert on collected events and on-disk state. Cover every harness case in the spec's Testing section, plus:

- the position is preserved when an earlier source is removed;
- EOS on the last source leaves the player paused;
- a seek in the final second of a source stays in that source.

Use polling helpers with timeouts rather than sleeps.

Commit: `test(harness): Phase 2 bus end-to-end`.

## Task 6 — Slint UI shell

`crates/video-coach-app/ui/*.slint` plus `src/main.rs`. Nothing is rendered in the video area yet.

- **Startup:** the backend selector from D2; build the bus with the GL sink; send `RestoreLastProject`.
- **Layout** (D11): sidebar (project name with `LineEdit`, Sources list with remove and reorder), the player area with empty-state cards, the transport bar (Open…, Add…, play/pause, scrubber, readout, volume), and an error dialog.
  - File pickers use `rfd` (a native dialog crate) unless Slint 1.18 offers one; confirm in Task 0.
- **State:**
  - `ProjectChanged` updates the Slint models;
  - a 30 Hz `Timer` computes the readout from the `PositionHandle` plus the last `Position` event, preferring `target` when it is set (D8);
  - `format_hms` from core formats it.
- **Scrubber:** drag emits `ScrubMove`, release emits `ScrubRelease`. **Volume:** changes emit `SetVolume { commit: false }`, release emits `commit: true`.
- **Keyboard** (D10): a root `FocusScope` with `capture-key-pressed`; yield while the name `LineEdit` has focus; letters match case-insensitively; Ctrl+O opens.
- **Non-finite values** from any UI input are dropped before a command is built (BACKLOG #28).

Commit: `feat(app): Phase 2 window, sidebar and transport`.

## Task 7 — Video in the window

Move Task 0's spike logic into the app's rendering notifier (D3):

- `RenderingSetup`: assert EGL; wrap into `GlSlot`; send `GlReady`.
- `BeforeRendering`: take the mailbox buffer, wait on its sync meta, map it, keep the mapped frame, and set the `Image` source.
- `RenderingTeardown`: send `Teardown` and wait for the ack.
- Show `Diagnostics` in the log.

Manual check with screenshots: play, pause, a paused seek showing the new frame (preroll), and a cross-source seek. Delete the spike example only once this task has proved everything it did. Otherwise keep it as a diagnostic.

Commit: `feat(app): zero-copy video in the Slint window`.

## Task 8 — Zoom

D9. The zoom state lives in the UI (not persisted); the bus doesn't need it in Phase 2.

- **Content rect** from `Zoom::IDENTITY.transform(frame_w, frame_h, area_w, area_h)`. The video `Image` geometry comes from `zoom.transform(...)` inside `clip: true`.
- **Input:**
  - Ctrl+scroll: `scale × 1.1^(dy/60)` about the cursor normalized to the content rect, clamped to its edge;
  - plain scroll: pan by `delta / (content size × scale)`, a no-op at 1×;
  - primary-button drag: pan when scale > 1, after a 4 px threshold;
  - keys `1`, `2`, `3` and `Ctrl+0`;
  - every result goes through `clamped()`. No snapping and no throttle.
- **Reset** to identity on `ProjectChanged` when the project folder changes.
- **Indicator** (D9 constants).
- **Unit test** (in the app crate, no display): the cursor-to-content-rect normalization for letterboxed 16:9 and 4:3 frames, including a cursor in the bars.

Commit: `feat(app): zoom and pan`.

## Task 9 — CI

`.github/workflows/rust.yml`:

- The `workspace` job installs the dev packages from Task 0 plus `gstreamer1.0-plugins-base` and `gstreamer1.0-plugins-good`. It runs fmt, clippy and **`cargo test --workspace`**, headless with no display.
- The app crate's tests must not open a window.
- The `core` job is unchanged, still with no GStreamer.
- Pin the toolchain to 1.92 in one job, so `rust-version` is actually checked.

Commit: `ci: run media and harness tests`.

## Task 10 — Closeout

- **Manual checklist** on the reference laptop: every item in the spec's "Done when", recorded in this plan with screenshots saved in the scratchpad (not committed), plus:
  - arrows after touching each slider;
  - Ctrl+scroll and two-finger pan on the touchpad;
  - the boundary hold during multi-source playback (spec Risk 4).
- **Gate script:** `scripts/linux-gate-check.sh` on the camera footage still passes.
- **Wayland** (spec Risk 2): if a Wayland session is available, check that EGL is used and frames display. If it isn't available, record that it wasn't checked.
- **BACKLOG #30:** tune `DEFAULT_BURST_WINDOW` by feel, and record the value and why.
- **`CLAUDE.md`:** add how to run the app, and note that the app needs the Skia renderer.
- Adversarial review of the shipped code (`adversarial-review` skill), apply the fixes, `verify`, commit.

## Deliberately not in this phase

- Clips, tags and undo (Phase 3).
- Recording and capture (Phase 4).
- Export (Phases 5 and 8).
- Drawing (Phase 6).
- Preview (Phase 7).
- Scoreboard (Phase 9).
- Transcription (Phase 10).
- Everything in the spec's Deferred list.
