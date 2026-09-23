# Avatar recording: a picture that talks instead of a webcam

**Date:** 2026-09-22
**Status:** Draft, awaiting adversarial review. The user decided, 2026-09-22: one image per project; a smoothed pulse with the voice; **the corner pulses live while recording too** (the recorder's `level` element already posts peaks at 10 Hz, so it is nearly free) as well as in previews and exports; the camera is never opened in avatar mode; and the avatar rests slightly smaller than the webcam inset, growing to exactly the inset on a loud frame (Q1's first option). Open questions at the end; the plan proceeds on each one's default unless the user says otherwise.
**Builds on:** Phase 4 (the recorder, the self-view, the recording lifecycle), Phase 7 (the composite, the overlay, the PiP pad, `layout.rs`), Phase 8 (the export graph, the audio mix), Phase 10 (the 16 kHz `Reader`), Match Vision F (the format-bump rules)
**Evidence:**

- The code, cited by `path:line`.
- `docs/superpowers/spikes/2026-09-19-compositing-throughput.md`, cited as **the compositing spike**.
- `docs/superpowers/specs/2026-09-19-linux-port-phase-{4,5,8}-design.md`.

Labels, as in the match-vision spec: **[measured]** was measured on this machine or in this repository. **[cited]** comes from a named source. **[estimate]** is arithmetic, and every estimate here is a number the plan replaces with a measurement.

---

## Goal

A coach with no webcam — or who does not want to be on camera — records commentary as usual, and the corner of every preview and export carries a picture of their choosing instead of their face. The picture pulses with their voice, so the inset reads as "someone is talking" rather than as a sticker.

Concretely:

1. A project holds one avatar image, copied into the project folder when it is picked.
2. With avatar mode on, a take opens the microphone and **never the camera**. It works on a machine with no camera at all, and on one with no H.264 encoder.
3. In preview and export the avatar scales up on louder commentary and settles back, smoothed so it does not flicker per syllable.
4. While recording, the corner shows the still image. Nothing reacts live.

## Scope and product rules

These are the user's decisions (2026-09-22). This spec follows them and does not reopen them.

- **One avatar image per project**, copied into the project folder when picked.
- **The motion is a pulse with the voice**: scale up on louder speech, settle back, smoothed.
- **It reacts in previews and exports only**, computed from the recorded commentary audio. The corner shows the still image while recording.
- **Avatar mode never opens the camera.** Microphone only.

Two things are explicitly out of scope: any live reaction while recording, and any change to how clips, drawings, zoom, the scoreboard or the match clock work. An avatar take is an ordinary take whose inset comes from somewhere else.

---

## Decisions

### A. The image

**A1. It is one file in the project folder, named in `project.json`.**

- The file is `<project>/avatar.<ext>`, beside `project.json` and `recordings/` (`crates/video-coach-core/src/project.rs:1-4`).
- `Project.avatar: Option<String>` holds its **file name**, not a path. `None` means no image has been picked. Storing the name rather than a fixed constant keeps the original extension, which is what lets the file be opened by a file manager and by Slint without a sniff.
- **Why a copy and not a reference:** a project already refuses to lose data to a moved file for its own writable assets. Sources are referenced and have a relink flow (`SourceRef.relative_path`, `project.rs:96-109`); the avatar has no relink flow and does not deserve one. Copying makes the project folder self-contained, which is also what makes it safe to move between machines.
- **Formats:** whatever GStreamer's installed decoders read. PNG and JPEG are the two the tests pin, because `pngdec` and `jpegdec` are already runtime dependencies (`jpegdec` is in the self-view chain, `crates/video-coach-media/src/capture/self_view.rs:135-140`), so nothing joins `packaging/build-deps.txt` or the smoke test's element list.
- **Transparency is kept.** The overlay layer is premultiplied RGBA and the mixer pad is already told so (`crates/video-coach-media/src/overlay.rs:26-31`), so a cut-out PNG floats over the picture with no plate behind it.

**A2. Picking one is: decode, copy, save.**

1. The coach picks a file. The app decodes it (through the same loader export uses, A3). A file that will not decode is **refused before anything is copied**, with the decoder's message.
2. A file over `AVATAR_MAX_BYTES = 16 MiB` is refused with "that image is too large" before it is decoded. It is a cheap pre-check, not a correctness guarantee (see Risks).
3. The bytes are copied verbatim to `<project>/avatar.<ext>` through a temporary file and a rename in the same directory, exactly as `store::write` writes `project.json` (`crates/video-coach-core/src/store.rs:200-204`). A copy cut short leaves no avatar.
4. Any previous `avatar.*` with a different extension is deleted, so the folder never holds two.
5. `Project.avatar` is set and the project is saved.

Replacing the image replaces it for the whole project, including clips already recorded. That follows from "one avatar image per project": the render reads the project's current image, never a per-clip copy (see B2's rejected alternative).

**A3. It is decoded once per job, in media, through GStreamer.**

`video-coach-media/src/composite/avatar.rs` gains:

```rust
pub(super) struct Avatar {
    /// Pre-scaled to the run's largest drawn size, so the per-frame blit is
    /// never an upscale and barely a downscale.
    image: tiny_skia::Pixmap,
    /// The image's own display aspect, which is what sizes the inset.
    aspect: f64,
    /// `layout::pip_rect(out_w, out_h, aspect)`: the inset's footprint.
    rect: layout::Rect,
}
pub(super) fn open(path: &Path, out_w: f64, out_h: f64) -> Result<Avatar, String>;
```

- The pipeline is `filesrc ! decodebin3 ! videoconvert ! video/x-raw,format=RGBA,pixel-aspect-ratio=1/1 ! appsink`, with a bounded wait, one sample pulled and the pipeline set to NULL. `decodebin3`, never `decodebin` (CLAUDE.md).
- The sample is copied into a `Pixmap` at native size, then scaled **once** into a `Pixmap` of `pip_rect(out_w, out_h, aspect)` rounded up. Everything after that reads the small one.
- **Why not add an image crate to media:** media is the GStreamer crate and the decoders are already there. `video-coach-core` of course gets nothing: it declares no media dependency, not even an image crate (CLAUDE.md, `crates/video-coach-core/Cargo.toml:9-13`).
- **The aspect is the image's.** `pip_rect(out_w, out_h, cam_aspect)` takes a display aspect and sizes the inset from it (`crates/video-coach-core/src/layout.rs:76-86`); an avatar hands it the image's aspect where a webcam hands it the camera's. So a tall portrait avatar gets a tall inset in the same 22%-of-width column, and `layout.rs` needs no new constant.

**A4. A missing or unreadable image costs the inset, never the run.**

- **At export or preview time:** the load fails, one line goes to stderr, and the entry draws no inset — the same rule and the same tone as `Pip::open`'s, whose comment is "A missing PiP is a smaller loss than a failed export of an hour of video" (`crates/video-coach-media/src/composite/export.rs:447-449`). The export produces a file.
- **At record time:** avatar mode with no image **refuses the start** (C6). That is the one place it is worth refusing, because the alternative is a take the coach discovers is faceless afterwards.
- **In the UI:** the Devices popover shows the picked image and says so when the file has gone, so the coach finds out before a take rather than after an export.

### B. A project setting, a per-clip fact

**B1. The mode is a preference, so it applies to the next take and not to past ones.**

`Preferences` gains `avatar_for_new_recordings: bool`, default `false` (`crates/video-coach-core/src/project.rs:55-86`).

- `Preferences` carries `#[serde(default)]` on the **container**, filled from its hand-written `Default` impl, so a new `bool` there is safe: the field-level-default hazard the module comment warns about (`project.rs:5-14`) does not arise. `pip_for_new_recordings` is the existing precedent, and this field sits beside it.
- It is stored with the project, like the camera and microphone preferences, not machine-wide: which project is a talking-head project is a property of the project.

**B2. A clip records which inset it was made with.**

`Clip` gains:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Inset {
    #[default]
    Camera,
    Avatar,
}

// on Clip, beside `show_pip`:
#[serde(default)]
pub inset: Inset,
```

- **So an existing webcam clip keeps its webcam and a new take gets the avatar.** A project holds both kinds, and each entry of one export renders the kind its clip was recorded with.
- **A field-level default on an enum, not an `Option`.** Match Vision F2 states the rule as "an `Option` or a `Vec`", with the reason "`None` and empty are exactly what an older file means". The reason is the rule: the default must be what an older file means. `Inset::default()` is `Camera`, and every v7–v9 clip was recorded with a camera, so it is. Spelling it `Option<Inset>` would give two spellings of one state (`None` and `Some(Camera)`) and every reader a match arm to get wrong. `Resolution` and `Quality` are the same shape already (`project.rs:31-47`).
- **Why not derive it from the file** — an avatar recording has no video track, and `probe` already answers `ProbeError::NoVideo` for one (`crates/video-coach-media/src/probe.rs:30-31`), so "no video track" would need no format change at all. Rejected: a webcam take whose camera died mid-recording, or whose file was truncated, also has no usable video, and deriving would then draw the coach's avatar over a take they recorded on camera. That is a *wrong* picture, not a missing one. And nothing would record what the coach chose.
- **Why not store the image name per clip:** it would let a project's clips diverge onto different images, which contradicts "one avatar image per project", and it would put a missing-file case on every clip instead of one.
- `add_recorded_clip` sets `inset` from `preferences.avatar_for_new_recordings`, exactly as it already sets `show_pip` from `preferences.pip_for_new_recordings` (`project.rs:429-431`).

**B3. `show_pip` keeps its meaning and its command.**

`show_pip` already means "draw the inset for this clip". An avatar clip with `show_pip` off draws no inset, the same as a webcam clip with it off. `ClipEdit::ShowPip`, its undo, the inspector checkbox (`crates/video-coach-app/ui/app.slint:468, 2909, 2925`) and `Pip::open`'s check (`export.rs:460-462`) are all unchanged. Only the checkbox's **label** follows the clip's `inset`: "Show webcam" or "Show avatar" (G3).

**B4. What a camera-less take's file looks like.** Matroska with one Opus audio track and nothing else. It is a valid recording: `recording_filename` still points at it, `recording_duration` still comes from `StopOutcome.duration`, the event log is unchanged, and transcription reads it unchanged (I5). What it does *not* have is a video track, which is exactly why `Pip::open` must not be asked to open it (E3).

### C. Recording without a camera

**C1. The recorder builds the audio branch only.**

`CaptureSources` (`crates/video-coach-media/src/capture/recorder.rs:37-45`) makes the video side optional rather than growing two new variants:

```rust
pub enum CaptureSources {
    /// `camera: None` records sound alone: no camera is opened.
    Devices { camera: Option<Camera>, mic: Option<String> },
    /// `video: None` records sound alone; `Some(delay)` puts the first video
    /// buffer at `delay`, as a camera warming up.
    Test { video: Option<Duration> },
}
```

`build` (`recorder.rs:218-370`) then skips the whole video branch when there is no camera: no `v4l2src`, no `capsfilter`, no `choose_encoder`, no `h264parse`, and **no `video_%u` pad requested on the mux**. Everything else is byte-for-byte the pipeline it is today.

Consequences worth stating:

- **An avatar take needs no H.264 encoder.** `choose_encoder` never runs, so a machine with neither VA-API nor `x264enc` can still record commentary.
- **`AUDIO_QUEUE_NS` (6 s of encoded audio, `recorder.rs:31-34`) exists because the mux holds audio until the first video frame.** With no video pad the mux holds nothing, so the queue never fills. It is left in place: it costs nothing and removing it would mean two audio branches.
- **`level` stays.** The live meter in the recording bar keeps working (`app.slint:3272-3292`), because it is the microphone's, not the camera's.

**C2. Time 0 and the clock are untouched.**

- The recorder still forces `SystemClock` (`recorder.rs:226-228`), so `host_ns` from `now_ns()` and the file's timeline stay on CLOCK_MONOTONIC.
- `matroskamux` still has `offset-to-zero=false` (`recorder.rs:231-236`), so the file's time 0 is the pipeline's `base_time`.
- `t0_ns` is still read the moment `set_state(PLAYING)` returns, and is still never waited on (`recorder.rs:130-139`). Phase 4 R5's reason for not waiting was the mux holding preroll for the camera's first frame; with no video pad `set_state` may now return `Success` instead of `Async`, which the existing `match (started, t0)` already accepts. The `debug_assert!(t0.nseconds() > 0)` still holds.

So `RecordingLog::new(t0, zoom, start)` and every `record_time` it computes mean exactly what they mean today, and a drawing made during an avatar take replays at the same instant it would on a camera take.

**C3. The first-buffer gate moves to the audio pad.**

Today `track_end` is asked to send `FirstVideo` from the **video** mux pad (`recorder.rs:343-360`), and the bus uses it for the only two things Phase 4 R6 lists: the "Preparing…" label, and whether a stop keeps the clip or aborts it (`crates/video-coach-app/src/bus/recording.rs:64-66, 154-156, 167-171`).

- With no camera, the flag is set from the **audio** pad instead. Its meaning is unchanged: "the first buffer reached the muxer, so there is a file worth keeping".
- `RecorderMessage::FirstVideo` is therefore renamed **`FirstBuffer`**, and `Active.video_seen` to `media_seen`. It is a rename in one crate and its two consumers, and it is worth doing: leaving a variant called `FirstVideo` to mean "the first audio buffer" is the kind of lie that costs an afternoon later.
- `START_TIMEOUT` (5 s, `bus/recording.rs:27-28`) applies unchanged, with the message adjusted: "no sound from the microphone within 5 seconds" in avatar mode, "no video from the camera…" otherwise (`bus/recording.rs:190-196`). A microphone that never delivers is exactly as fatal as a camera that never does.

**C4. There is no self-view pipeline.**

`self_view::start` is fed from a pad probe on the recorder's camera capsfilter (`recorder.rs:363-368`, `self_view.rs:47, 74-80`). With no camera there is no such pad and no self-view pipeline; `Recorder.self_view` is already an `Option` (`recorder.rs:77-78`), so `None` is the shape the rest of the code already handles, including `Drop` (`recorder.rs:204-214`). What the corner shows instead is a UI question, answered in G2.

**C5. Start and Stop guarantee what they guarantee today.**

Phase 4 R6's contract is unchanged in every clause:

- Start still pauses the player, still captures `pending` from where the player is heading, still creates the log at `t0`, and still refuses before anything changes if a precondition fails.
- A stop before the first buffer still aborts: pipeline to NULL, file deleted, no clip.
- A stop after it still keeps the clip, even if finalization is unclean — `finish_recording` closes the log first, so no event outlasts the file (`bus/recording.rs:298-320`).
- `StopOutcome.duration` still comes from `last_end`, now tracked on the one pad there is. On a camera take the audio pad already contributes to that `fetch_max`, so the number's definition does not change.
- Recording still preempts transcription (`bus/recording.rs:140-146`) and still blocks export and preview.

**C6. What the bus refuses.**

`capture_sources` (`bus/recording.rs:274-296`) branches on `preferences.avatar_for_new_recordings`:

- **Avatar mode, no image picked:** refuse with `CantRecord("pick an avatar image in Devices first")`. Nothing is created, per R6 step 1.
- **Avatar mode, image picked:** resolve the microphone only. `resolve_camera` is not called, so `UserError::NoCamera` cannot be raised and a machine with no camera records fine. A microphone fallback still emits its `DeviceFallback` notice.
- **Camera mode, no camera:** still `UserError::NoCamera`, with the message extended to name the way out: "no camera found — pick an avatar image in Devices to record without one". The mode is never switched silently.

### D. The pulse

**D1. The signal is the recorded commentary, read once per entry.**

- Media opens the entry's recording with `Reader::start(recording, PULSE_RATE, 1, cancel)` and `Reader::rest(cancel)` — the same reader transcription uses at the same rate (`crates/video-coach-media/src/composite/audio.rs:289-294, 464-468`; `crates/video-coach-media/src/transcribe.rs:31, 629-630`). `PULSE_RATE = 16_000`, mono.
- A file with no audio track comes back as `Ok(None)` and the pulse is flat (`composite/audio.rs:336-341`).
- **It is a second read of the file, not the export mixer's.** The mixer streams forward at 48 kHz stereo and seeks per region (`composite/audio.rs:74-92`); the pulse needs the whole thing up front, at a different rate, before the first frame is pushed. Audio decode runs about 90× realtime (Phase 8 E3), so one extra pass over a one-minute take is milliseconds.
- **Only the commentary drives it.** The game's audio does not, so a clip where the coach says nothing over a roaring crowd shows a still avatar. That is the correct answer: the inset is the coach.

**D2. The derivation is one pure function in core.**

`video-coach-core/src/avatar.rs`:

```rust
pub const PULSE_RATE: u32 = 16_000;
pub const PULSE_MAX: f64 = 1.10;
pub const PULSE_FLOOR_DB: f64 = -45.0;
pub const PULSE_CEILING_DB: f64 = -12.0;
pub const PULSE_ATTACK: f64 = 0.06;   // seconds
pub const PULSE_RELEASE: f64 = 0.22;  // seconds

/// One scale per output frame of an entry, each in `1.0..=PULSE_MAX`.
/// `frames` is the entry's frame count; samples past the end read as silence.
pub fn pulse(samples_mono: &[f32], rate: u32, frames: usize) -> Vec<f64>;

/// The rect the avatar is drawn in: `pip` scaled about its centre by
/// `scale / PULSE_MAX`, so `PULSE_MAX` is exactly `pip` and nothing is larger.
pub fn avatar_rect(pip: Rect, scale: f64) -> Rect;
```

For output frame `n` of the entry:

1. **Window:** the samples in `[n / 30, (n + 1) / 30)` seconds of the recording — 533 samples at 16 kHz. Past the end of the file the window is empty.
2. **Level:** `rms = sqrt(mean(x²))`, `db = 20·log10(max(rms, 1e-6))`, `level = clamp((db − FLOOR) / (CEILING − FLOOR), 0, 1)`. An empty window is `level = 0`.
3. **Smoothing:** a one-pole filter across frames with separate time constants,
   `s(n) = s(n−1) + a·(level(n) − s(n−1))`, `a = 1 − exp(−dt / τ)`, `dt = 1/30`,
   `τ = PULSE_ATTACK` while rising and `PULSE_RELEASE` while falling, `s(−1) = 0`.
4. **Scale:** `1 + (PULSE_MAX − 1)·s(n)`.

**Why an output frame is the window.** The pulse is consumed one value per output frame, so a window that is not the frame is a second sampling grid to keep aligned with the first. One frame of 16 kHz audio is 33 ms, which is already several pitch periods of a voice, so the RMS is stable before any smoothing is applied.

**Why record time is the right clock.** `PlanEntry::record_time(frame)` is `(frame − start_frame) / 30` (`crates/video-coach-core/src/plan.rs:67-80`), and the commentary region runs from the recording's own zero for the whole entry, freezes included (`crates/video-coach-core/src/audio.rs:118-131`). So the pulse table is indexed by `frame − entry.start_frame` and needs no interpolation, and during a freeze the picture holds while the avatar keeps talking — which is what the sound is doing.

**D3. Why it cannot jitter, and why it cannot clip.**

- **Jitter:** a syllable rate of 4–6 Hz is a 170–250 ms period. `PULSE_RELEASE = 0.22 s` is 6.6 frames, longer than the gap between syllables, so the image does not drop back between them; `PULSE_ATTACK = 0.06 s` is 1.8 frames, fast enough that the rise lands on the word. The 33 ms RMS window removes the waveform's own zero crossings before the filter sees them. [estimate — Q1 is where the user's eyes settle it.]
- **Clipping:** `s ∈ [0, 1]` by construction, so `scale ∈ [1, PULSE_MAX]`, so `avatar_rect` returns a rect between `pip / PULSE_MAX` and `pip` — **never larger than the webcam's inset**. The inset's footprint is therefore identical to a camera clip's, `layout.rs` gains no constant, `pip_rect_over_picture` (`layout.rs:102-114`) is unchanged, and the live corner can use the same placement. The cost is that the resting avatar is 91% of the inset rather than 100%; the alternative (rest at 100%, grow past it) would put a loud frame 17 px into the 24 px gap above the text bar at 1080p, which is a layout change dressed as a pulse.

**D4. Why not the live meter, which already exists.**

The recorder already posts `RecorderMessage::Level { peak_db }` every 100 ms from a `level` element (`recorder.rs:29-30, 119-124`), the bus forwards it as `Event::Level` (`bus/recording.rs:172`), and the app draws a meter with it (`crates/video-coach-app/src/main.rs:1748-1754`, `app.slint:3272-3292`). Storing that series on the clip would need no audio decode at all.

Rejected:

- 10 Hz is a visible 100 ms staircase on a 30 fps pulse, and interpolating it invents the shape the filter is supposed to find.
- It is a peak, not an RMS: one consonant burst would throw it.
- It would be a stored array of ~600 floats per clip in `project.json`, which is data the file can already derive exactly.
- Messages can be dropped, so the series would have holes that the file does not.
- Clips recorded before this feature would have none.

Deriving from the file is exact, reproducible, identical in preview and export, and stores nothing.

**D5. Where it is computed.** In media, on the job's own thread, in `composite/avatar.rs::pulse_table(recording, frames, cancel) -> Vec<f64>`, which calls the reader and then core's `pulse`. Export computes one table per avatar entry, at the moment the entry is laid out (beside `Pip::open`, `export.rs:309`); preview computes one at job start. Core never sees a file and media never decides a number.

### E. Rendering: the overlay draws it, not the GL PiP pad

**E1. The decision.** The avatar is drawn into the existing tiny-skia overlay layer, under the strokes and the bar, at the same point in `OverlayRenderer::draw` where the layer order is already decided (`overlay.rs:313-322`).

| | Overlay (chosen) | GL PiP pad |
|---|---|---|
| Per-frame CPU at 1080p | **1.5–2.6 ms** [estimate from the compositing spike's 2.58 ms webcam blit, scaled by the pre-scaled source] | ~0 |
| Preview and export parity | Free: `overlay.rs::draw` is the one shared drawer | Two code paths — export pushes PiP buffers from its pump, preview plays the recording's video pad natively (`composite/preview.rs:8-10, 562-566`), and an avatar has no video pad, so preview needs a third appsrc and a per-frame push |
| The pulse | A rect passed per frame into a function already called per frame | A **per-frame** pad rect, where `Layout` is per **entry** today (`composite/mod.rs:349-355, 392-396`). Needs a per-frame scale threaded into the PTS-keyed probe — the probe discipline itself is non-negotiable, since a rect set from the pushing thread lands up to 4 frames early [measured, Phase 8 E2] |
| Caps risk | None | A new GL texture on a pad that also carries the 1×1 filler. The filler is GL **precisely because** a system-memory buffer on that pad breaks `glupload` mid-run (`export.rs:556-567`, reproduced) |
| New code | One draw step, one `OverlayFrame` field | An upload, a per-frame scale table in `Schedule`, a preview appsrc and branch |

**Why this does not break CLAUDE.md's pixel split.** The rule is that GStreamer owns every **full-frame** pixel operation on the GPU and Rust owns the vector overlay layer, and it exists because full-frame resampling in Rust costs 37.56 ms a frame against 3.62 ms for vectors [measured, the compositing spike, lines 16 and 35-39]. The avatar is an inset of about 133k pixels, 6% of a 1080p frame, and it is the only raster element in a layer that is otherwise vector. It is the same class of thing as the text bar's plate, not the same class as the base image.

**E2. What is drawn.**

- `OverlayFrame` gains `avatar: Option<(&tiny_skia::Pixmap, LayoutRect)>` — the pre-scaled image and the rect `avatar_rect(pip_rect(out_w, out_h, aspect), scale)` for this frame. `None` for every frame that has no avatar, which is every webcam clip, every clip with `show_pip` off, and every reel and whole-match entry.
- The draw is one `draw_pixmap` with a scale transform and bilinear filtering, in **output space**, like the bar and the scoreboard. It is not mapped through the zoom and it is not in the picture rect: the inset is chrome, which is the rule `layout.rs` states for the PiP (`layout.rs:16-19`).
- It goes **after** the highlights and **before** the bar's background, so the layer order stays what it is today: the coach's pen over the inset, the bar's words over the pen, the scoreboard over everything (`overlay.rs:305-322`). A stroke drawn over the inset lands over it, exactly as it does over a webcam.
- Nothing is drawn behind it. A transparent PNG floats (A1); a rectangular photo fills its rect.

**E3. The GL PiP pad still runs, and still takes the filler.**

`Pip::open` returns `Pip::filler()` for an avatar clip, checked on `clip.inset` **before** the probe (`export.rs:451-475`). Two reasons:

- The probe on a video-less Matroska returns `ProbeError::NoVideo` (`probe.rs:30-31`) and `refuse` prints a line per entry. Checking `inset` first keeps stderr honest: "no picture-in-picture" is not what happened.
- The pad is then fed the 1×1 GL filler every frame, which is the path `show_pip: false` already takes, and which is why an export mixing an avatar clip and a webcam clip cannot hit the caps-feature trap: the pad carries GL memory from the first frame to the last, and only its size changes (`export.rs:556-567`).

**E4. The cost is measured before it ships.** The plan adds a media benchmark (the `measure-media` skill) of the overlay with and without an avatar at 1080p and at the preview's 720p, on a real `Exporter` run with the output duration asserted — the measurement trap Phase 8 records. **The gate:** 1080p export stays at or above 1.3× realtime (it is ~1.6× today [measured, Phase 8]) and the preview holds 30 fps against a scanning control in the same session (CLAUDE.md's UI budget rule). If it misses, the fallback is the GL pad as costed in E1's table, and the plan says so rather than discovering it.

### F. Preview and export parity

They share one composite (CLAUDE.md, `composite/`), and the avatar keeps that true:

- **Both** resolve the avatar image from `open.folder.join(project.avatar)` on the bus, as a snapshot on the job, like `highlights` and `scoreboard` already are (`export.rs:90-96`, `preview.rs:100-110`). `ExportJob` and `PreviewJob` each gain `avatar: Option<PathBuf>`.
- **Both** call `avatar::open` once for the run, at the run's own output size — 1920×1080 for export, 1280×720 for preview (`preview.rs:73-76`). The ratios in `layout.rs` do the rest, which is what that module exists for.
- **Both** compute the pulse table from the entry's recording with the same function, and index it by `frame − entry.start_frame`.
- **Both** hand the same `OverlayFrame.avatar` to the same `OverlayRenderer::render`.
- **Preview must not request the PiP pad for an avatar clip.** Today it requests it on `job.clip.show_pip` alone (`preview.rs:562-566, 894`) and links the recording's video pad to it; an avatar recording has no video pad, and **an unfed mixer pad stalls everything** [measured, Phase 8 E2]. The condition becomes `show_pip && clip.inset == Inset::Camera`. This is the one line where the two tails differ, and it is the same decision `Pip::open` makes in export.

The consequence is the property that matters: what the coach checks in the preview is what the file gets, at a different size.

### G. The UI

**G1. Turning it on and picking the image** live in the Devices popover, beside the Camera and Microphone lists (`app.slint:3337-3364`), because that is already "where a take's inputs are chosen".

- A third section, **Inset**, with two radio-style rows: "Camera" and "Avatar image".
- Under it, the picked image as a thumbnail with its file name, a **Choose…** button that opens a file dialog, and a **Remove** button. With no image picked, "Avatar image" shows "Pick an image" and the Record button's refusal explains itself (C6).
- With the image file missing, the thumbnail is replaced by "avatar.png is missing".
- Both the mode and the image are per project, so both are saved with the project.

**G2. The corner while recording** is the still image.

The existing self-view `Image` is reused as it stands (`app.slint:1659-1667, 2643-2651`): it is already placed by `place_self_view(content, aspect)` → `layout::pip_rect_over_picture` (`main.rs:1176-1195`), already sits over the picture and under the drawings, and already takes no input.

- In avatar mode the app sets its `source` to the project's avatar once, at the start of the take, with `slint::Image::load_from_path`.
- `self_view_shown` is driven by `SELF_VIEW_QUIET` today — one second without a frame hides it, because a frozen camera picture would lie about the camera (`main.rs:59-63, 2184-2189`). A still image is not a lie about anything, so avatar mode sets a flag that keeps it shown for the whole take, and the quiet timer applies to camera takes only.
- It does **not** pulse. There is no live reaction anywhere (Scope).
- It is at rest size (`pip_rect / PULSE_MAX`), not full size, so the corner matches what the export will mostly look like.

**G3. The inspector** changes by one word. The existing "Show webcam" checkbox (`app.slint:337, 468, 2909, 2925`) reads "Show avatar" when the selected clip's `inset` is `Avatar`. Same property, same command, same undo (B3). A clip whose project has lost its avatar image also shows a one-line note under it, so the coach is told before they export rather than after.

**G4. Nothing else moves.** The recording bar, the level meter, the elapsed clock, the drawing tools, zoom, tagging, the transcript row and the export sheet are untouched.

### H. Format: v10

Following Match Vision F, and the guard as it stands (`store.rs:21-26, 124-135`).

| Version | Change |
|---|---|
| v10 | `Project.avatar: Option<String>`; `Preferences.avatar_for_new_recordings: bool`; `Clip.inset: Inset` |

- **`MIN_READABLE_FORMAT_VERSION` stays 7.** Every change here is additive and reads as it stands: a v7–v9 file has no `avatar` key (`None`), no `avatarForNewRecordings` inside `preferences` (filled from `Preferences::default()`), and no `inset` on a clip (`Inset::Camera`).
- **An older build refuses a v10 project** with `StoreError::TooNew { found: 10, supported: 9 }`. That is the whole reason to bump (F3): serde ignores unknown keys, so a v9 build would otherwise open a v10 project, drop the avatar and the per-clip inset, and save them away.
- **The first save at v10 keeps a way back.** `store::write` already copies `project.json` to `project.json.v9` before raising the version, once, never overwriting (`store.rs:175-193`). Restoring it reopens the project in the older build, losing the changes since.
- **The graceful floor, if a v10 project ever reaches an older build another way:** an avatar recording has no video track, `Pip::open`'s probe says `NoVideo`, and the entry gets the transparent filler. No inset, no crash, no wrong face.
- **Source edits need no new remapping.** `avatar` is one file for the project and `inset` is a clip's own field, so `move_source`, `remove_source` and `source_is_referenced` are unchanged (`project.rs:272-357`).

### I. Edge cases

**I1. Avatar mode on, no image set.** The recording is refused before anything changes (C6). Export and preview cannot meet this case, because a clip only exists if a take ran.

**I2. An avatar clip's recording played by an older build.** It can't open the project (H), and if it could, the audio-only recording gets the filler and no inset.

**I3. An avatar clip beside a webcam clip in one compilation.** The PiP pad carries GL for the webcam entries and the 1×1 GL filler for the avatar entries — a size change on a pad whose caps feature never changes, which is the case the filler was made GL for (`export.rs:556-567`). The overlay carries the avatar on the avatar entries only. Both are per-entry decisions taken where every other per-entry decision is taken (`export.rs:304-320`). A media test pins this ordering in both directions.

**I4. A silent take.** Every window's RMS is at or below the floor, `s` stays 0, every scale is 1.0, and the avatar sits still at 91% of the inset. Correct and quiet.

**I5. A very loud take.** `level` saturates at 1 for the loud stretches, the scale reaches `PULSE_MAX`, and the drawn rect is exactly `pip_rect`. It cannot go further (D3). A take recorded into clipping pulses at full amplitude nearly all the time, which reads as loud, not as broken.

**I6. Transcription is unaffected — verified.** `transcribe.rs` reads a recording through the same `Reader` at 16 kHz mono (`transcribe.rs:31, 629-630`), and `Reader::start` finds its audio stream from `decodebin3`'s stream collection and selects it explicitly (`composite/audio.rs:333-380`). It never requires a video stream; the comment at `composite/audio.rs:356-368` is about a file with **no audio**, which an avatar recording is not. An audio-only Matroska is if anything the easier case: there is no unselected video stream to post `not-linked`. The model, the queue, the preemption rules and the transcript row are all unchanged. A media test pins it on the audio-only fixture.

**I7. A huge image.** Refused over 16 MiB at pick time (A2). Below that, it is decoded at native size once per job, which is a transient RGBA frame; see Risks.

**I8. The avatar image is replaced mid-project.** Every avatar clip renders with the new image from the next preview or export. That follows from "one avatar image per project". A running export keeps the image it opened, because the job carries a snapshot (F).

**I9. Reel and whole-match entries.** `clip_id: None`, so no clip, no `inset`, no commentary and no inset at all — the existing filler path (Match Vision R4, W2). The avatar is a property of a commentary take, and those entries have none.

---

## Crate responsibilities

| Crate | Contents |
|---|---|
| `video-coach-core` | `Inset`, `Project.avatar`, `Preferences.avatar_for_new_recordings`, the v10 bump. `avatar.rs`: `pulse` (samples in, one scale per output frame out), `avatar_rect`, and the constants. **No new dependency** — the audit still lists exactly `serde`, `serde_json`, `thiserror`, `uuid`. |
| `video-coach-media` | The camera-less recorder branch and `CaptureSources`' optional video. `composite/avatar.rs`: decoding the image to a pre-scaled `Pixmap`, and the pulse table via the existing `Reader`. The avatar draw step in `overlay.rs`. `Pip::open`'s `inset` check; preview's PiP-pad condition. A still-image fixture. |
| `video-coach-app` | Bus: avatar mode in `capture_sources`, the refusals, picking and copying the image, `avatar` on both jobs. UI: the Devices popover's Inset section, the still corner during a take, the inspector's label. |
| `video-coach-harness` | An avatar take end to end: record with test sources, get a clip, export it. |

Core touches no pixel and no file. Media hands it a slice of `f32` and a `Rect`; everything that decides the motion is tested on CI with synthetic input.

## Testing

- **Core:**
  - `pulse`: silence is all 1.0; a full-scale tone saturates at `PULSE_MAX`; a step from silence rises within two frames and decays over about seven; an empty slice and a `frames` longer than the audio both give 1.0 tails; **every output is within `[1, PULSE_MAX]`** as a property over random input.
  - `avatar_rect`: equals `pip` at `PULSE_MAX`, is centred on `pip` and strictly inside it below that, is monotone in `scale`, and **never exceeds `pip`** as a property over random scales.
  - Format: a v9 project loads with `avatar: None`, `avatar_for_new_recordings: false` and every clip `Inset::Camera`; a v10 file round-trips; v11 is `TooNew`; v6 is still `LegacyProject`; a v9 file gets its `project.json.v9` backup on the first save.
  - `add_recorded_clip` takes `inset` from the preference, as it takes `show_pip`.
- **Media** (generated fixtures only — no real camera, microphone or network, ever):
  - `fixtures::still_image(dir, w, h, format)` writes a PNG and a JPEG with `videotestsrc num-buffers=1 ! pngenc|jpegenc ! filesink`. `avatar::open` reads both, keeps a non-square aspect, and errors on a non-image and on a missing file.
  - The recorder with `CaptureSources::Test { video: None }` writes a playable Matroska with an audio track and **no** video track; `t0_ns > 0`; `StopOutcome.duration` matches the audio; `self_view_pipeline()` is `None`; `FirstBuffer` arrives.
  - Export of a one-entry avatar compilation: the inset's pixels land inside `pip_rect` on a loud frame and inside a strictly smaller rect on a silent frame, read back with `fixtures::decode_rgb`. Properties, not golden images, as in Phase 9.
  - Export of avatar-then-webcam **and** webcam-then-avatar: both produce the asserted frame count and do not stall (I3).
  - An avatar clip whose image file is gone exports with no inset and no failure.
  - Preview of an avatar clip runs and does not stall (the unrequested PiP pad).
  - `read_all` on `fixtures::audio_only` returns samples — transcription on a video-less recording (I6).
  - The overlay benchmark and its gate (E4).
- **Harness:**
  - Record in avatar mode with `CaptureKind::Test`, get a clip with `inset == Avatar`, export it end to end.
  - Recording in avatar mode with no image is refused and leaves no file and no clip.
- **Manual (batched, the user's eyes):**
  - Pick a real avatar, record a take, watch the preview and the export. The one thing only the user settles is whether the pulse reads as talking: `PULSE_MAX`, the attack and the release (Q1).
  - A transparent PNG avatar over a bright pitch.

## Risks

1. **The overlay blit costs more than estimated.** The compositing spike measured a 640×480 webcam blit at 2.58 ms; ours starts pre-scaled, so it should be well under that, but 1.5–2.6 ms on a 3.2 ms overlay is a real fraction. E4 is the gate and the GL pad is the costed fallback.
2. **The pulse constants are a guess.** They are named `const`s in core with no stored copy, so retuning them changes every render and nothing in any file. Q1.
3. **A very large image decodes large before it is scaled.** The 16 MiB file cap is on the compressed bytes, not the pixels: a pathological 16 MiB JPEG could decode to a few hundred MB transiently. A real avatar is a few MB. Bounding the decode by pixels is in Deferred.
4. **The `FirstVideo` → `FirstBuffer` rename touches the recording lifecycle**, which is the code path where a mistake costs a coach their commentary. It is mechanical and the lifecycle tests are the proof; it goes in its own task, before anything else.

## Deferred

- **A live pulse while recording.** The signal is already there at 10 Hz (`Event::Level`), so it is cheap. The user's decision is that the corner is still; revisit only on request.
- **Switching an existing clip between camera and avatar.** The design makes it nearly free — one `ClipEdit::Inset` variant, one checkbox, one undo case — because the avatar is drawn from the project's image and the recording's audio, and a webcam clip has both. Not in v1 because a clip should say what it was recorded with. Q5.
- **More than one avatar per project**, or a per-clip avatar. Contradicts the user's decision; revisit only if a project ever has two commentators.
- **Bounding the image decode by pixels** rather than by file bytes (Risk 3).
- **A background plate, a border or a rounded mask for the avatar.** The image is drawn as it is. Q3.
- **Other motions** (a bob, a tilt, a mouth swap, two-frame talking). The user asked for a pulse.
- **The avatar in the corner while scanning**, outside a recording. Q4.
- **Recording with a camera *and* an avatar**, picking per take at the Record button rather than in Devices. The preference already makes this a two-click change of mode; a per-take control would be a second place to look.

---

## Open questions for the user

Each has a default, and **the plan proceeds on it unless the user says otherwise.**

- **Q1. Is the pulse the right size and speed?** The default is 10% growth (`PULSE_MAX = 1.10`), a 60 ms attack and a 220 ms release, over −45 to −12 dBFS. Too small and it reads as a wobble; too big and it reads as a cartoon.
  **Default:** those numbers, tuned once by eye on the user's first real take. They are `const`s in core and store nothing, so retuning them costs a rebuild and no file change.
- **Q2. Does "avatar mode" belong to the project or to the machine?** It is the project's today, beside the camera and microphone preferences, so one project can be a talking-head project and another not.
  **Default:** the project's.
- **Q3. Should a cut-out avatar get a plate behind it?** A transparent PNG currently floats directly over the picture, with no rectangle and no border.
  **Default:** no plate. Add one only if a cut-out is hard to read over a bright pitch.
- **Q4. Should the avatar show in the corner while scanning, outside a recording?** Today the corner is the recording's self-view and is empty otherwise.
  **Default:** no. The corner means "a take is running".
- **Q5. Should a clip recorded on camera be switchable to the avatar afterwards (and back)?** It is nearly free to add (Deferred), but it means an exported clip can show a face that was never in that take, or hide one that was.
  **Default:** no for v1. Say the word and it is one field, one checkbox and one undo case.
