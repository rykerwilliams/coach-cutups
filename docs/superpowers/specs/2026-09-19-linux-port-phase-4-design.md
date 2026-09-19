# Linux Port — Phase 4: Capture (Commentary Recording)

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 4, and the Milestone C note: Phase 4 runs before Phase 3)
**Builds on:** `docs/superpowers/specs/2026-09-19-linux-port-phase-2-design.md` (bus, player, transport, zoom)
**Evidence:** the capture research on the reference laptop, summarized in "Measured facts"; and the macOS inventory of `RecordingController.swift`, `CaptureSessionController.swift`, `DeviceCatalog.swift` and `ContentView.swift`'s recording flow.

---

## Goal

A coach presses **R** while scanning. The app records their webcam and microphone while they talk over the game footage and steer it: play, pause, skip, zoom. Pressing **R** again produces a **clip**: a pointer into the game video, the commentary recording, and the log of what they did. The clip appears in the sidebar.

Clip editing, tags, delete and undo belong to Phase 3. Stroke drawing belongs to Phase 6. Export belongs to Phases 5 and 8.

## Done when

1. With a project whose sources are all present, **R** starts a recording. The transport shows "Preparing…", then a red **Recording** indicator with elapsed time and a live mic level. The webcam light is on only while recording.
2. During the recording, Space, skips and zoom all work. Each action lands in the clip's event log with a timestamp that lines up with the recording.
3. **R** (or Esc, or Stop) ends it. A clip named `<source#>-HH:MM:SS` appears in the sidebar's Clips list. `project.json` holds its source index and start time, its duration read from the recorded file, and an event log that starts `[zoom @0, pause @0]`. `recordings/<uuid>.mkv` plays in any player.
4. The camera and microphone can be chosen. The choice persists, and a missing device falls back to the default without forgetting the preference.
5. Killing the app mid-recording leaves a playable `.mkv` behind. Nothing else is recovered (R7).
6. Pausing shows the frame at the paused position, not the next one (Phase 2 fix, R10).

---

## Measured facts (reference laptop, 2026-09-19)

These come from the capture research. They are measured, not assumed.

- **Devices.** `gst::DeviceMonitor` sees only PipeWire devices by default: the PipeWire provider hides the v4l2 and pulse duplicates.
  - The laptop exposes **two cameras with the same display name**: the RGB webcam (`/dev/video0`, MJPEG and YUY2) and an IR face-unlock camera (`/dev/video2`, GRAY8 only).
  - **Stable across reboots:** `node.name` (e.g. `v4l2_input.pci-0000_00_14.0-usb-0_6_1.0`, `alsa_input.pci-…analog-stereo`), and the camera's `device.serial`.
  - **Volatile:** `object.id`/`object.serial`, `node.id`, `/dev/videoN`, and `object.path` (which embeds videoN). `/dev/v4l/by-id` is wrong for this camera: the RGB and IR interfaces share a serial, and the symlink points at the IR camera.
- **Camera formats.** 16:9 at ≤1280 wide and 30 fps exists only as **1280×720 MJPEG**. YUY2 tops out at 640×480.
- **Low light drops the frame rate.** The UVC control `exposure_dynamic_framerate=1` let the camera fall to **7.5 fps** in a dark room while the caps still said 30/1. `v4l2src extra-controls="c,exposure_dynamic_framerate=0"` restored 30 fps. `pipewiresrc` can't set V4L2 controls.
- **Timestamps.** `v4l2src` stamps buffers with the kernel capture time, about 28 ms before arrival. `pipewiresrc` stamps them on arrival. Video reaches the muxer 52–96 ms after capture (median 58 ms), after decode and encode.
- **Encoders.**
  - `vajpegdec ! vah264lpenc`: **8% CPU** at 720p30.
  - `x264enc speed-preset=ultrafast tune=zerolatency`: 110%. `veryfast`: 292%, not usable live.
  - `vah264lpenc` is the only VA H.264 encoder here, and it is **CQP-only** on this driver: bitrate settings are ignored, so the QPs must be set explicitly.
  - `vaapih264enc` (the deprecated gstreamer-vaapi plugin) hung a test harness after a negotiation error.
  - Both `va` elements are rank 0, so they must be named explicitly.
- **Clock.** `GstSystemClock` is CLOCK_MONOTONIC. The pipeline picks its clock from the audio source. `pipewiresrc`'s clock happens to track monotonic time. **`pulsesrc`'s clock was about 473,000 s off.** With `use_clock(SystemClock)` forced, both line up within milliseconds.
- **Timestamp values.** `vah264lpenc`'s output PTS carries a +3600 s offset, so use running time, never raw PTS. `t0 = base_time + running_time` of the first video buffer at the muxer equals the file's video start time.
- **Crash safety.** After `kill -9`:
  - non-streamable `matroskamux` with `filesink buffer-mode=unbuffered` left a playable file;
  - with the default buffered filesink, `streamable=true` left **0 bytes**;
  - Discoverer's duration for a crashed file is an estimate and can be badly wrong (1.19 s reported for a 3.2 s file).
  - A clean stop (EOS → wait for the bus EOS → NULL) writes a correct duration.
- **Level meter.** `level interval=100ms` posts rms/peak dB per channel on the bus, about every 99 ms.
- **Two pipelines at once.** VA decode for playback and VA encode for capture ran together in one process with no contention: 27 fps playback, 28 fps capture, 38% CPU total.
- **No portal needed on X11.** PipeWire access is unrestricted and `/dev/video0` has a uaccess ACL. No prompts.
- **Frame accuracy.**
  - In PAUSED after an ACCURATE seek, `query_position` equals the target exactly.
  - While PLAYING, it runs 0.6–34 ms (median 20 ms) ahead of the PTS of the displayed frame, and the displayed frame is the one covering that position.
  - The query takes about 150 µs and is synchronous.
  - **After a pause, the sink prerolls and shows the next frame** (+33 ms, 12 of 12 trials) while the position stays at the anchor.

---

## Decisions

### R1. Recording is a second pipeline owned by the bus

`video-coach-media` gains a `Recorder`, and the bus owns it next to the `SourcePlayer`. The recording pipeline is separate from the playback pipeline and runs alongside it (measured: no contention).

```
v4l2src device=<path> extra-controls=c,exposure_dynamic_framerate=0
  ! image/jpeg,width=1280,height=720,framerate=30/1        (caps chosen per R3)
  ! queue ! <jpeg decode> ! <h264 encode> ! h264parse ! queue ! mux.
pipewiresrc target-object=<mic node.name>
  ! audio/x-raw,rate=48000,channels=2 ! queue ! audioconvert ! audioresample
  ! level interval=100000000 ! opusenc bitrate=96000 ! queue ! mux.
matroskamux name=mux ! filesink buffer-mode=unbuffered location=recordings/<uuid>.mkv
```

- **Clock:** always `pipeline.use_clock(Some(&gst::SystemClock::obtain()))`. Measured: pulsesrc's clock can be days off monotonic.
- **Camera source:** `v4l2src`, for its kernel capture timestamps and because it can set V4L2 controls. The device path is resolved from the PipeWire device at record time, never stored.
- **Microphone source:** `pipewiresrc target-object=<node.name>`, falling back to `pulsesrc` when PipeWire is absent.
- **Frame rate:** if the camera lacks `exposure_dynamic_framerate`, a `videorate` element holds 30/1 instead.
- **Sources are injectable,** exactly as the player's sinks are (D1 in Phase 2). Tests build the recorder with `videotestsrc is-live=true` and `audiotestsrc is-live=true`, so media and harness tests run with no camera, no microphone and no display.

### R2. Devices: enumerate via PipeWire, key on `node.name`

- **Enumeration:** `gst::DeviceMonitor` for `Video/Source` and `Audio/Source`. Drop cameras whose caps offer only GRAY8, so the IR camera never shows up as a webcam choice.
- **Display:** the device's display name. When two remaining devices share a name, append a short distinguisher (for example " (2)"). Never match a device by its name.
- **Stored key:** `node.name` goes in `Preferences.preferred_camera_id` / `preferred_mic_id`, the fields that already exist. For cameras, `device.serial` plus the interface index is kept as a soft fallback, so a USB camera moved to another port is still found.
- **Missing device:** if the stored device is absent at record time, record with the default device and show a notice. The preference is **not** cleared (macOS behavior; parent spec).
- **Scope:** preferences are **per project**. That is macOS parity, and the format already has the fields there.
- **UI:** a Devices popover (camera list, microphone list, "System default") opened from a transport-bar button. It is disabled while recording. The device list refreshes when devices are added or removed.

### R3. Camera format

- Choose the largest **16:9** mode that is **≤1280 wide at 30/1** from the device's caps. At equal size, prefer raw over MJPEG. On the reference laptop the only match is 1280×720 MJPEG.
- If no 16:9 mode exists, use the largest mode ≤1280 wide at 30/1, whatever its shape. The picture-in-picture (Phase 8) letterboxes it.
- If no 30/1 mode exists at all, use the closest rate with `videorate` to 30/1.
- Refusing to record is reserved for a camera with no usable format at all. That is macOS's `noSuitableFormat`, but narrower.

### R4. Encoder: probe, name explicitly, CQP

Probe once per session, in this order, keeping the first chain that negotiates in a short live test:

1. `vajpegdec ! vah264lpenc rate-control=cqp qpi=<i> qpp=<p> key-int-max=30` (8% CPU measured)
2. `vah264enc` with the same settings, where the hardware offers it
3. `jpegdec ! videoconvert ! x264enc speed-preset=ultrafast tune=zerolatency key-int-max=30` (110% CPU measured)

- Raw camera modes skip the JPEG decode.
- **Never** use `vaapih264enc`.
- **QP values:** start at qpi=24/qpp=26, from the research. Tune them by eye in a lit room during the plan's closeout, and record the result. The driver is CQP-only, so file size isn't bounded by a bitrate.
- **Audio:** Opus at 96 kbit/s.

### R5. One clock for timestamps: CLOCK_MONOTONIC nanoseconds

Every timestamp in a recording is a CLOCK_MONOTONIC nanosecond count: `gst::SystemClock::obtain().time()`, or `clock_gettime(CLOCK_MONOTONIC)`. `std::time::Instant` uses the same clock, but it exposes no raw value.

- **t0:** a buffer probe on the muxer's video sink pad records the first buffer. `t0_ns = pipeline.base_time() + segment.to_running_time(buffer.pts)`. This is the first frame's capture time on the monotonic clock, and it matches the file's video start. Raw PTS is never used: `vah264lpenc` adds a +3600 s offset.
- **Event times:** every event-log `record_time` is `(host_ns − t0_ns) / 1e9`, where `host_ns` is captured **by the caller at the input event**. That is the Phase 2 bus contract: the UI thread reads the monotonic clock when the key is pressed, not when the bus handles the command.
- **Stated error budget:**
  - Audio timestamps come from arrival time (pipewiresrc), so audio may sit about 20–40 ms late relative to v4l2 video.
  - A keypress's host time is taken on the UI thread, which is at most one event-loop turn late.
  - Both are well under one video frame at the replay's 30 fps. Neither is corrected in Phase 4 (backlog: measure against a real clap).

### R6. Recording lifecycle and states

```
Idle ──R──▶ Starting ──first video buffer at mux (t0)──▶ Recording ──R/Esc/Stop──▶ Stopping ──bus EOS──▶ Idle
              │ R/Esc/Cancel, error, or 5 s with no first frame
              ▼
             Idle (partial file deleted, error shown if any)
```

- **Preconditions for `StartRecording`:**
  - a project is open;
  - it has sources and **none is missing**;
  - the current source is loaded;
  - the player's seek slot is idle.

  Otherwise the command is refused with a message. macOS checked none of the source conditions.
- **When Starting begins:**
  1. The player is paused, as on macOS, so every clip starts on a still frame.
  2. The bus captures `pending = { clip_id, source_index, start_source_seconds }` from the **bus-owned** current position. macOS used mpv's `playlistPos`, which was wrong after cross-source seeks.
  3. The recorder starts.
- **Starting is cancellable.** R, Esc or a Cancel button abort it (macOS swallowed every key for up to 10 s). So does a 5 s timeout with no first frame. Aborting stops the pipeline and deletes the partial file.
- **Recording begins at t0.** The bus creates the `RecordingController` (R8) with t0 and the UI's initial zoom. The log gets `zoom(current) @0`, then `pause(start_source_seconds) @0`, in that order.
- **Stopping ignores further R/Esc.** macOS's double stop leaked a continuation and let events land after the file's end. The bus then:
  1. sends EOS and waits for the bus EOS, with a 5 s timeout;
  2. sets NULL;
  3. reads `recording_duration` from the finished file with Discoverer (reliable after a clean stop);
  4. builds the clip (R9) and saves.

  If the EOS wait times out, the file is kept but no clip is created, and the user is told (backlog: repair).
- **Events are not logged after Stop is pressed.** The controller's `finish()` is called when Stopping begins, so no event can outrun the file.
- **Closing the window while recording** stops cleanly before exit. The teardown acknowledgement waits for the stop.

### R7. Crash recovery: none, by design

A crash loses the in-memory event log and the pending start position, so the orphaned `.mkv` can't become a meaningful clip. The file is left on disk and stays playable (measured). The parent spec's "fallback for unknown duration after a crash" is **dropped**: there's no clip to give a duration to. Listing and cleaning up unreferenced recordings belongs with Phase 3's trash handling (backlog).

### R8. `RecordingController` is ported to core, as a pure event log

`video-coach-core` gains `recording.rs`, which ports `RecordingController.swift` with its injected clock replaced by caller-supplied times.

- `new(t0_ns)`.
- `append_initial(zoom, start_source_seconds)`: `zoom @0`, then `pause @0`.
- `append_play(host_ns, source_seconds)` / `append_pause(host_ns, source_seconds)`: **caller-captured** time and source anchor.
- `append_skip(host_ns, delta)`: records the **requested** delta, as macOS does. Replay clamps within the source, and `playback_segments` already clamps.
- `append_zoom(host_ns, zoom)`:
  - drops a value equal to the last one;
  - if more than 100 ms have passed since the last capture, first emits an anchor keyframe at `t − 1 ms` holding the previous value;
  - **never throttles.** macOS's 20 Hz workspace throttle had no trailing flush and could drop a gesture's final value (Phase 2 D9).
- `finish() -> Vec<CommentaryEvent>`.
- `record_time` is always clamped at ≥ 0. A key pressed during Starting can't produce a negative time, because events are only accepted in Recording.
- **Tests:** port `apple/Tests/AppTests/RecordingZoomCaptureTests.swift` and the controller's documented invariants (initial event order, anchor keyframe, dedupe).

### R9. Clip construction is a pure function in core

`Clip::from_recording(pending, duration, events, show_pip, existing_clips) -> Clip`:

- `id = pending.clip_id`, and `recording_filename = "<id>.mkv"`.
- **`name`** is `"<source_index + 1>-HH:MM:SS"`, with `start_source_seconds` floored (macOS `defaultClipName`).
- **`sort_index = max(existing) + 1`**, or 0 for the first clip. macOS used `clips.count`, which duplicates an index once a delete leaves a gap.
- `show_pip` is sampled **at stop**, as on macOS.
- `notes`, `tags` and `transcript` start empty. `created_at` is set at stop (RFC3339).

### R10. Transport during recording, and the pause fix

**Allowed while recording:** Space, skips (±3/±10), and zoom (keys, Ctrl+scroll, scroll pan, drag pan). Each is logged through R8 with the UI-captured `host_ns`. Play and pause also carry a **synchronous `query_position`** read on the UI thread before the toggle (Phase 2 bus contract). That position is accurate to the displayed frame (measured).

**Clamped to the clip's source.** A clip points into one source, so its log can't represent crossing into another:
- skips are clamped to `[0, source_duration − 0.05]` of the pending source;
- EOS pauses instead of advancing;
- the scrubber is **disabled** while recording.

**Disabled while recording or starting:** Open Project, Add/Remove/Move/Relink source, rename, the Devices popover, and the scrubber.

**The Phase 2 fix: pausing shows the right frame.** On every pause, recording or not, the bus follows the pause with an **ACCURATE seek to the paused position**. The prerolled frame is then the one at the anchor, not the next one (measured: +1 frame in 12/12 trials). The seek goes through the existing seek slot, and the Settling state already absorbs the pause's own `ASYNC_DONE`.

### R11. Recording UI

- **Transport while Starting:** a yellow dot, "Preparing recording…", and **Cancel**.
- **Transport while Recording:**
  - a red dot, "Recording", and elapsed time since t0 in `M:SS` (`H:MM:SS` past an hour), updated at 1 Hz;
  - the mic level meter;
  - the PiP checkbox;
  - **Stop**, with the tooltip "Stop recording (R or Esc)".
- **Level meter:**
  - fed by the `level` element's messages;
  - uses the **maximum over channels** of peak dB, mapped from −60…0 dBFS onto a 90×6 bar with a green/yellow/red gradient;
  - holds the peak for 1 s;
  - shows "Waiting for audio…" when there are no messages or everything is below −60 dB.
- **Start flash:** a red overlay fades in and out over about 400 ms.
- **No live camera preview** (macOS parity).
- **Clips list:** the sidebar gains a **Clips** section below Sources, showing each clip's name and duration (`M:SS`), ordered by `sort_index`. There's no selection, editing or deletion yet (Phase 3).
- **PiP checkbox:** "Show webcam in export" (`pip_for_new_recordings`), in the transport bar while scanning and while recording, saved on change.
- **Errors:** one dialog each for:
  - device missing or unusable;
  - no suitable camera format;
  - encoder or pipeline failure;
  - no first frame within 5 s;
  - EOS timeout on stop.

  Every path returns to Idle.

---

## Crate responsibilities

| Crate | Phase 4 contents |
|---|---|
| `video-coach-core` | `recording.rs` (the controller: log, anchors, zoom anchor/dedupe); `Clip::from_recording`, including the default name and `sort_index = max + 1`. |
| `video-coach-media` | `Recorder`: the pipeline from R1 with injected sources, the clock forced, the t0 probe, a clean stop, and level messages. `devices`: DeviceMonitor enumeration, `node.name` keying, dropping GRAY8-only cameras, the camera caps choice (R3), and the encoder probe (R4). |
| `video-coach-app` | Bus: the recording state machine (R6), recording commands and events, the start preconditions, clamping during recording, and the pause re-seek fix. UI: R key, recording transport, level meter, Devices popover, Clips list, PiP checkbox, flash. |
| `video-coach-harness` | Record with test sources; clip creation; logged events; cancel during Starting; stop while stopping ignored; a start refused when a source is missing. |

---

## Testing

- **Core.**
  - Controller: initial order `zoom, pause @0`; play and pause use caller times and anchors; skip logs the requested delta; zoom dedupe; the anchor keyframe after 100 ms of quiet; no throttling (a dense zoom stream is kept whole); `record_time` clamped to ≥ 0.
  - `from_recording`: name formatting, `sort_index` after a gap, `show_pip` sampled at stop.
- **Media** (test sources, no hardware):
  - a 2 s recording produces an `.mkv` with H.264 and Opus streams, and Discoverer reports a duration within one frame of the requested length;
  - t0 equals `base_time` plus the first video buffer's running time, and is on the monotonic clock (compare with the system clock);
  - clean stop within the timeout;
  - level messages arrive;
  - encoder probe fallback: with the VA elements' rank forced to NONE, the probe picks `x264enc`;
  - device enumeration and caps choice through pure functions over recorded caps structures, with no hardware.
- **Harness** (bus end to end, test sources):
  - R → Recording → R → a clip appears with the right `source_index`, `start_source_seconds`, duration, and a log starting `[zoom, pause]`;
  - the file exists;
  - `sort_index` is `max + 1` after a manual gap;
  - cancel during Starting leaves no clip and no file;
  - a second stop during Stopping is ignored;
  - starting with a missing source is refused;
  - a skip during recording is clamped to the source;
  - pause re-seek: after a pause, the mailbox frame's PTS covers the paused position.
- **Manual** (reference laptop, in the plan's closeout, batched with the other hands-on checks):
  - the real webcam and mic;
  - the webcam light is on only while recording;
  - a recording plays back with lip sync that looks right;
  - the level meter moves;
  - the camera and mic choices persist;
  - the IR camera is absent from the list;
  - QP tuning in a lit room;
  - killing the app mid-recording leaves a playable file.

## Risks

1. **CQP-only hardware encoding.** File size isn't bounded, so the QPs are tuned by eye (R4). A long recording in a busy scene can be large.
2. **The UVC exposure control** needs `v4l2src`. Under Flatpak (Phase 11) that means device access; the portal path, `pipewiresrc` with an fd, can't set the control.
3. **Audio is arrival-stamped:** about 20–40 ms of A/V offset, unmeasured against a real clap (backlog).
4. **Wayland and portals** are untested; the reference laptop runs X11.
5. **Camera start-up** takes 0.25–0.7 s before the first frame. t0 absorbs it, and Starting shows "Preparing…".

## Deferred (→ BACKLOG)

- Measuring and correcting the A/V offset against a real clap.
- Listing and cleaning up orphaned recordings (with Phase 3's trash).
- Repairing a recording whose stop timed out.
- Global rather than per-project device preferences, if the user wants them.
