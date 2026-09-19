# Linux Port — Phase 4: Capture (Commentary Recording)

**Date:** 2026-09-19
**Status:** Reviewed (simplify and correctness passes applied)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 4, and the Milestone C note: Phase 4 runs before Phase 3)
**Builds on:** `docs/superpowers/specs/2026-09-19-linux-port-phase-2-design.md` (bus, player, transport, zoom)
**Evidence:** the capture research on the reference laptop, summarized in "Measured facts"; and the macOS inventory of `RecordingController.swift`, `CaptureSessionController.swift`, `DeviceCatalog.swift` and `ContentView.swift`'s recording flow.

---

## Goal

A coach presses **R** while scanning. The app records their webcam and microphone while they talk over the game footage and steer it: play, pause, skip, zoom. Pressing **R** again produces a **clip**: a pointer into the game video, the commentary recording, and the log of what they did. The clip appears in the sidebar.

Clip editing, tags, delete and undo belong to Phase 3. Stroke drawing belongs to Phase 6. Export (including the picture-in-picture) belongs to Phases 5 and 8.

## Done when

1. With a project whose sources are all present, **R** starts a recording.
   - The transport shows "Preparing…", then a red **Recording** indicator with elapsed time and a live mic level.
   - The webcam light is on only while recording.
2. During the recording, Space, skips and zoom all work. Each action lands in the clip's event log with a timestamp that lines up with the recording's audio.
3. **R** (or Esc, or Stop) ends it, and a clip named `<source#>-HH:MM:SS` appears in the sidebar's Clips list.
   - `project.json` holds the clip's source index and start time, its duration, and an event log that starts `[zoom @0, pause @0]`.
   - `recordings/<uuid>.mkv` plays in any player.
4. The camera and microphone can be chosen. The choice persists, and a missing device falls back to the default without forgetting the preference.
5. Killing the app mid-recording leaves a playable `.mkv` behind. Nothing else is recovered (R7).
6. Pausing leaves the frame that was on screen, and that is the frame the logged anchor points at (R10).
   - The one exception: the anchor is read on the UI thread at the keypress, and the pause lands a few milliseconds later on the bus. When that gap crosses a frame boundary, the anchor is one frame behind the held frame. This is accepted, to keep the caller-captured contract.

---

## Measured facts (reference laptop, 2026-09-19)

These come from the capture research and the spec review. They are measured, not assumed.

- **Devices.** `gst::DeviceMonitor` sees only PipeWire devices by default: the PipeWire provider hides the v4l2 and pulse duplicates.
  - The laptop exposes **two cameras with the same display name**: the RGB webcam (`/dev/video0`, MJPEG and YUY2) and an IR face-unlock camera (`/dev/video2`, GRAY8 only).
  - `node.name` (e.g. `v4l2_input.pci-0000_00_14.0-usb-0_6_1.0`) is stable across reboots.
  - `object.id`, `/dev/videoN` and `object.path` are volatile.
  - `/dev/v4l/by-id` is wrong for this camera: the symlink points at the IR interface.
- **Camera formats.** 16:9 at ≤1280 wide and 30 fps exists only as **1280×720 MJPEG**.
- **Low light.**
  - The UVC control `exposure_dynamic_framerate=1` let the camera fall to **7.5 fps** in a dark room while the caps still said 30/1.
  - `v4l2src extra-controls="c,exposure_dynamic_framerate=0"` restores 30 fps. `pipewiresrc` can't set V4L2 controls.
- **Timestamps.** `v4l2src` stamps buffers with the kernel capture time, about 28 ms before arrival. `pipewiresrc` (audio) stamps them on arrival.
- **Encoders.**
  - `vajpegdec ! vah264lpenc` costs **8% CPU** at 720p30. `x264enc speed-preset=ultrafast tune=zerolatency` costs 110%.
  - `vah264lpenc` is **CQP-only** on this driver: bitrate settings are ignored.
  - `vaapih264enc` (deprecated) hung a test harness.
  - The `va` elements are rank 0, so they must be named explicitly.
- **Clock.**
  - `GstSystemClock` is CLOCK_MONOTONIC.
  - `pulsesrc`'s clock was about 473,000 s off monotonic. `pipewiresrc`'s tracks monotonic.
  - With `use_clock(SystemClock)` forced, everything lines up within milliseconds.
- **File time 0 is the pipeline's `base_time`.**
  - `matroskamux` writes running time as-is. It does not shift the file to its first video frame.
  - With video delayed 0.6 s (a simulated camera start-up), audio starts at 0 in the file and video at 0.605. Discoverer's duration includes the video-less lead-in.
  - Dropping early audio after `opusenc` to "fix" this writes audio at negative time, which desyncs the file.
  - `vah264lpenc`'s raw PTS carries a +3600 s offset, so only running times are meaningful.
- **Crash safety.** After `kill -9`:
  - non-streamable `matroskamux` with `filesink buffer-mode=unbuffered` left a playable file;
  - the default buffered filesink left 0 bytes;
  - Discoverer's duration for a crashed file can be badly wrong.
- **Level meter.** `level interval=100ms` posts rms/peak dB per channel, about every 99 ms.
- **Two pipelines at once.** VA decode for playback and VA encode for capture ran together with no contention.
- **No portal needed on X11.**
- **Frame accuracy.**
  - In PAUSED after an ACCURATE seek, `query_position` equals the target exactly.
  - While PLAYING, it lies within the displayed frame (0.6–34 ms past its PTS).
  - **After PLAYING→PAUSED the sink prerolls and shows the next frame** (+1 frame, 12/12 trials) while the position stays put.

---

## Decisions

### R1. Recording is a second pipeline owned by the bus

`video-coach-media` gains a `Recorder`, and the bus owns it next to the `SourcePlayer`:

```
v4l2src device=<path> extra-controls=c,exposure_dynamic_framerate=0
  ! image/jpeg,width=1280,height=720,framerate=30/1        (caps chosen per R3)
  ! queue ! <encode chain per R4> ! h264parse ! queue ! mux.
pipewiresrc target-object=<mic node.name>
  ! audio/x-raw,rate=48000,channels=2 ! queue ! audioconvert ! audioresample
  ! level interval=100000000 ! opusenc bitrate=96000 ! queue ! mux.
matroskamux name=mux offset-to-zero=false ! filesink buffer-mode=unbuffered location=recordings/<uuid>.mkv
```

- **Clock:** always `pipeline.use_clock(Some(&gst::SystemClock::obtain()))`, so file time 0 (`base_time`) is a monotonic instant (R5).
- **`offset-to-zero=false`** is the default. It is set explicitly with a comment, because R5 depends on it.
- **Camera source:** `v4l2src`, for its kernel timestamps and the exposure control. The device path is resolved from the PipeWire device at record time and never stored.
  - `extra-controls` is always set. A camera without the control logs a warning and records anyway. The plan's first task verifies this.
- **Microphone source:** PipeWire only. The parent spec's `pulsesrc` fallback is dropped: its clock was the one measured days off, and the target and Flatpak (Phase 11) both run PipeWire.
- **Sources are injectable,** as the player's sinks are (Phase 2 D1). Tests use `videotestsrc is-live=true` and `audiotestsrc is-live=true`, so media and harness tests need no camera, microphone or display.
- **Messages:** the recorder's GStreamer messages go on their **own path** into the bus, never through `SourcePlayer::handle`. There, its EOS would advance the source, its ERROR would reset the player, and its ASYNC_DONE would complete a seek.
  - The bus tags each recorder's messages with a recording generation number, captured in the callback it passes to `Recorder::start`, and drops those from a recorder it has already stopped.

### R2. Devices: enumerate via PipeWire, key on `node.name`

- **Enumeration:** `gst::DeviceMonitor` for `Video/Source` and `Audio/Source`, run when the Devices popover opens and again at record time. There is no long-lived monitor and no hot-plug watching.
- **Usable cameras only:** a camera is listed only if R3 finds a mode for it. GRAY8 is never a usable format, so the IR camera never appears, and a camera the app can't record from can't be picked.
- **System default:**
  - For the mic, "default" means no `target-object` is set, so PipeWire's session manager picks.
  - For the camera, it means the listed camera with the highest `priority.session` (1000 for the webcam, 980 for the IR camera here).
  - DeviceMonitor exposes no default flag.
- **Stored key:** `node.name`, the only key, goes in the existing `Preferences.preferred_camera_id` / `preferred_mic_id`.
  - The field doc comment in `project.rs`, which recommends `/dev/v4l/by-id`, is corrected.
- **Missing device:** if the stored device is absent at record time, record with the default device and show a notice. The preference is **not** cleared.
- **Scope:** preferences are per project (macOS parity; the fields are already there).
- **UI:** a Devices popover (camera list, microphone list, "System default"), opened from the transport bar. Picking a row sends `SetCamera` or `SetMic` with that one preference, so a stale list can't revert the other.

### R3. Camera format: one rule, else refuse

Use the largest **exactly 16:9** (`w·9 = h·16`) mode that is **≤1280 wide at 30/1**, MJPEG or raw (not GRAY8).
- The choice decides the head of the chain: MJPEG gets a JPEG decode, raw gets a `videoconvert`, because `vah264lpenc` accepts only NV12 (measured: YUY2 won't link).
- A camera with no such mode isn't listed (R2). With no usable camera at all, the recording is refused: "No camera with a 16:9, 30 fps mode up to 1280 wide". This is the parent spec's rule and macOS's `noSuitableFormat`.
- The choice is a pure function over the device's caps, tested with recorded caps structures.

### R4. Encoder: a presence check, CQP

- **Hardware chain:** if both `vah264lpenc` and (for MJPEG) `vajpegdec` exist, use `vajpegdec ! vah264lpenc rate-control=cqp qpi=24 qpp=26 key-int-max=30`.
- **Software chain:** otherwise `jpegdec ! videoconvert ! x264enc speed-preset=ultrafast tune=zerolatency key-int-max=30`.
- Raw camera modes drop the JPEG decode.
- **Never** use `vaapih264enc`.
- There is no live negotiation probe. A chain that fails at start goes through the recording error path.
- The choice is a pure function (element availability in, chain description out).
- The QPs are tuned by eye in a lit room during the plan's closeout, and the result is recorded.
- **Audio:** Opus at 96 kbit/s.

### R5. Recording time 0 is the pipeline's `base_time`

- **One clock.** Every timestamp in a recording is a CLOCK_MONOTONIC nanosecond count read from `gst::SystemClock::obtain().time()`.
- **Time 0.** `t0_ns = pipeline.base_time()`, read when the pipeline reaches PLAYING. This is the file's time 0 (measured).
- **Camera warm-up.** The camera's first frame lands 0.25–0.7 s later. That lead-in holds the coach's first words and no video.
  - The log's `pause @0` covers it, so replay holds the start frame from 0.
  - Phase 8's PiP must show nothing, or the first webcam frame, until the video stream starts, rather than assume video at 0.
- **Event times.** Every event's `record_time` is `(host_ns − t0_ns) / 1e9`. `host_ns` is captured **by the caller at the input event** (the Phase 2 bus contract).
- **Error budget.**
  - Audio is arrival-stamped, so it may sit about 20–40 ms late relative to v4l2 video.
  - A keypress's host time is at most one event-loop turn late.
  - Neither is corrected in Phase 4 (backlog: measure against a real clap).

### R6. Recording lifecycle

```
Idle ──ToggleRecording──▶ Active(starting) ──first video buffer at mux──▶ Active ──Toggle/StopRecording──▶ (stop, blocking) ──▶ Idle
                           │ Toggle/StopRecording, ERROR, or 5 s with no video
                           ▼
                          Idle (file deleted, no clip, error shown if any)
```

**Commands.** `ToggleRecording { zoom }`, `StopRecording` and `Zoom { host_ns, zoom }`.
- R and the Record/Stop button send `ToggleRecording`. The bus decides: it starts while Idle, and stops (or aborts, while starting) while Active. The UI's status can lag the bus's, so the UI doesn't choose; a second R during start-up cancels, which is what the user means.
- Esc sends `StopRecording`, a no-op while Idle.
- R and Space act only on a key's first press, never its auto-repeat: holding R would toggle the recording on and off.

**Preconditions for Start:**
- a project is open;
- it has sources and none is missing;
- the current source is loaded or loading.

Otherwise the command is refused with a message. A skip or scrub in flight is **not** a reason to refuse: the start position is where the player is heading.

**Start sequence:**
1. The bus resolves the capture devices (R2). With no usable camera it refuses here, before anything changes.
2. It pauses the player (as on macOS, every clip starts on a still frame).
3. It records `pending = { clip_id, source_index, start_source_seconds }`. Both the source and the position follow the same order as the R10 anchor: the burst target (located, since it can lie in the next source), else the in-flight target, else the pipeline's position.
4. It starts the recorder and reads `t0_ns = base_time` as soon as `set_state(PLAYING)` returns.
   - Don't wait for PLAYING. `matroskamux` waits for data on every pad, so the pipeline doesn't reach PLAYING until the camera's first frame arrives. `base_time` is fixed when `set_state` returns (measured).
5. It creates the `RecordingLog` (R8) with t0, the UI's zoom and `start_source_seconds`.

There is one **Active** state from here on. Until the first video buffer reaches the muxer, it is flagged *starting*. That flag changes only two things:
- the UI label ("Preparing…");
- what stopping does: a stop, a recorder ERROR, or 5 s with no video **abort**. The pipeline goes to NULL, the file is deleted and no clip is made.

Transport and zoom during the camera warm-up are logged like any other. File time 0 is `base_time`, so the warm-up is recorded audio, and a key pressed there really happened at that record time. (macOS ignored keys while starting because its time 0 was the first frame; that reason doesn't apply here.)

A failed `Recorder::start` also deletes the file, which filesink has already created at 0 bytes.

**Stopping is synchronous on the bus thread.** `Recorder::stop()` and the bus's closing steps:
1. **Stop logging:** the controller's `finish()` is called first, so no event can outlast the file.
2. **Finalize:** `stop()` sends EOS, waits on the recorder's own GStreamer bus for EOS or ERROR (at most 5 s), sets NULL, and returns the duration.
3. **Duration:** the latest buffer end, in running time, that reached the muxer. A pad probe tracks it. After a clean EOS this is the file's duration, so Discoverer isn't needed. After a timeout or error it is still a good estimate of what was written.
4. **Save:** the bus builds the clip (R9) and saves. **The clip is always built once the first video frame arrived.** Losing a coach's commentary over a finalization hiccup is the worst outcome, and the file is playable either way.
5. **Notify:** if the stop timed out or errored, the user also gets a notice.

A typical stop takes <100 ms, so no Stopping state or "ignore R while stopping" rule is needed.

**Closing the window while recording** runs the same stop, builds the clip and saves before the bus exits. While still starting, it aborts instead.

**One guard while recording.** While Active, the bus refuses every command except:
- TogglePlay, Skip, SetVolume;
- Zoom, ToggleRecording, StopRecording;
- GlReady, Shutdown.

`Zoom` while Idle is ignored. The UI sends every zoom change, so a change made before its status catches up isn't lost.

This replaces a per-command list, so commands added later (Phase 3's delete and undo) are refused by default. The UI greys out the sidebar, the menus and the scrubber from one `recording` property.

**Timer:** the bus's single `deadline` slot (the skip debounce) becomes two deadlines, adding the starting timeout. That one is derived, not stored: while Active and starting, it is the start instant plus 5 s. Deadlines that have passed are dispatched after every input, not only on a receive timeout, so a busy channel (level messages at 10 Hz) can't starve them.

### R7. Crash recovery: none, by design

A crash loses the in-memory event log and the pending start position, so an orphaned `.mkv` can't become a meaningful clip. The file stays on disk, playable.

The parent spec's "fallback for unknown duration after a crash" is dropped. R6 already covers the non-crash case (timeout or error at stop). Listing and cleaning up unreferenced recordings belongs with Phase 3's trash handling (backlog).

### R8. `RecordingLog`: `RecordingController` ported to core, as a pure event log

`video-coach-core` gains `recording.rs`, which ports `RecordingController.swift` as `RecordingLog` (it controls nothing). The Swift version's injected clock is replaced by caller-supplied times. The log lives on the bus.

- `new(t0_ns, zoom, start_source_seconds)` writes `zoom @0` then `pause(start) @0`. The order is an invariant of construction, not something the caller must remember.
- `play(host_ns, source_seconds)` and `pause(host_ns, source_seconds)` take a caller-captured time and the anchor chosen by the bus (R10).
- `skip(host_ns, delta)` records the **requested** delta, as macOS does. Replay clamps within the source.
- `zoom(host_ns, zoom)`:
  - drops a value equal to the last one;
  - if more than 100 ms have passed since the last capture, first emits an anchor keyframe holding the previous value at `max(t − 1 ms, last event time)`, so the log stays sorted. macOS could break the order here, which makes `debug_assert_sorted` panic;
  - never throttles.
- `finish() -> Vec<CommentaryEvent>`.
- `record_time` is clamped to ≥ the last event's time. The log starts with events at 0, so this also covers a `host_ns` captured just before t0.
- **Tests:** port `apple/Tests/AppTests/RecordingZoomCaptureTests.swift`, plus the construction order, the sorted anchor and dedupe.

### R9. Clip construction lives on `Project`

`Project::add_recorded_clip(pending, duration, events, created_at) -> &Clip`:

- `id = pending.clip_id`, and `recording_filename = "<id>.mkv"`.
- `name = "<source_index + 1>-HH:MM:SS"`, with `start_source_seconds` floored (macOS `defaultClipName`).
- `sort_index = max(existing) + 1`, or 0 for the first clip. macOS's `clips.count` duplicates after a delete.
- `show_pip = preferences.pip_for_new_recordings`, which defaults to true. It has no UI until export needs one (backlog).
- `created_at` is passed in (RFC3339), because core has no clock.
- `notes`, `tags` and `transcript` start empty.
- No format bump: every field already exists in v7.

### R10. Transport during recording

**Allowed while recording:** Space, skips (±3/±10) and zoom (keys, Ctrl+scroll, scroll pan, drag pan). Each is logged through R8 with the UI-captured `host_ns`.

**The pause and play anchor** is chosen by the bus:
- If a skip burst is outstanding, the anchor is the SkipCoordinator's burst target. Otherwise, if a seek is in flight, it is `player.target_secs()`. The pipeline hasn't got there yet, but live playback will: the skip ends in an accurate seek to that target, advanced by the play time since the burst began, while playing, so live matches replay's model (base + deltas + elapsed).
- Otherwise it is the `query_position` the UI read synchronously at the keypress. The UI already holds a `PositionHandle`.

**Clamped to the clip's source.** A clip points into one source.
- Skips are clamped to `[offset(src), offset(src) + duration(src) − 0.05]` in concat time. `SkipCoordinator::request_skip` takes the range as a parameter.
  - Replay clamps a skip to the source's full duration, so after an end-clamped skip followed by a backward skip the two can differ by 50 ms. Accepted.
- **EOS** pauses instead of advancing, and **no Pause event is logged**. Replay's play tail and freeze reproduce it. A bus-side timestamp would break the caller-captured rule.
- The scrubber is disabled.

**The Phase 2 fix: pausing keeps the frame that was on screen.**

- **The problem.** Going from PLAYING to PAUSED prerolls the frame *after* the one displayed, while `query_position` stays inside the displayed frame. Measured 20/20 on both the system and GL sinks.
- **The rule.** The sink shows a preroll only when it is the first frame since a flush or a new stream: a seek, a load or a reload. A pause's preroll is not put in the mailbox, and the frame already on screen stays up.
- **The result.** The displayed frame, the logged anchor and replay's "last frame with PTS ≤ anchor" agree, with no seek. On resume, the prerolled frame is rendered at its own PTS, so playback continues gapless from the held frame (measured 20/20).
- **Mechanism.** An atomic `fresh` flag in `sink.rs`:
  - set by a pad probe on `FLUSH_STOP`/`STREAM_START`;
  - cleared by `new_sample`;
  - read by `new_preroll`.

  Everything runs on the streaming thread, in order.
- **What is untouched:** the seek slot, the SkipCoordinator and the bus. The pause still settles on its own `ASYNC_DONE`.
- **Rejected alternatives:**
  - an ACCURATE re-seek after every pause. It displaced in-flight skips in the latest-wins slot, fired on EOS and error pauses, and cost a decode per pause.
  - snapping the anchor to the prerolled PTS. It would log a frame the coach never saw.
- **Tests:**
  - after a pause settles, the mailbox is empty or holds a frame covering `query_position`;
  - an accurate seek while paused still delivers its frame.

This fix applies whenever the user pauses, recording or not, so the plan lands it as its own first task.

### R11. Recording UI

- **Starting** (Active, before the first video frame): "Preparing recording…". There is no Cancel button: R, Esc or Stop aborts.
- **Recording:**
  - a red dot, "Recording", and elapsed time since t0 at 1 Hz, via the existing `format_hms`;
  - a mic level bar;
  - **Stop**, with the tooltip "Stop recording (R or Esc)".
- **Level bar:**
  - the maximum over channels of peak dB from the `level` element, mapped from −60…0 dBFS onto the bar's width;
  - shows "Waiting for audio…" until the first level message arrives.
- **No live camera preview** (macOS parity).
- **Clips list:** the sidebar gains a **Clips** section below Sources, with each clip's name and duration (`format_hms`), ordered by `sort_index`. There is no selection or editing yet (Phase 3).
- **Errors:** new `UserError` variants. They cover:
  - device missing (a notice; the recording still goes ahead);
  - no usable camera;
  - recording failed (pipeline error, or no video within 5 s);
  - the stop didn't finalize cleanly (a notice; the clip is kept).
- **Notices** (`UserError::is_notice`) never open the modal error dialog, which would swallow a recording's transport keys. The latest shows in a line in the transport bar for about 6 s. Errors use the existing dialog.

---

## Crate responsibilities

| Crate | Phase 4 contents |
|---|---|
| `video-coach-core` | `recording.rs` (the controller); `Project::add_recorded_clip`; `SkipCoordinator` range parameter. |
| `video-coach-media` | `Recorder`: the pipeline from R1 with injected sources, the forced clock, t0 = base_time, a first-video-buffer signal, a last-buffer-end probe, a synchronous `stop()`, and level and error messages. `devices`: enumeration of usable devices, the caps choice (R3), the encoder choice (R4). |
| `video-coach-app` | Bus: the recording state machine (R6), Toggle/Stop/Zoom commands, the generation tag on recorder messages, the one guard, anchor choice, clamping, named deadlines, stop on shutdown. UI: R/Esc, the recording transport, the level bar, the Devices popover, the Clips list. |
| `video-coach-harness` | End-to-end recording with test sources. |

---

## Testing

- **Core.**
  - Controller:
    - construction writes `zoom, pause @0`;
    - play and pause use caller times and anchors;
    - skip logs the requested delta;
    - zoom dedupe;
    - anchor keyframe after 100 ms of quiet, and never before the last event;
    - no throttling;
    - `record_time` ≥ 0.
  - `add_recorded_clip`: name formatting, `sort_index` after a gap, `show_pip` from preferences.
  - SkipCoordinator range clamp.
- **Media** (test sources, no hardware):
  - A 2 s recording yields an `.mkv` with H.264 and Opus. The returned duration is within one frame of the file's duration as measured by Discoverer.
  - **Delayed video** (a valve held closed 0.5 s): the first video PTS in the file equals the first video buffer's running time from `base_time`, and audio starts at 0. This is the test that would have caught the t0 bug; instant test sources can't.
  - Stop returns within the timeout. Stop on a pipeline whose EOS never arrives returns after the timeout with a duration.
  - Encoder and caps choices are pure-function tests.
- **Harness** (bus end to end, test sources):
  - Start → Recording (with a mic level) → Stop → a clip appears with the right `source_index`, `start_source_seconds`, duration, and a log starting `[zoom, pause]`. The file exists.
  - `sort_index` is `max + 1` after a manual gap.
  - A second toggle while starting (camera delayed) leaves no clip and no file.
  - TogglePlay during the warm-up is logged.
  - Start with a missing source is refused.
  - A command outside the guard (e.g. AddSource) is refused while recording.
  - A skip is clamped to the source.
  - Skip then an immediate pause logs the skip target as the anchor.
  - Shutdown while recording saves the clip.
- **Manual** (reference laptop, batched with the other hands-on checks):
  - the real webcam and mic, and the webcam light;
  - playback of a recording looks lip-synced;
  - the level bar moves;
  - the device choice persists, and the IR camera is absent;
  - QP tuning in a lit room;
  - `kill -9` mid-recording leaves a playable file.

## Risks

1. **CQP-only hardware encoding:** file size isn't bounded by a bitrate.
2. **The UVC exposure control** needs `v4l2src`. Under Flatpak (Phase 11) that means device access; the portal path can't set it.
3. **Audio is arrival-stamped:** about 20–40 ms of A/V offset, unmeasured.
4. **Wayland and portals** are untested.
5. **A present-but-broken VA encoder** fails the recording instead of falling back to x264 (backlog).
6. **Stop blocks the bus thread** for up to 5 s in the failure case. The UI keeps rendering, and the commands queue.

## Deferred (→ BACKLOG)

- Measure and correct the A/V offset against a real clap.
- List and clean up orphaned recordings (with Phase 3's trash).
- Fall back to x264 when a VA encoder is present but fails.
- Camera fallbacks: non-16:9, non-30 fps, `videorate`.
- The `pulsesrc` fallback for PulseAudio-only systems.
- A device list that updates live while the popover is open.
- The PiP checkbox (`pip_for_new_recordings`), with Phase 8 or the Phase 3 inspector.
- The start flash, the level meter's peak hold and colour gradient (macOS polish).
- Global rather than per-project device preferences.
