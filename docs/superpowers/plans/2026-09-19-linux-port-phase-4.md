# Linux Port — Phase 4 Plan (Capture)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-4-design.md` (decisions cited as R1–R11)
**Status:** Draft, pre-review.

**Goal:** everything on the spec's "Done when" list works on the reference laptop.

**Execution.** Same as Phase 2:
- Each task runs in a fresh subagent that is given this plan, the spec and `CLAUDE.md`, and no chat history.
- After each task the orchestrator runs the `verify` skill and commits that task on its own.
- CI must stay green at every commit.

**Privacy.** Automated tests use `videotestsrc`/`audiotestsrc` only.
- A task that opens the real camera or microphone says so explicitly.
- Record to the scratchpad, and delete the media when done.
- Restore any V4L2 control you change.

**Known facts. Don't re-derive these** (they come from the spec's "Measured facts" and the review):
- **Pipeline timing.**
  - File time 0 = `base_time`: `matroskamux` writes running time as-is.
  - `vah264lpenc` PTS carries +3600 s.
  - `pulsesrc`'s clock is days off. Always `use_clock(SystemClock)`.
- **Encoders.** `vah264lpenc` and `vajpegdec` exist on the laptop (rank 0; name them). `vah264lpenc` is CQP-only.
- **Devices.** `DeviceMonitor` lists PipeWire devices. Camera `device.api`/props include `api.v4l2.path` (e.g. `/dev/video0`) and `node.name`. The IR camera offers GRAY8 only.
- **The pause preroll.** Going from PLAYING to PAUSED prerolls displayed+1. A pad probe on `FLUSH_STOP`/`STREAM_START` distinguishes a seek or load preroll from a pause preroll (measured 20/20 on both sinks; the probe source is `scratchpad/deliberation-pause/probe/src/main.rs`).
- **Bus internals.**
  - The bus loop multiplexes `Input::{Cmd, Gst}` on one mpsc channel with a single `deadline: Option<Instant>` (the skip debounce).
  - Player messages come in through `SourcePlayer::new`'s `on_message` callback.
  - `current_secs()` prefers `player.target_secs()`.
  - `SkipCoordinator::request_skip(delta, current, clip_duration)` clamps to `[0, clip_duration]`.
- **Dates.** `created_at` can come from `gst::glib::DateTime::now_utc()?.format_iso8601()`, with no new dependency.

---

## Task 0 — Pause keeps the frame on screen (R10, Phase 2 fix)

This task is independent of recording, so it lands first.

1. **`crates/video-coach-media/src/player/sink.rs`:**
   - Add an `Arc<AtomicBool> fresh`, initially `true`.
   - Add a pad probe on the appsink's sink pad (`EVENT_DOWNSTREAM | EVENT_FLUSH`) that sets it on `FLUSH_STOP` and `STREAM_START`.
   - `new_sample` clears it after delivering.
   - `new_preroll` delivers only while it is set.
   - Update the `FrameMailbox` doc, which currently says "Both `new_sample` and `new_preroll` fill it", to state the rule and why: a pause's preroll is the *next* frame, while the position stays on the displayed one.
2. **Player tests** (`player/tests.rs`, system sink, fixture video):
   - (a) Play about 1 s, pause, wait for the pause to settle. The mailbox is either empty after a `take()` issued just before the pause, or holds a frame whose `[pts, pts+dur)` covers `query_position`.
   - (b) An accurate seek while paused still delivers a frame covering the target.
   - (c) The existing `a_seek_right_after_a_pause_is_not_completed_by_the_pause` still passes.

   Write (a) so that it fails without the fix; check this by temporarily reverting.

Commit: `fix(media): pausing keeps the frame on screen`.

## Task 1 — Core: the recording log, clip construction, skip range

No GStreamer.

1. **`crates/video-coach-core/src/recording.rs`:** port `apple/App/Recording/RecordingController.swift` per R8. Read it and `apple/Tests/AppTests/RecordingZoomCaptureTests.swift` first, and use the `port-swift-module` skill.
   ```rust
   pub struct RecordingLog { /* t0_ns, events, last zoom, last zoom capture ns */ }
   impl RecordingLog {
       pub fn new(t0_ns: u64, zoom: Zoom, start_source_seconds: f64) -> Self; // zoom@0, pause(start)@0
       pub fn play(&mut self, host_ns: u64, source_seconds: f64);
       pub fn pause(&mut self, host_ns: u64, source_seconds: f64);
       pub fn skip(&mut self, host_ns: u64, delta: f64);
       pub fn zoom(&mut self, host_ns: u64, zoom: Zoom);
       pub fn finish(self) -> Vec<CommentaryEvent>;
   }
   ```
   - `record_time = ((host_ns as i128 − t0_ns as i128) as f64 / 1e9).max(0.0)`.
   - Then clamp it to ≥ the last event's time, so a late-arriving `host_ns` can't unsort the log. The UI thread is single, so this only matters for rounding. Say so in a comment.
   - Zoom anchor: dedupe equal values. If more than 100 ms have passed since the last zoom capture, emit the previous value at `max(t − 0.001, last event time)` first.
   - Name it `RecordingLog`, not `…Controller`: it controls nothing.
2. **`Project::add_recorded_clip(&mut self, recording: RecordedClip) -> &Clip`** in `project.rs`, where `RecordedClip { id: Uuid, source_index, start_source_seconds, duration, events, created_at: String }`. Per R9:
   - the name is `"{}-{:02}:{:02}:{:02}"` from `source_index + 1` and floored `start_source_seconds`;
   - `sort_index` = max + 1, or 0 for the first clip;
   - `show_pip` comes from `preferences.pip_for_new_recordings`;
   - `recording_filename = "<id>.mkv"`.
3. **`SkipCoordinator`:**
   - `request_skip(delta, current, range: RangeInclusive<f64>)` replaces `clip_duration`. The `.max` guard against inverted bounds stays: build the range defensively, and keep the comment about `f64::clamp` panicking.
   - Add `pub fn target(&self) -> Option<f64>`.
   - Update the existing bus call site to `0.0..=(total − END_MARGIN).max(0.0)`.
4. **Doc fixes:**
   - `event.rs`'s module doc says record time is "seconds since the first frame of the commentary recording". It is now "seconds since the recording's time 0, the capture pipeline's base time (Phase 4 R5)".
   - `Preferences.preferred_camera_id`'s doc: "PipeWire `node.name`". Drop the `/dev/v4l/by-id` mention, which the spec measured as wrong.
5. **Tests** (`crates/video-coach-core/tests/recording.rs` and additions to `project_format.rs`/`skip.rs`), as listed in the spec's Testing → Core:
   - construction order;
   - caller times and anchors;
   - the requested skip delta;
   - zoom dedupe;
   - an anchor after 100 ms quiet and never before the last event;
   - a dense zoom stream kept whole;
   - `record_time ≥ 0`;
   - the name format (including `3725.9 s` → `…-01:02:05`);
   - `sort_index` after a gap;
   - `show_pip` from preferences;
   - the skip range clamp at both ends.

Commit: `feat(core): recording log, recorded-clip construction, skip range`.

## Task 2 — Media: devices and encoder choice

Add `crates/video-coach-media/src/capture/{mod.rs,devices.rs}`.

1. **Pure functions,** tested without hardware:
   - `choose_camera_caps(&gst::Caps) -> Option<CameraMode>`, per R3. `CameraMode { width, height, mjpeg: bool }`: the largest 16:9, ≤1280-wide, 30/1 structure. `image/jpeg` → `mjpeg = true`; `video/x-raw` of any format → false. Structures may carry lists or ranges of framerates; handle `gst::List`, a fixed fraction, and `gst::FractionRange`.
   - `is_ir_only(&gst::Caps) -> bool`: every structure is `video/x-raw` with `format=GRAY8`.
   - `choose_encoder(has: impl Fn(&str) -> bool, mjpeg: bool) -> EncoderChain`, per R4. `EncoderChain` is an enum `{ Va, Software }` with a method that builds the elements, so the tests assert on the variant, not on strings.
   - **Tests:** build caps from strings mirroring the laptop's cameras. Get them with `gst-device-monitor-1.0 Video/Source`; this only lists devices and doesn't open the camera. Also test the IR camera, a camera with no 16:9 mode, and a framerate-list camera.
2. **Enumeration:**
   - `pub fn list_devices() -> Devices { cameras: Vec<Device>, mics: Vec<Device> }`, where `Device { name: String /* node.name */, label: String, v4l2_path: Option<String>, caps: gst::Caps }`.
   - It starts a `DeviceMonitor` with `Video/Source` and `Audio/Source` filters, reads `devices()`, stops it, and drops IR-only cameras.
   - Keep only devices that have a `node.name` property (PipeWire). Check on the laptop that the property is literally `node.name` in `device.properties()`.
3. **`pub fn resolve(devices: &Devices, preferred: Option<&str>, kind) -> Option<(&Device, bool /* fell back */)>`:** the preferred device by `node.name`, else the first. The spec calls it the "system default". `DeviceMonitor` orders PipeWire's default first; confirm this on the laptop with `gst-device-monitor-1.0`, and if not, use the device whose props say `is-default`.
4. **`pub fn now_ns() -> u64`** in `capture/mod.rs`: `gst::SystemClock::obtain().time()` as ns. This is the only clock for `host_ns` and t0 (R5).

Commit: `feat(media): capture device enumeration, format and encoder choice`.

## Task 3 — Media: the Recorder

Add `crates/video-coach-media/src/capture/recorder.rs`, per R1, R4, R5 and R6.

1. **API.**
   ```rust
   pub enum CaptureSources { Devices { camera: Device, mode: CameraMode, mic: Device }, Test { video_delay: Duration } }
   pub enum RecorderMessage { FirstVideo, Level { peak_db: f64 }, Error(String) }
   pub struct Recorder { /* pipeline, generation, t0_ns, last_end: Arc<AtomicU64> */ }
   impl Recorder {
       /// Builds and starts to PLAYING; t0 is read once PLAYING is reached.
       pub fn start(sources: CaptureSources, path: &Path, generation: u64,
                    on_message: impl Fn(u64, RecorderMessage) + Send + Sync + 'static) -> Result<Recorder, String>;
       pub fn t0_ns(&self) -> u64;
       /// EOS, wait ≤ timeout for EOS/ERROR on the pipeline's own bus, NULL.
       pub fn stop(self, timeout: Duration) -> StopOutcome; // { duration: f64, clean: bool }
       /// NULL without EOS, for an aborted start; the caller deletes the file.
       pub fn abort(self);
   }
   ```
2. **Construction.** Build the elements in code, not with `parse_launch`, so the encoder chain from Task 2 plugs in.
   - `Test` uses `videotestsrc is-live=true pattern=ball` at 1280×720/30 and `audiotestsrc is-live=true wave=ticks`. With `video_delay > 0`, a `valve drop=true` opens after the delay on a timer thread. The Test source always uses `x264enc`, or VA when present? Always `choose_encoder` with raw input, so the tests exercise the production encoder on the laptop and x264 on CI.
   - CI needs `x264enc` (`gstreamer1.0-plugins-ugly`), `opusenc` and `matroskamux` (base/good). Add them to `.github/workflows/rust.yml`'s apt line.
3. **Settings and probes.**
   - `pipeline.use_clock(Some(&gst::SystemClock::obtain()))`.
   - Set `matroskamux offset-to-zero=false` explicitly, with a comment pointing at R5.
   - `filesink buffer-mode=unbuffered`.
   - A buffer probe on each mux sink pad records `max(running_time(pts) + duration)` into `last_end`.
   - The video pad's probe also sends `FirstVideo` once.
4. **Messages and stopping.**
   - Messages from the pipeline's bus: a **sync handler** forwards `ERROR` (as `Error`) and `level` element messages (`Level`, with the max of the `peak` array over channels), tagged with the generation.
   - It returns `BusSyncReply::Pass` for EOS and ERROR, so `stop()` can `timed_pop_filtered` them, and `Drop` for the rest.
   - Because only EOS and ERROR are passed, nothing else accumulates in the bus queue.
   - `stop()`'s duration is `last_end` in seconds. `clean` is true if EOS arrived before the timeout.
5. **Tests** (`crates/video-coach-media/tests/recorder.rs`, Test sources, scratch dir):
   - (a) Record 2 s and stop. The file has H.264 and Opus streams (Discoverer). `|StopOutcome.duration − Discoverer duration| ≤ 1/30 s`, and `clean`.
   - (b) With a 0.5 s video delay: the first video PTS in the file (demux and probe, as in the spec) ≈ 0.5 s (±50 ms), the first audio PTS < 50 ms, and `FirstVideo` arrived.
   - (c) At least one `Level` message within 1 s.
   - (d) `t0_ns()` is within 1 s before `now_ns()` taken after `start` returns.
   - (e) Stop timeout: build a recorder whose EOS can't complete, and assert `stop` returns after about the timeout with `clean == false`. For example, block the audio branch with a pad probe that drops EOS. If that is awkward, test a short timeout on a normal pipeline instead, and note it.
6. **Hardware check** (the laptop; **opens the real camera and mic** for about 3 s):
   - Record with `Devices` from Task 2 into the scratchpad.
   - Confirm the file plays (Discoverer) and the element names in use (`vajpegdec`, `vah264lpenc`).
   - Confirm `extra-controls` with an unsupported control only warns: set a bogus control name on the same camera once.
   - Delete the file. Record the results in this plan's Task 3 notes.

Commit: `feat(media): commentary recorder`.

## Task 4 — Bus: the recording state machine

Split this into files: `bus/recording.rs` holds the state machine, while `bus/mod.rs` gets the command and event variants and the loop changes.

1. **Commands and events.**
   - `Command::TogglePlay { at: Stamp }` and `Command::Skip { delta, at: Stamp }`, where `Stamp { host_ns: u64, source_secs: Option<f64> }` is captured on the UI thread (`now_ns()` and `PositionHandle::query_position()`).
   - `Command::StartRecording { zoom: Zoom }`, `Command::StopRecording`, `Command::Zoom { host_ns, zoom }`.
   - `Command::SetDevices { camera: Option<String>, mic: Option<String> }` persists the preferences.
   - `Event::Recording(RecordingStatus)`, where `RecordingStatus { Idle, Starting, Recording { t0_ns: u64 } }`.
   - `Event::Level(f64 /* peak dBFS */)`.
   - New `UserError` variants: `NoCamera`, `NoMicrophone`, `NoSuitableCameraFormat`, `RecordingFailed(String)`, `DeviceFallback { what: &'static str }` (a notice), `StopNotClean` (a notice).
   - Existing callers update: the harness sends `Stamp { host_ns: now_ns(), source_secs: None }`.
2. **Capture sources.**
   - `Bus::spawn`/`spawn_with_state` gain `capture: CaptureKind { Devices, Test }`.
   - The app passes `Devices`, and the harness passes `Test` with zero video delay.
   - `Input::Recorder(u64, RecorderMessage)` is a new input variant, never routed to `player.handle`. The bus drops messages whose generation isn't the live recorder's.
3. **Deadlines.** Replace `deadline: Option<Instant>` with two named fields, `skip_deadline` and `start_deadline`. The loop waits for the earlier one and dispatches whichever passed. Keep it that simple; don't build a timer wheel.
4. **State.**
   - `recording: RecState { Idle, Starting { pending, recorder, zoom }, Recording { pending, recorder, log } }`.
   - `pending = PendingClip { id: Uuid, source_index, start_source_seconds }`.
5. **Start** (R6):
   - Check the preconditions, each refusal an `Event::Error`: open, sources, none missing, current loaded (`player.holds(uri)`), recorder Idle.
   - Pause (`set_playing(false)`).
   - `start_source_seconds` = `skip.target()` mapped to source seconds via `locate`, else `current_secs()`.
   - Resolve devices from preferences. Fallback emits a `DeviceFallback` notice.
   - `Recorder::start` to `<folder>/recordings/<id>.mkv`. `recordings/` exists: `store::write` creates it, and a project opened from an existing folder has one; `create_dir_all` it anyway.
   - Arm `start_deadline = now + 5 s` and emit `Recording(Starting)`.
6. **The Starting state:**
   - `FirstVideo` → create `RecordingLog::new(t0, zoom, start)`, clear the deadline, emit `Recording(Recording { t0_ns })`.
   - `StopRecording`, `Error`, or the deadline passing → `abort`, delete the file, emit `Recording(Idle)`, and an error unless it was StopRecording.
7. **Stop:**
   - `log.finish()`, then `recorder.stop(5 s)`.
   - `open.project.add_recorded_clip(…)` with `created_at` from glib.
   - Save via the existing `project_changed()`, emit `Recording(Idle)`, and add `StopNotClean` if not clean.
   - A recorder `Error` while Recording runs the same stop, since the clip is kept, plus `RecordingFailed`.
8. **Shutdown:** if Recording, stop as above before dropping. If Starting, abort.
9. **The guard** (R6): at the top of `command()`, while not Idle, allow only `TogglePlay`, `Skip`, `SetVolume`, `Zoom`, `StopRecording`, `GlReady`, and handle `Shutdown` in `run`. Drop everything else with an `eprintln!`: the UI greys these out, so reaching here is a UI bug, not a user error. Starting additionally drops TogglePlay and Skip.
10. **Transport while Recording** (R10):
    - **Anchor:** `skip.target()` → `locate` → source secs, else `player.target_secs()`, else `at.source_secs`, else `current_secs()`. The anchor must be in the pending source; if `locate` gives a different index, clamp. It can't happen given the skip range, but debug-assert it.
    - `toggle_play` logs `play`/`pause` with `at.host_ns` and the anchor.
    - `skip` uses the range `offset(src)..=offset(src) + dur(src) − END_MARGIN` and logs `skip(at.host_ns, delta)`.
    - `Zoom` logs `log.zoom(host_ns, zoom)`; in Starting it only updates the stored zoom.
    - `end_of_stream` while Recording pauses without advancing and logs nothing.
11. **`SetDevices`** writes the preferences and saves (allowed only while Idle, by the guard).
12. **Harness tests** (`crates/video-coach-harness/tests/recording.rs`), per the spec's Testing → Harness list:
    - start/stop → clip fields and file;
    - `sort_index` after a gap, using `write_project` with a clip at `sort_index` 5;
    - stop during Starting → no clip, no file. Use a harness option for video delay (`CaptureKind::Test` with a delay) so Starting lasts long enough to hit.
    - TogglePlay during Starting is ignored;
    - a missing source refuses;
    - AddSource is refused while recording;
    - a skip clamped to the source, in a two-source project;
    - skip then an immediate pause anchors at the skip target;
    - shutdown while recording saves the clip (read `project.json` after).

    Add `Harness` helpers as needed (`wait_recording(status)`). The existing transport tests switch to the new `TogglePlay { at }` shape.

Commit: `feat(app): recording on the bus`.

## Task 5 — UI: recording controls, Clips list, Devices

In `crates/video-coach-app/ui/app.slint` and `src/main.rs`.

1. **Keys.**
   - **R** toggles: sends `StartRecording { zoom }` when the UI's status is Idle, and `StopRecording` otherwise.
   - **Esc** sends `StopRecording` when not Idle.
   - Both go through the root `FocusScope`, yield to the name `LineEdit` like the other shortcuts, and use `capture-key-pressed`.
2. **Stamps.** Space and skip keys and buttons capture `Stamp { host_ns: now_ns(), source_secs: position.query_position() }` in the callback, before sending. The `PositionHandle` is already on the UI thread.
3. **Zoom forwarding.** `set_zoom` also sends `Command::Zoom { host_ns: now_ns(), zoom }` when the status isn't Idle. Every change is sent; the log dedupes.
4. **The transport bar** (R11):
   - **Starting:** "Preparing recording…".
   - **Recording:** a red dot, "Recording", and the elapsed time. The elapsed time is `format_hms((now_ns() − t0) / 1e9)` in the existing 30 Hz tick; the text only changes once a second.
   - **Level bar:** a 90×6 rectangle whose width fraction is `clamp((peak_db + 60) / 60, 0, 1)` from `Event::Level`, with "Waiting for audio…" until the first `Level` of a recording.
   - A **Record/Stop** button with the tooltip "Record (R)" / "Stop recording (R or Esc)".
5. **The guard in the UI.** A `recording: bool` property (not Idle) disables:
   - the sidebar's Sources section (add, remove, move, relink);
   - Open Project and rename;
   - the scrubber;
   - the Devices button.
6. **Clips list.** A sidebar section below Sources: name and `format_hms(recording_duration)` rows ordered by `sort_index`, from `show_project`. No interaction yet.
7. **Devices popover.**
   - A transport-bar button opens a `PopupWindow`. Opening it calls `list_devices()` on the UI thread (a quick call; measure it, and if it's >100 ms do it on a thread and fill in the result).
   - It shows two lists with "System default" first, marking the project's preferences.
   - Choosing a device sends `SetDevices`.
8. **Errors and notices** use the existing error display. `DeviceFallback` and `StopNotClean` are phrased as notices.
9. **Manual run** on the laptop, with `XDG_CONFIG_HOME` in the scratchpad. **Opens the real camera and mic.**
   - Record about 5 s with a pause, a skip and a zoom.
   - Screenshot the Recording state and the Clips list.
   - Check the clip's events in `project.json`.
   - Delete the recording.

   A synthetic X key event (`xdotool key r`) can drive R/Esc if it is installed. Don't install anything; if it's missing, test R through the button's callback instead, and add R to the user's batched checklist.

Commit: `feat(app): recording controls, clips list and device picker`.

## Task 6 — Closeout

1. Update `CLAUDE.md`'s Rust port section with one paragraph on capture:
   - `v4l2src` + PipeWire mic;
   - the forced system clock;
   - recording time 0 = `base_time`;
   - the injected capture sources for tests.
2. **The batched hands-on checklist** for the user. Append to the Phase 2 list in the Phase 2 plan's Task 8 status, or keep a single list in this plan's Task 6 notes, whichever is shorter:
   - real R/Esc;
   - the webcam light only while recording;
   - lip sync looks right;
   - the level bar moves with speech;
   - the device choice persists;
   - the IR camera is absent;
   - QP tuning in a lit room;
   - `kill -9` mid-recording leaves a playable file.
3. Run the adversarial review on the shipped diff (the `adversarial-review` skill), apply, and backlog any deferrals.

## Deliberately not in this phase

- Clip selection, editing, delete, tags, undo: Phase 3.
- Strokes: Phase 6.
- Export and the PiP checkbox: Phases 5 and 8.
- A/V offset measurement and orphan cleanup: BACKLOG #37 and #38.
