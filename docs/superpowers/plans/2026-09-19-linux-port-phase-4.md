# Linux Port — Phase 4 Plan (Capture)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-4-design.md` (decisions cited as R1–R11)
**Status:** Reviewed. Simplification and correctness passes are applied. The correctness pass ran the recorder pipeline, the device monitor and the CI package check on the reference laptop.

**Goal:** everything on the spec's "Done when" list works on the reference laptop.

**Execution.** Same as Phase 2:
- Each task runs in a fresh subagent that is given this plan, the spec and `CLAUDE.md`, and no chat history.
- After each task the orchestrator runs the `verify` skill and commits that task on its own.
- CI must stay green at every commit, and **every task must build the whole workspace**, including the binary. `on_event` in `main.rs` matches `Event` exhaustively.

**Privacy.** Automated tests use `videotestsrc`/`audiotestsrc` only.
- A step that opens the real camera or microphone says so explicitly.
- Record to the scratchpad, and delete the media when done.
- Restore any V4L2 control you change.

**Known facts. Don't re-derive these.**
- **Pipeline timing.**
  - File time 0 = `base_time`: `matroskamux` writes running time as-is.
  - `base_time` is fixed when `set_state(PLAYING)` returns, but the pipeline doesn't *reach* PLAYING until every mux pad has data. In 1.24 `matroskamux` is CollectPads-based, so with no camera frame `set_state` returns `Async` and the pipeline stays PAUSED with PLAYING pending. **Never wait for PLAYING.**
  - `vah264lpenc` PTS carries +3600 s. Use running time.
  - Always `use_clock(SystemClock)`.
- **Elements.**
  - `vah264lpenc` and `vajpegdec` exist on the laptop (rank 0; name them).
  - `vah264lpenc`'s sink accepts **NV12 only**, so YUY2 won't link and raw input needs `videoconvert`.
  - `x264enc` is in plugins-ugly. **`h264parse` is in plugins-bad.** `opusenc` is in base; `matroskamux` and `level` are in good.
  - The `va` plugin registers no elements without `/dev/dri`, so CI gets x264.
- **Devices.**
  - PipeWire device properties include `node.name`, `api.v4l2.path`, `device.api`, `object.path` and `priority.session` (webcam 1000, IR 980).
  - There is **no default flag**.
  - The IR camera is GRAY8 only. The webcam lists 848×480 and 424×240, which are near 16:9 but not exact.
  - The first `DeviceMonitor` enumeration takes ~250 ms; later ones take ~36 ms.
- **`level`.** Its `peak` field is a `GValueArray`: read it as `glib::ValueArray`, then `.as_slice()`. Silence reads −350 to −700 dB.
- **Pause preroll.** PLAYING→PAUSED prerolls displayed+1. `FLUSH_STOP`/`STREAM_START` reach the appsink inside the GL bin, and a flushing seek while PLAYING does call `new_preroll`. The probe:
  ```rust
  let fresh = Arc::new(AtomicBool::new(true));
  appsink.static_pad("sink").unwrap().add_probe(
      gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::EVENT_FLUSH,
      { let fresh = fresh.clone(); move |_, info| {
          if let Some(gst::PadProbeData::Event(ev)) = &info.data {
              if matches!(ev.type_(), gst::EventType::FlushStop | gst::EventType::StreamStart) {
                  fresh.store(true, Ordering::SeqCst);
              }
          }
          gst::PadProbeReturn::Ok
      }});
  // new_sample: deliver, then fresh.store(false); new_preroll: deliver only if fresh.load()
  ```
- **Bus internals.**
  - The loop multiplexes `Input::{Cmd, Gst}` on one unbounded mpsc channel with one `deadline: Option<Instant>` (the skip debounce).
  - `Bus::loaded()` (`sources.rs`) is `player.holds(uri)` for `current`: true while it's loaded or loading.
  - `current_secs()` prefers `player.target_secs()`.
  - `SkipCoordinator::request_skip(delta, current, clip_duration)` clamps to `[0, clip_duration]`.
- **Call sites that change with the command shapes.**
  - `main.rs:195-196`: TogglePlay and Skip.
  - `harness/tests/transport.rs`.
  - `harness/tests/project_and_sources.rs:377, 389, 493, 528`.
- **Dates.** `created_at` = `glib::DateTime::now_utc()?.format("%Y-%m-%dT%H:%M:%SZ")`: whole seconds, matching the fixtures and Swift.

---

## Task 0 — Pause keeps the frame on screen (R10, Phase 2 fix)

1. **`crates/video-coach-media/src/player/sink.rs`:**
   - Add the `fresh` flag and probe from Known facts, on the **appsink's** sink pad.
   - `new_sample` delivers, then clears the flag.
   - `new_preroll` delivers only while it is set.
   - Rewrite the `FrameMailbox` doc, which currently says "Both `new_sample` and `new_preroll` fill it": a preroll is shown only when it is the first frame since a flush or a new stream (a seek or a load), because a pause's preroll is the *next* frame while the position stays on the displayed one.
2. **Player test** (`player/tests.rs`, system sink, fixture video): play about 1 s, pause, wait for the pause to settle. Then the mailbox is either empty (after a `take()` just before the pause) or holds a frame whose `[pts, pts+dur)` covers `query_position`. Confirm it fails without the fix by temporarily reverting. The existing seek tests cover seek prerolls.

Commit: `fix(media): pausing keeps the frame on screen`.

## Task 1 — Core: the recording log, recorded clips, skip range

No GStreamer. Use the `port-swift-module` skill. Read `apple/App/Recording/RecordingController.swift` and `apple/Tests/AppTests/RecordingZoomCaptureTests.swift` first.

1. **`crates/video-coach-core/src/recording.rs`** (R8):
   ```rust
   pub struct PendingClip { pub id: Uuid, pub source_index: usize, pub start_source_seconds: f64 }
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
   - `record_time = (host_ns as i128 − t0_ns as i128) as f64 / 1e9`, then `.max(last event time)`.
     - One clamp covers two cases. A `host_ns` captured just before t0 arrives while Active. The log starts with events at 0, so ≥ last also means ≥ 0. And it keeps the log sorted.
   - **Zoom:**
     - dedupe values equal to the last one;
     - if more than 100 ms have passed since the last zoom capture, first emit the previous value at `max(t − 0.001, last event time)`;
     - no throttle.
2. **`Project::add_recorded_clip(&mut self, pending: PendingClip, duration: f64, events: Vec<CommentaryEvent>, created_at: String) -> &Clip`** (R9):
   - the name is `"{}-{:02}:{:02}:{:02}"` from `source_index + 1` and floored `start_source_seconds`;
   - `sort_index` = max + 1, or 0 for the first clip;
   - `show_pip` comes from `preferences.pip_for_new_recordings`;
   - `recording_filename = "<id>.mkv"`.
3. **`SkipCoordinator`:**
   - `request_skip(delta, current, range: RangeInclusive<f64>)` replaces `clip_duration`. Keep the guard against an inverted or NaN upper bound, and its comment: `f64::clamp` panics on `min > max`.
   - Add `pub fn target(&self) -> Option<f64>`.
   - The bus call site becomes `0.0..=(total − END_MARGIN).max(0.0)`.
4. **Doc fixes:**
   - `event.rs`'s module doc: record time is "seconds since the recording's time 0, the capture pipeline's base time (Phase 4 R5)", not "since the first frame".
   - `Preferences.preferred_camera_id`: "PipeWire `node.name`". Drop `/dev/v4l/by-id`.
5. **Tests** (`tests/recording.rs`, plus additions to `project_format.rs` and `skip.rs`):
   - construction order;
   - caller times and anchors;
   - the requested skip delta;
   - zoom dedupe;
   - an anchor after 100 ms quiet, never before the last event;
   - a dense zoom stream kept whole;
   - a `host_ns` before t0 → 0;
   - the name (3725.9 s → `…-01:02:05`);
   - `sort_index` after a gap;
   - `show_pip` from preferences;
   - the skip range at both ends.

Commit: `feat(core): recording log, recorded-clip construction, skip range`.

## Task 2 — Media: devices and chain choice

Add `crates/video-coach-media/src/capture/{mod.rs,devices.rs}`.

1. **Pure functions,** tested without hardware:
   - `choose_camera_mode(&gst::Caps) -> Option<CameraMode>` (R3). `CameraMode { width, height, input: Input::{Mjpeg, Raw} }`:
     - the largest structure with `w·9 == h·16`, `w ≤ 1280` and 30/1 available;
     - `image/jpeg` → Mjpeg; `video/x-raw` → Raw, except `format=GRAY8`, which is never usable;
     - handle a fixed fraction, a `gst::List` and a `gst::FractionRange` for `framerate`.
   - `choose_encoder(has: impl Fn(&str) -> bool, input: Input) -> EncoderChain` (R4). `EncoderChain::{Va, Software}` has a method that builds `(head, tail)` elements:
     - Va + Mjpeg: `vajpegdec ! vah264lpenc`;
     - Va + Raw: `videoconvert ! vah264lpenc` (NV12 only);
     - Software: `jpegdec`/`videoconvert` then `x264enc speed-preset=ultrafast tune=zerolatency key-int-max=30`;
     - VA settings: `rate-control=cqp qpi=24 qpp=26 key-int-max=30`.

     Va requires `vah264lpenc`, plus `vajpegdec` for Mjpeg.
   - **Tests:** caps strings copied from `gst-device-monitor-1.0 Video/Source` on the laptop (listing only; this doesn't open the camera):
     - the webcam → 1280×720 Mjpeg;
     - the IR camera → None;
     - 848×480 is not 16:9;
     - a camera with no 16:9 mode → None;
     - a framerate list and a framerate range;
     - each encoder variant, including that Va + Raw has a converter.
2. **Enumeration:**
   - `pub fn list_devices() -> Devices { cameras: Vec<Camera>, mics: Vec<Mic> }`.
     - `Camera { node_name, label, v4l2_path, mode: CameraMode, priority: i32 }` lists only cameras with a mode, sorted by `priority.session` descending.
     - `Mic { node_name, label }`.
   - It starts a `DeviceMonitor` (`Video/Source`, `Audio/Source`), reads `devices()`, and stops it.
   - Only devices with a `node.name` property are kept.
3. **Resolution:**
   - `resolve_camera(&[Camera], preferred: Option<&str>) -> Option<(&Camera, bool /*fell back*/)>`: the preferred device, else the first, which has the highest priority.
   - Mics: a preferred `node.name` that is present → `target-object`. Otherwise no `target-object`, so PipeWire picks the default. Report whether it fell back.
4. **`pub fn now_ns() -> u64`** in `capture/mod.rs`: `gst::SystemClock::obtain().time()` in ns. It is the only clock for `host_ns` and t0 (R5).

Commit: `feat(media): capture device enumeration, format and encoder choice`.

## Task 3 — Media: the Recorder

Add `crates/video-coach-media/src/capture/recorder.rs` (R1, R4, R5, R6).

1. **API.**
   ```rust
   pub enum CaptureSources { Devices { camera: Camera, mic: Option<String> }, Test { video_delay: Duration } }
   pub enum RecorderMessage { FirstVideo, Level { peak_db: f64 }, Error(String) }
   pub struct StopOutcome { pub duration: f64, pub clean: bool }
   impl Recorder {
       /// Builds, sets PLAYING, reads t0 = base_time as soon as set_state returns
       /// (never waits for PLAYING: the mux holds preroll until the camera's first frame).
       /// On Err the caller deletes `path` (filesink has created it).
       pub fn start(sources: CaptureSources, path: &Path, generation: u64,
                    on_message: impl Fn(u64, RecorderMessage) + Send + Sync + 'static) -> Result<Recorder, String>;
       pub fn t0_ns(&self) -> u64;
       /// EOS, wait ≤ timeout for EOS/ERROR on the pipeline's own bus, NULL.
       pub fn stop(self, timeout: Duration) -> StopOutcome;
   }
   impl Drop for Recorder { /* NULL; used for aborts too */ }
   ```
2. **Construction.** Build elements in code with Task 2's chain.
   - **Test sources:** `videotestsrc is-live=true pattern=ball` at **320×180/30**, raw, and `audiotestsrc is-live=true wave=ticks`.
     - A small frame size keeps x264 cheap on CI, where tests run in parallel.
     - `video_delay` is a buffer probe on `videotestsrc`'s src pad that drops buffers with PTS < delay. A live `videotestsrc`'s PTS is its running time, so the first video lands at exactly the delay. There is no valve and no thread.
     - Test sources go through `choose_encoder` like production: VA on the laptop, x264 on CI.
   - **The queue after `opusenc`:** `max-size-time` 6 s, with buffers and bytes set to 0. The mux holds audio until video arrives, and the starting timeout is 5 s.
   - **CI:** add `gstreamer1.0-plugins-ugly gstreamer1.0-plugins-bad` to `.github/workflows/rust.yml`'s apt line.
3. **Settings and probes.**
   - `use_clock(SystemClock)`.
   - Set `matroskamux offset-to-zero=false` explicitly, with a comment pointing at R5.
   - `filesink buffer-mode=unbuffered`.
   - A buffer probe on each mux sink pad records `max(running_time(pts) + duration)` into an `AtomicU64 last_end`, using the pad's sticky SEGMENT.
   - The video pad's probe also sends `FirstVideo` once.
4. **Bus messages.**
   - A sync handler forwards `ERROR` (as `Error`) and `level` element messages (`Level`, with the max of `peak` over channels) through `on_message` with the generation.
   - It returns `BusSyncReply::Pass` for EOS and ERROR, so `stop()` can `timed_pop_filtered` them, and `Drop` for everything else.
   - `stop()` returns `duration = last_end` seconds, and `clean` if EOS came before the timeout. An ERROR already queued makes it return at once, unclean.
5. **Tests** (`crates/video-coach-media/tests/recorder.rs`, Test sources, a temp dir):
   - (a) Record 2 s and stop. The file has H.264 and Opus (Discoverer). `|duration − Discoverer| ≤ 1/30 s`, and `clean`.
   - (b) With a 0.5 s video delay: the first video PTS in the file (demux + probe) is 0.5 s ±40 ms, the first audio PTS is < 40 ms, `FirstVideo` arrived, and `start` returned in < 200 ms (it didn't wait for PLAYING).
   - (c) A `Level` message within 1 s, carrying the generation passed to `start`.
   - (d) Stop timeout: add a pad probe that drops EOS on the audio branch before the mux. `stop(1 s)` returns after about 1 s with `clean == false` and a duration > 0.
6. **Hardware check** (the laptop; **opens the real camera and mic** for about 3 s):
   - Record with `Devices` into the scratchpad.
   - Confirm the file plays and the elements in use (`vajpegdec`, `vah264lpenc`).
   - Confirm that an unsupported control in `extra-controls` only warns, by setting a bogus control name once.
   - Delete the file. Write the results into this plan's Task 3 notes.

Commit: `feat(media): commentary recorder`.

## Task 4 — Bus: recording

Put the state machine in `bus/recording.rs`. `bus/mod.rs` gets the variants and the loop changes. **This task also updates every call site** (main.rs and the harness), so the workspace builds.

1. **Commands.**
   - `TogglePlay { host_ns: u64, source_secs: Option<f64> }` and `Skip { delta, host_ns: u64 }`.
   - `StartRecording { zoom: Zoom }`, `StopRecording`, `Zoom { host_ns, zoom }`.
   - `SetDevices { camera: Option<String>, mic: Option<String> }`.
   - In `main.rs`, TogglePlay captures `now_ns()` and `position.query_position()` in the callback, and Skip captures `now_ns()`. `set_zoom` (the single choke point) **always** sends `Zoom`; the bus ignores it while Idle.
   - In the harness, add a `Harness::toggle_play()` / `skip(delta)` helper that fills in `now_ns()`.
2. **Events.**
   - `Event::Recording(RecordingStatus)`, where `RecordingStatus::{Idle, Starting, Recording { t0_ns }}`.
   - `Event::Level(f64)`.
   - `main.rs`'s `on_event` gets arms for both (store them; Task 5 draws them).
   - New `UserError` variants: `NoDevice { what: &'static str }`, `RecordingFailed(String)`, `DeviceFallback { what }` (a notice), `StopNotClean` (a notice).
3. **Capture.**
   - `Bus::spawn`/`spawn_with_state` take `capture: CaptureKind::{Devices, Test { video_delay: Duration }}`. The app passes `Devices`.
   - `Harness::new` passes `Test { video_delay: 0 }`; `Harness::with_capture(dir, CaptureKind)` is for delays.
   - The bus keeps a clone of its input `Sender` for the recorder's `on_message`, which sends `Input::Recorder(generation, msg)`. That input is never routed to `player.handle`.
   - The bus increments `generation` **before** each `Recorder::start` and drops messages that don't match.
4. **Deadlines.**
   - `skip_deadline` and `start_deadline` replace `deadline`.
   - The loop waits for the earlier of the two.
   - After **every** input (and on a timeout), it dispatches each deadline that is ≤ now.
5. **One helper for "where the player is heading"** (R6, R10): `heading(&self, ui_secs: Option<f64>) -> (usize, f64)`, returning source index and source seconds:
   - `skip.target()` → `project.locate(abs)`;
   - else `(current, player.target_secs())`;
   - else `(current, ui_secs)`;
   - else `(current, current_secs())`.

   Start calls it with `None`; the play/pause anchor passes the UI's position. While Active, the skip range keeps targets inside the pending source.
6. **State.** `recording: Option<Active>`, where `Active { pending: PendingClip, recorder: Recorder, log: RecordingLog, video_seen: bool }`.
7. **Start** (R6):
   - **Preconditions:** a project is open, it has sources, none is missing, and `loaded()`. A failed check is an `Event::Error`.
   - **Order:**
     1. `set_playing(false)`.
     2. `pending` from `heading(None)`.
     3. Resolve devices from preferences. Emit a `DeviceFallback` notice if one fell back, or `NoDevice` if there is no camera.
     4. `create_dir_all(recordings/)`.
     5. `generation += 1`, then `Recorder::start`. On `Err`: delete the file and emit `RecordingFailed`.
     6. `RecordingLog::new(t0, zoom, start)`.
     7. `start_deadline = now + 5 s`, and emit `Recording(Starting)`.
8. **Messages.**
   - `FirstVideo` → `video_seen = true`, clear `start_deadline`, emit `Recording(Recording { t0_ns })`.
   - `Level` → `Event::Level`.
   - `Error` → if `!video_seen`, abort; else stop (the clip is kept), plus `RecordingFailed`.
9. **Abort** (StopRecording, an error or `start_deadline` while `!video_seen`): drop the recorder, delete the file, emit `Recording(Idle)`.
10. **Stop** (`video_seen`):
    1. `log.finish()`.
    2. `recorder.stop(5 s)`.
    3. `add_recorded_clip(pending, duration, events, created_at)`.
    4. `project_changed()`, which saves.
    5. Emit `Recording(Idle)`, plus `StopNotClean` if not clean.
11. **Shutdown:** if Active, stop or abort as above before dropping.
12. **Guard:** at the top of `command()`, while Active, allow only TogglePlay, Skip, SetVolume, Zoom, StopRecording and GlReady. Shutdown is handled in `run`. Drop everything else with `eprintln!`: the UI greys these out, so reaching here is a UI bug.
13. **Transport while Active** (R10):
    - **TogglePlay:** run the existing logic, then log from the resulting `self.playing` (not from the command), with the anchor from `heading(source_secs)`.
    - **Skip:** the range is `offset(src)..=offset(src) + dur(src) − END_MARGIN`; log `skip(host_ns, delta)`.
    - **Zoom:** `log.zoom(host_ns, zoom)`.
    - **EOS:** pause without advancing, and log nothing.
14. **SetDevices** writes the preferences and saves.
15. **Harness tests** (`crates/video-coach-harness/tests/recording.rs`):
    - start → `Recording` → stop → a clip with the right `source_index`, `start_source_seconds`, duration ≈ elapsed, and events starting `[zoom, pause]`; the file exists;
    - stop while starting (`Test { video_delay: 2 s }`) → no clip, no file;
    - TogglePlay during the warm-up (same delay, then let video arrive) is logged as `play`;
    - a missing source refuses;
    - AddSource while Active is refused;
    - a skip clamped to the pending source in a two-source project;
    - skip then an immediate pause anchors at the skip target;
    - shutdown while Active saves the clip (read `project.json`);
    - `Level` events arrive.

Commit: `feat(app): recording on the bus`.

## Task 5 — UI: recording controls, Clips list, Devices

In `ui/app.slint` and `src/main.rs`.

1. **Keys.**
   - **R:** `StartRecording { zoom }` when the UI's status is Idle, else `StopRecording`.
   - **Esc:** `StopRecording` when not Idle.
   - Both go through the root `FocusScope`'s `capture-key-pressed`, and yield to the name `LineEdit` like the other shortcuts.
2. **The transport bar** (R11):
   - **Starting:** "Preparing recording…".
   - **Recording:** a red dot, "Recording", and `format_hms((now_ns() − t0) / 1e9)` in the existing 30 Hz tick.
   - **Level bar:** 90×6, width fraction `clamp((peak_db + 60) / 60, 0, 1)`, with "Waiting for audio…" until the first `Level` of the recording.
   - A **Record/Stop** button, with the tooltip "Record (R)" / "Stop recording (R or Esc)".
3. **The guard in the UI.** A `recording: bool` property (not Idle) disables:
   - the Sources section's add, remove, move and relink;
   - Open Project and rename;
   - the scrubber;
   - the Devices button.
4. **Clips list.** A sidebar section below Sources, with name and `format_hms(recording_duration)` rows ordered by `sort_index`, from `show_project`. Read-only.
5. **Devices popover.**
   - A transport-bar button opens a `PopupWindow`.
   - It runs `list_devices()` on a short-lived thread (the first call takes ~250 ms) and fills in the lists through `upgrade_in_event_loop`.
   - Each list has "System default" first, and the project's choice is marked.
   - Choosing a device sends `SetDevices`.
6. **Notices.** `DeviceFallback` and `StopNotClean` use the existing error display, worded as notices.
7. **Manual run** on the laptop, with `XDG_CONFIG_HOME` in the scratchpad. **Opens the real camera and mic.**
   - Record about 5 s with a pause, a skip and a zoom.
   - Screenshot the Recording state and the Clips list.
   - Check the clip's events in `project.json`.
   - Delete the recording.

   Use `xdotool key r` only if it's already installed. Otherwise drive the button and add R/Esc to the user's checklist.

Commit: `feat(app): recording controls, clips list and device picker`.

## Task 6 — Closeout

1. `CLAUDE.md`, in the Rust port section, gets one paragraph on capture:
   - `v4l2src` + PipeWire mic;
   - the forced system clock;
   - recording time 0 = `base_time`, and never waiting for PLAYING;
   - injected test sources.
2. **The batched hands-on checklist** for the user, kept in this plan's Task 6 notes:
   - real R/Esc;
   - the webcam light only while recording;
   - lip sync looks right;
   - the level bar moves with speech;
   - the device choice persists;
   - the IR camera is absent;
   - QP tuning in a lit room;
   - `kill -9` mid-recording leaves a playable file.
3. Run the adversarial review on the shipped diff, apply, and backlog any deferrals.

## Deliberately not in this phase

- Clip selection, editing, delete, tags, undo: Phase 3.
- Strokes: Phase 6.
- Export and the PiP checkbox: Phases 5 and 8.
- A/V offset and orphan cleanup: BACKLOG #37 and #38.
