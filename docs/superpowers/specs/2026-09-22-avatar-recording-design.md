# Avatar recording: a picture that talks instead of a webcam

**Date:** 2026-09-22
**Status:** Reviewed (simplify + correctness applied). The user decided, 2026-09-22: one image per project; a smoothed pulse with the voice; **it pulses live in the corner while recording *and* is rendered from the recorded audio in previews and exports**; the camera is never opened in avatar mode; and the avatar rests slightly smaller than the webcam inset, reaching exactly the inset at its loudest. Open questions at the end; the plan proceeds on each one's default unless the user says otherwise.
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

1. A project holds one avatar image, copied into the project folder when it is picked. **The image is the mode**: a project with one records commentary without a camera; a project without one records on camera.
2. An avatar take opens the microphone and **never the camera**. It works on a machine with no camera at all, and on one with no H.264 encoder.
3. In preview and export the avatar scales up on louder commentary and settles back, smoothed so it does not flicker per syllable.
4. **While recording, the corner pulses too**, from the recorder's live meter, on the same curve and against the same thresholds. It is a second estimator of one quantity (D1), not a second feature.

## Scope and product rules

These are the user's decisions (2026-09-22). This spec follows them and does not reopen them.

- **One avatar image per project**, copied into the project folder when picked, and that image's presence *is* avatar mode.
- **The motion is a pulse with the voice**: scale up on louder speech, settle back, smoothed.
- **It reacts everywhere the coach can see the inset**: live in the corner while recording, and in previews and exports rendered from the recorded commentary.
- **Avatar mode never opens the camera.** Microphone only.

One thing is explicitly out of scope: any change to how clips, drawings, zoom, the scoreboard or the match clock work. An avatar take is an ordinary take whose inset comes from somewhere else.

---

## Decisions

### A. The image

**A1. It is one file in the project folder, named in `project.json`.**

- The file is `<project>/avatar.<ext>`, beside `project.json` and `recordings/` (`crates/video-coach-core/src/project.rs:1-4`).
- `Project.avatar: Option<String>` holds its **file name**, not a path. `None` means no image has been picked, which is also what "record on camera" means (B1). Storing the name rather than a fixed constant keeps the original extension, which is what lets the file be opened by a file manager and named back to the decoder without a sniff.
- **Why a copy and not a reference:** a project already refuses to lose data to a moved file for its own writable assets. Sources are referenced and have a relink flow (`SourceRef.relative_path`, `project.rs:96-109`); the avatar has no relink flow and does not deserve one. Copying makes the project folder self-contained, which is also what makes it safe to move between machines.
- **Formats: PNG and JPEG, and nothing else.** Not "whatever GStreamer happens to have": the set has to be one the pick-time check, the render and the corner all agree on, and `pngdec` and `jpegdec` are already runtime dependencies (`jpegdec` is in the self-view chain, `crates/video-coach-media/src/capture/self_view.rs:135-140`), so nothing joins `packaging/build-deps.txt` or the smoke test's element list. The pick is refused by **what decoded**, not by the extension: a `.png` that is really a TIFF is refused because the decode says so.
- **Transparency is kept.** The overlay layer is premultiplied RGBA and the mixer pad is already told so (`crates/video-coach-media/src/overlay.rs:26-31`), so a cut-out PNG floats over the picture with no plate behind it. See A3 on what that means for the copy into the pixmap.

**A2. Picking one is: decode, copy, save. Removing one is: clear, delete.**

**Choose…** decodes, copies and saves:

1. The coach picks a file. The app decodes it through the **same loader the render uses** (A3). A file that will not decode is **refused before anything is copied**, with the decoder's message. This is the whole validation: a decode that produces pixels is proof the render will produce them too.
2. The bytes are copied verbatim to `<project>/avatar.<ext>` through a temporary file and a rename in the same directory, exactly as `store::write` writes `project.json` (`crates/video-coach-core/src/store.rs:200-204`). A copy cut short leaves no avatar.
3. **Replacing one deletes exactly the file `Project.avatar` names**, before the new name is stored — never a `avatar.*` glob over the directory. The project knows the old file's name; globbing a folder the coach can also put files in is a way to delete something else.
4. `Project.avatar` is set and the project is saved.

**Remove** does the reverse, in one step: `Project.avatar` is cleared, the copy in the project folder is deleted, and the project is saved. **The coach's original file is untouched** — what is deleted is the project's own copy. The project is then a camera project again (B1).

**No byte cap.** An earlier draft refused a file over 16 MiB before decoding. It prevented nothing: decode-then-copy already rejects junk, and the pathological case a cap was aimed at — a small file that decodes enormous — is precisely the case a byte cap cannot see. Bounding the *decode* by pixels is in Deferred; the cap was a number pretending to be that bound.

Replacing the image replaces it for the whole project, including clips already recorded. That follows from "one avatar image per project": the render reads the project's current image, never a per-clip copy (see B2's rejected alternative).

**A3. It is decoded once per job, in media, through GStreamer.**

`video-coach-media/src/composite/avatar.rs` gains:

```rust
pub(super) struct Avatar {
    /// Pre-scaled to the run's largest drawn size, so the per-frame blit is
    /// never an upscale and barely a downscale. **Premultiplied** (see below).
    image: tiny_skia::Pixmap,
    /// `layout::pip_rect(out_w, out_h, <the image's display aspect>)`: the
    /// inset's footprint at its loudest. The aspect is not stored separately
    /// — this rect already carries it.
    rect: layout::Rect,
}
pub(super) fn open(path: &Path, out_w: f64, out_h: f64) -> Result<Avatar, String>;
```

And, because the app needs the same pixels at pick time and for the live corner (G2), one public seam on the crate:

```rust
/// One still frame's straight-alpha RGBA, at its own size. The one decoder
/// for avatar images: the pick validates with it, the corner draws with it,
/// and `avatar::open` scales its output into the pixmap.
pub fn decode_still(path: &Path) -> Result<Still, String>;   // { w, h, rgba: Vec<u8> }
```

- The pipeline is `filesrc ! decodebin3 ! videoflip video-direction=auto ! videoconvert ! video/x-raw,format=RGBA,pixel-aspect-ratio=1/1 ! appsink`, with a bounded wait, **one sample pulled** and the pipeline set to NULL. `decodebin3`, never `decodebin` (CLAUDE.md).
  - **`videoflip video-direction=auto`** reads the `image-orientation` tag, which is where a phone's EXIF rotation ends up. Without it a portrait photo taken on a phone draws sideways. Note that `probe.rs` *refuses* rotated video for sources (`ProbeError::Rotated`, `crates/video-coach-media/src/probe.rs:32-33`); an avatar is a still, there is no timeline to worry about, and rotating it is right.
  - **One sample pulled** is also the rule for a multi-frame file: an animated PNG, or a file that decodes to several frames, yields its **first** frame and the pipeline stops. No animation, and no error either.
- **The copy into the pixmap premultiplies.** tiny-skia stores premultiplied pixels; GStreamer's `RGBA` means straight alpha (`overlay.rs:26-31` says exactly this about the layer). A memcpy would leave a cut-out PNG's soft edges too bright and haloed — every partly transparent pixel drawn at its full colour. The copy multiplies each of R, G and B by A/255. Nothing else in `overlay.rs` demultiplies, and nothing here should either: the mixer pad is already configured for premultiplied.
- The sample is copied into a `Pixmap` at native size, then scaled **once** into a `Pixmap` of `pip_rect(out_w, out_h, aspect)` rounded up. Everything after that reads the small one.
- **Why not add an image crate to media:** media is the GStreamer crate and the decoders are already there. `video-coach-core` of course gets nothing: it declares no media dependency, not even an image crate (CLAUDE.md, `crates/video-coach-core/Cargo.toml:9-13`).
- **The aspect is the image's.** `pip_rect(out_w, out_h, cam_aspect)` takes a display aspect and sizes the inset from it (`crates/video-coach-core/src/layout.rs:76-86`); an avatar hands it the image's aspect where a webcam hands it the camera's. So a tall portrait avatar gets a tall inset in the same 22%-of-width column, and `layout.rs` needs no new constant.

**A4. A missing or unreadable image costs the inset, never the run.**

- **At export or preview time:** the load fails, one line goes to stderr, and the entry draws no inset — the same rule and the same tone as `Pip::open`'s, whose comment is "A missing PiP is a smaller loss than a failed export of an hour of video" (`crates/video-coach-media/src/composite/export.rs:447-449`). The export produces a file.
- **At record time:** nothing to refuse. `Project.avatar` is the mode, so "avatar mode with no image" is not a state that exists (B1). A project whose avatar *file* has gone under it still records — it is an avatar take with a missing picture, which is the export-time case above, and the Devices popover has already said so.
- **In the UI:** the Devices popover shows the picked image, and says "avatar.png is missing" when the file has gone, so the coach finds out before a take rather than after an export.

**A5. The avatar is drawn as a circle** (the user, 2026-09-22: "the image would be like my gravatar"). A square portrait is the normal case, so:

- The image is **fitted** inside the inset keeping its own shape — a square shows whole, nothing is stretched — and then masked to a circle inscribed in that fitted box.
- The mask is built once, with the pre-scaled pixmap, not per frame: it is the same `tiny_skia::Mask` machinery the overlay already memoizes for the picture rect.
- The pulse scales the circle, so it breathes around its own centre.
- A transparent cut-out still works; it is simply masked too, which costs it nothing in the middle of the frame.

### B. A project setting, a per-clip fact

**B1. The image *is* the mode. There is no separate flag.**

`Project.avatar: Option<String>`:

- **`Some(file)`** — takes record commentary only, no camera is opened, and the inset is that picture.
- **`None`** — takes record on camera, exactly as they do today.

An earlier draft added `Preferences.avatar_for_new_recordings: bool` beside `pip_for_new_recordings`. It is deleted, and with it: one field from the v10 bump, the "mode on, no image" state, the refusal that state needed (old C6), the edge case that documented it (old I1), and two of the four UI controls — the Inset section is now just the thumbnail, **Choose…** and **Remove**. Two spellings of one fact is one spelling too many: a `bool` and an `Option` that must agree is a bug waiting for a half-finished pick.

- It is stored with the project, like the camera and microphone preferences, not machine-wide: which project is a talking-head project is a property of the project (Q2).

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

// on Clip, beside `show_pip` (`project.rs:149`):
#[serde(default)]
pub inset: Inset,
```

- **So an existing webcam clip keeps its webcam and a new take gets the avatar.** A project holds both kinds, and each entry of one export renders the kind its clip was recorded with.
- **A field-level default on an enum, not an `Option`.** Match Vision F2 states the rule as "an `Option` or a `Vec`", with the reason "`None` and empty are exactly what an older file means". The reason is the rule: the default must be what an older file means. `Inset::default()` is `Camera`, and every v7–v9 clip was recorded with a camera, so it is. Spelling it `Option<Inset>` would give two spellings of one state (`None` and `Some(Camera)`) and every reader a match arm to get wrong. `Resolution` and `Quality` are the same shape already (`project.rs:30-47`).
  **The module comment in `project.rs` is amended to say so** (one line, in this work): a `#[derive(Default)]` enum whose default is what an older file means qualifies alongside `Option` and `Vec`. The hazard the comment is really about is `f64` and `bool`, whose `Default` is `0.0` and `false` and means nothing. Amending it is cheaper than the next reader re-litigating this.
- **Why not derive it from the file** — an avatar recording has no video track, and `probe` already answers `ProbeError::NoVideo` for one (`probe.rs:30-31`), so "no video track" would need no format change at all. Rejected: a webcam take whose camera died mid-recording, or whose file was truncated, also has no usable video, and deriving would then draw the coach's avatar over a take they recorded on camera. That is a *wrong* picture, not a missing one. And nothing would record what the coach chose.
- **Why not store the image name per clip:** it would let a project's clips diverge onto different images, which contradicts "one avatar image per project", and it would put a missing-file case on every clip instead of one.
- `add_recorded_clip` sets `inset` from `self.avatar.is_some()`, one line beside the one that sets `show_pip` from `preferences.pip_for_new_recordings` (`project.rs:430`).

**B3. `show_pip` keeps its meaning and its command, and one predicate reads both fields.**

`show_pip` already means "draw the inset for this clip". An avatar clip with `show_pip` off draws no inset, the same as a webcam clip with it off. `ClipEdit::ShowPip`, its undo, the inspector checkbox (`crates/video-coach-app/ui/app.slint:466-468, 2909, 2925`) and its command are all unchanged. Only the checkbox's **label** follows the clip's `inset` (G3).

The product of `show_pip` and `inset` is interpreted in **one place**, `core::project`:

```rust
impl Clip {
    /// The webcam PiP pad carries this clip's recording.
    pub fn shows_camera_pip(&self) -> bool { self.show_pip && self.inset == Inset::Camera }
    /// The overlay draws the project's avatar for this clip.
    pub fn shows_avatar(&self) -> bool { self.show_pip && self.inset == Inset::Avatar }
}
```

Two functions, not one, because they are the two halves of one decision and each has a caller that wants it positively; written together they cannot drift. `shows_camera_pip()` replaces a bare `show_pip` read at **four** sites:

| Site | Today |
|---|---|
| `composite/export.rs:460` | `if !clip.show_pip { return Pip::filler() }` |
| `composite/preview.rs:564` | whether the launch string contains the PiP branch at all |
| `composite/preview.rs:637` | `place_pip(&mix_pad("sink_1"))` |
| `composite/preview.rs:894` | `job.clip.show_pip.then(…)`, which decides whether `decodebin3`'s video pad is linked at all |

An earlier draft called this "the one line where the two tails differ". It is three lines in preview alone, and they must agree: the pad is requested in the launch string, placed from the caps, and linked from `decodebin3`'s pad-added. Miss one and an avatar clip either stalls on an unfed pad or links a video pad that does not exist.

**B4. What a camera-less take's file looks like.** Matroska with one Opus audio track and nothing else. It is a valid recording: `recording_filename` still points at it, `recording_duration` still comes from `StopOutcome.duration`, the event log is unchanged, and transcription reads it unchanged (I5). What it does *not* have is a video track, which is exactly why `Pip::open` must not be asked to open it (E3).

### C. Recording without a camera

**C1. The recorder builds the audio branch only.**

`CaptureSources` (`crates/video-coach-media/src/capture/recorder.rs:38-45`) makes the video side optional in **both** arms rather than growing two new variants:

```rust
pub enum CaptureSources {
    /// `camera: None` records sound alone: no camera is opened.
    Devices { camera: Option<Camera>, mic: Option<String> },
    /// `video: None` records sound alone; `Some(delay)` puts the first video
    /// buffer at `delay`, as a camera warming up.
    Test { video: Option<Duration> },
}
```

`build` (`recorder.rs:219-370`) then skips the whole video branch when there is no camera: no `v4l2src`, no `capsfilter`, no `choose_encoder`, no `h264parse`, and **no `video_%u` pad requested on the mux** (`recorder.rs:342-360`). Everything else is byte-for-byte the pipeline it is today.

Consequences worth stating:

- **An avatar take needs no H.264 encoder.** `choose_encoder` never runs, so a machine with neither VA-API nor `x264enc` can still record commentary.
- **`AUDIO_QUEUE_NS` (6 s of encoded audio, `recorder.rs:31-34`) exists because the mux holds audio until the first video frame.** With no video pad the mux holds nothing, so the queue never fills. It is left in place: it costs nothing and removing it would mean two audio branches.
- **`level` stays**, and now has a second reader (D1). The live meter in the recording bar keeps working, because it is the microphone's, not the camera's.

**The `Test` arm is not an afterthought.** `capture_sources` returns early for `CaptureKind::Test` **before** any device resolution (`crates/video-coach-app/src/bus/recording.rs:275-277`), so an avatar branch written only into the `Devices` path would leave every harness test recording with video however the project was set up — and the tests below would pass while proving nothing. The early return becomes:

```rust
if let CaptureKind::Test { video_delay } = self.capture {
    // Avatar mode is the project's, not the device path's: a test take must
    // record the same shape of file the coach's would.
    let video = self.avatar_mode().then_some(None).unwrap_or(Some(video_delay));
    return Ok(CaptureSources::Test { video });
}
```

where `avatar_mode()` is `open.project.avatar.is_some()`.

**C2. Time 0 and the clock are untouched.**

- The recorder still forces `SystemClock` (`recorder.rs:226-228`), so `host_ns` from `now_ns()` and the file's timeline stay on CLOCK_MONOTONIC.
- `matroskamux` still has `offset-to-zero=false` (`recorder.rs:231-236`), so the file's time 0 is the pipeline's `base_time`.
- `t0_ns` is still read the moment `set_state(PLAYING)` returns, and is still never waited on (`recorder.rs:130-139`). Phase 4 R5's reason for not waiting was the mux holding preroll for the camera's first frame; with no video pad the mux prerolls on the mic, and `set_state` returns **`NoPreroll`** — the live-source answer — rather than `Async`. (Not `Success`: an earlier draft said so and was wrong. The conclusion is unchanged either way, because the existing `match (started, t0)` accepts `Ok(_)`, and `NoPreroll` is `Ok`.) The `debug_assert!(t0.nseconds() > 0)` still holds.

So `RecordingLog::new(t0, zoom, start)` and every `record_time` it computes mean exactly what they mean today, and a drawing made during an avatar take replays at the same instant it would on a camera take.

**C3. The first-buffer gate moves to the audio pad.**

Today `track_end` is asked to send `FirstVideo` from the **video** mux pad (`recorder.rs:342-360`), and the bus uses it for the only two things Phase 4 R6 lists: the "Preparing…" label, and whether a stop keeps the clip or aborts it (`bus/recording.rs:65, 154, 167-171`).

- With no camera, the flag is set from the **audio** pad instead. Its meaning is unchanged: "the first buffer reached the muxer, so there is a file worth keeping".
- `RecorderMessage::FirstVideo` is therefore renamed **`FirstBuffer`**, and `Active.video_seen` to `media_seen`. Leaving a variant called `FirstVideo` to mean "the first audio buffer" is the kind of lie that costs an afternoon later. It is a compiler-checked rename across one crate and its two consumers, done in whichever task first touches the recorder — it needs no ceremony of its own.
- `START_TIMEOUT` (5 s, `bus/recording.rs:28`) applies unchanged, with the message adjusted: "no sound from the microphone within 5 seconds" in avatar mode, "no video from the camera…" otherwise (`bus/recording.rs:190-196`). A microphone that never delivers is exactly as fatal as a camera that never does.

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

`capture_sources` (`bus/recording.rs:274-296`) branches on `open.project.avatar.is_some()`:

- **Avatar project:** resolve the microphone only. `resolve_camera` is not called, so `UserError::NoCamera` cannot be raised and a machine with no camera records fine. A microphone fallback still emits its `DeviceFallback` notice.
- **Camera project, no camera:** still `UserError::NoCamera`, with the message extended to name the way out: "no camera found — pick an avatar image in Devices to record without one". The mode is never switched silently.

That is the whole list. The refusal an earlier draft needed ("avatar mode, no image") is gone with the flag (B1).

### D. The pulse

**D1. One quantity, two estimators.**

The avatar's size is a function of **how loud the coach is right now**, on a 0–1 scale, smoothed. That quantity is estimated two ways, and the spec's job is to keep the two saying the same thing:

| | **Rendered** (preview, export) | **Live** (the corner while recording) |
|---|---|---|
| Source | the recording, decoded | the recorder's `level` element |
| Measure | RMS over the output frame | the element's **`rms`** field |
| Rate | 30 Hz (`dt = 1/30`) | 10 Hz (`dt = 0.1`, `LEVEL_INTERVAL_NS`, `recorder.rs:29-30`) |
| Thresholds | `PULSE_FLOOR_DB` … `PULSE_CEILING_DB` | the same |
| Smoothing | `core::avatar::smooth`, attack/release | **the same function**, at its own `dt` |
| Exact? | yes — reproducible, identical in preview and export | no — an estimate of the same curve |

The live one is not exact and does not need to be: it tells the coach their voice is reaching the microphone. The rendered one is what ships in the file, and it is the one the file's pixels are derived from every time it is rendered.

**The live estimator reads `rms`, not `peak`.** `level_peak` (`recorder.rs:418-430`) currently pulls the `peak` array out of the element's message and throws the `rms` array away. Peak through the same thresholds would be wrong in a visible way: **speech has a crest factor of 10–14 dB** [cited, the standard figure for voice], so a `peak` that reads −18 dBFS on the same breath whose RMS is −30 dBFS would sit near the ceiling live while the export was barely off the floor. The corner would peg and the file would not, and the coach would trust the wrong one. So:

- `RecorderMessage::Level` carries **both**: `{ peak_db, rms_db }`, and `level_peak` becomes `level_dbs`, returning the max over channels of each array. The meter in the recording bar keeps drawing `peak_db` — a meter should show peaks — and the avatar corner uses `rms_db`.
- The dB → 0–1 mapping is core's, not the app's: `avatar::level_from_db(db)`, the same clamp the rendered path applies to its own RMS.

**D2. The signal for the rendered path is the recorded commentary, read once per entry.**

- Media opens the entry's recording with `Reader::start(recording, PULSE_RATE, 1, cancel)` and `Reader::rest(cancel)` — the same reader transcription uses at the same rate (`crates/video-coach-media/src/composite/audio.rs:289, 468`; `crates/video-coach-media/src/transcribe.rs:629`). `PULSE_RATE = 16_000`, mono.
- A file with no audio track comes back as `Ok(None)` and the pulse is flat.
- **It is a second read of the file, not the export mixer's.** The mixer streams forward at 48 kHz stereo and seeks per region (`composite/audio.rs:74-92`); the pulse needs the whole thing up front, at a different rate, before the first frame is pushed. Audio decode runs about 90× realtime (Phase 8 E3), so one extra pass over a one-minute take is milliseconds.
- **Only the commentary drives it.** The game's audio does not, so a clip where the coach says nothing over a roaring crowd shows a still avatar. That is the correct answer: the inset is the coach.

**D3. The derivation is pure functions in core.**

`video-coach-core/src/avatar.rs`:

```rust
pub const PULSE_RATE: u32 = 16_000;
/// The loudest inset is exactly `pip_rect`; the resting one is this much
/// smaller. 1.10 means the avatar grows 10% from rest to full.
pub const PULSE_GROWTH: f64 = 1.10;
pub const PULSE_FLOOR_DB: f64 = -45.0;
pub const PULSE_CEILING_DB: f64 = -12.0;
pub const PULSE_ATTACK: f64 = 0.06;   // seconds
pub const PULSE_RELEASE: f64 = 0.22;  // seconds

/// `db` mapped into `0..=1` against the floor and the ceiling.
pub fn level_from_db(db: f64) -> f64;

/// One step of the one-pole filter: `tau` is the attack while rising and the
/// release while falling. Both estimators call this, at their own `dt`.
pub fn smooth(previous: f64, target: f64, dt: f64) -> f64;

/// One **level** per output frame of an entry, each in `0.0..=1.0`, already
/// smoothed. `frames` is the entry's frame count; samples past the end read
/// as silence.
pub fn pulse(samples_mono: &[f32], rate: u32, frames: usize) -> Vec<f64>;

/// Where the avatar is drawn: `pip` at `level == 1.0`, and `pip` scaled about
/// its centre by `1.0 / PULSE_GROWTH` at `level == 0.0`, lerped between.
pub fn avatar_rect(pip: Rect, level: f64) -> Rect;
```

**Everything outside core carries a level in 0..1, never a scale.** An earlier draft had `pulse` return a scale in `1.0..=PULSE_MAX` and `avatar_rect` divide it back out by `PULSE_MAX` — a round trip through a unit nobody else used, and a second place for the growth constant to be applied. The growth ratio is now `avatar_rect`'s business alone.

For output frame `n` of the entry, `pulse` computes:

1. **Window:** the samples in `[n / 30, (n + 1) / 30)` seconds of the recording — 533 samples at 16 kHz. Past the end of the file the window is empty.
2. **Level:** `rms = sqrt(mean(x²))`, `db = 20·log10(max(rms, 1e-6))`, `level = level_from_db(db)`. An empty window is `level = 0`.
3. **Smoothing:** `s(n) = smooth(s(n−1), level(n), 1.0/30.0)`, i.e. `s + a·(level − s)` with `a = 1 − exp(−dt / τ)`, `τ = PULSE_ATTACK` while rising and `PULSE_RELEASE` while falling, `s(−1) = 0`.

**Why an output frame is the window.** The pulse is consumed one value per output frame, so a window that is not the frame is a second sampling grid to keep aligned with the first. One frame of 16 kHz audio is 33 ms, which is already several pitch periods of a voice, so the RMS is stable before any smoothing is applied.

**Why record time is the right clock.** `PlanEntry::record_time(frame)` is `(frame − start_frame) / 30` (`crates/video-coach-core/src/plan.rs:67-80`), and the commentary region runs from the recording's own zero for the whole entry, freezes included (`crates/video-coach-core/src/audio.rs:118-131`). So the pulse table is indexed by `frame − entry.start_frame` and needs no interpolation, and during a freeze the picture holds while the avatar keeps talking — which is what the sound is doing.

**D4. Why nothing is persisted.**

The live series could be stored on the clip as it arrives. It is not, and the reasons are the ones that survive the live corner existing:

- **It is exactly derivable from a file the project already owns.** Storing it would be a second copy of a fact, and the one that could go stale.
- **It would be ~600 floats per clip in `project.json`**, for a number recomputed in milliseconds (D2).
- **Messages can be dropped**, so the stored series would have holes the file does not. That is tolerable for a live hint and not for what the export renders.
- **Clips recorded before this feature would have none**, and would have to render differently from clips recorded after it.
- **10 Hz is a 100 ms staircase on a 30 fps render**, and interpolating it would invent the shape the filter is supposed to find.

So the live meter drives the corner and nothing else, and the file drives every pixel that gets written.

**D5. Where each is computed.**

- **Rendered, in media, on the job's own thread:** `composite/avatar.rs::pulse_table(recording, frames, cancel) -> Vec<f64>`, which calls the reader and then core's `pulse`.
  **Both tails build their tables in job setup, not in the frame loop.** Export builds one per avatar entry **before the first frame is pushed**, beside `Mixer::new(job)` (`composite/export.rs:275`) where `core::audio`'s regions are already resolved for the whole run; preview builds its one when the job starts. An earlier draft put the call beside `Pip::open` — which is *inside* the frame loop, run lazily at each entry change (`export.rs:309`) — and a whole-file audio decode there would stall the pump thread mid-run, holding the appsrc and the encoder behind it.
- **Live, in the app:** one `f64` of state on the UI struct, stepped by `avatar::smooth(previous, level_from_db(rms_db), 0.1)` on each `Event::Level`, and read by the placement callback (G2).
- Core never sees a file and never sees a pixel; media and the app never decide a number.

### E. Rendering: the overlay draws it, not the GL PiP pad

**E1. The decision.** The avatar is drawn into the existing tiny-skia overlay layer, under the strokes and the bar, at the same point in `OverlayRenderer::draw` where the layer order is already decided (`overlay.rs:316-333`).

| | Overlay (chosen) | GL PiP pad |
|---|---|---|
| Per-frame CPU at 1080p | **measured before the code is written** (E4) | ~0 |
| Preview and export parity | Free: `overlay.rs::draw` is the one shared drawer | Two code paths — export pushes PiP buffers from its pump, preview plays the recording's video pad natively (`composite/preview.rs:8-10, 562-566`), and an avatar has no video pad, so preview needs a third appsrc and a per-frame push |
| The pulse | A rect passed per frame into a function already called per frame | A **per-frame** pad rect, where `Layout` is per **entry** today (`composite/mod.rs:349-355, 392-396`). Needs a per-frame level threaded into the PTS-keyed probe — the probe discipline itself is non-negotiable, since a rect set from the pushing thread lands up to 4 frames early [measured, Phase 8 E2] |
| Caps risk | None | A new GL texture on a pad that also carries the 1×1 filler. The filler is GL **precisely because** a system-memory buffer on that pad breaks `glupload` mid-run (`export.rs:556-567`, reproduced) |
| New code | One draw step, one `OverlayFrame` field | An upload, a per-frame level table in `Schedule`, a preview appsrc and branch |

**Why this does not break CLAUDE.md's pixel split.** The rule is that GStreamer owns every **full-frame** pixel operation on the GPU and Rust owns the vector overlay layer, and it exists because full-frame resampling in Rust costs 37.56 ms a frame against 3.62 ms for vectors [measured, the compositing spike, lines 16 and 39]. The avatar is an inset of about 133k pixels, 6% of a 1080p frame, and it is the only raster element in a layer that is otherwise vector. It is the same class of thing as the text bar's plate, not the same class as the base image.

**E2. What is drawn.**

- `OverlayFrame` (`overlay.rs:106-132`) gains `avatar: Option<(&tiny_skia::Pixmap, LayoutRect)>` — the pre-scaled premultiplied image and the rect `avatar_rect(avatar.rect, level)` for this frame. `None` for every frame that has no avatar: every camera clip, every clip with `show_pip` off, and every reel and whole-match entry (`clip: None`).
- The draw is one `draw_pixmap` with a scale transform and bilinear filtering, in **output space**, like the bar and the scoreboard. It is not mapped through the zoom and it is not in the picture rect: the inset is chrome, which is the rule `layout.rs` states for the PiP (`layout.rs:16-19`).
- It goes **after** `draw_highlights` and **before** the bar's background, so the layer order stays what it is today: the coach's pen over the inset, the bar's words over the pen, the scoreboard over everything (`overlay.rs:316-333`). A stroke drawn over the inset lands over it, exactly as it does over a webcam.
- Nothing is drawn behind it. A transparent PNG floats (A1, A3); a rectangular photo fills its rect.

**E3. The GL PiP pad still runs, and still takes the filler.**

`Pip::open` returns `Pip::filler()` when `!clip.shows_camera_pip()`, checked **before** the probe (`export.rs:451-475`). Two reasons:

- The probe on a video-less Matroska returns `ProbeError::NoVideo` (`probe.rs:30-31`) and `refuse` prints a line per entry. Checking the predicate first keeps stderr honest: "no picture-in-picture" is not what happened.
- The pad is then fed the 1×1 GL filler every frame, which is the path `show_pip: false` already takes, and which is why an export mixing an avatar clip and a camera clip cannot hit the caps-feature trap: the pad carries GL memory from the first frame to the last, and only its size changes (`export.rs:556-567`).

**E4. One measurement, taken before the rendering task commits to this.**

The plan's rendering task opens with a microbenchmark and nothing else:

> `tiny_skia::PixmapMut::draw_pixmap` of a pre-scaled inset-sized pixmap into a 1920×1080 `PixmapMut`, with bilinear filtering, at the two ends of the range — a scale of `1.0 / PULSE_GROWTH` (rest) and of `1.00` (full) — timed over enough iterations to be stable, against the **3.2 ms** the whole overlay costs today.

That is the number that decides. If the blit is a small fraction of 3.2 ms, the overlay is the answer and the task proceeds. **If it is not, the GL PiP pad is the fallback** (E1's right column), and the task stops and says so rather than shipping a slow frame.

An earlier draft instead specified a gated `Exporter` benchmark plus a fully costed fallback design. Both are replaced by the line above: a full export benchmark measures the encoder and the GPU as much as the blit, and costing a fallback in advance is design work for a branch that one number probably closes. **The estimate that draft carried (1.5–2.6 ms) should be ignored:** it scaled the compositing spike's 2.58 ms webcam figure, which is a *resample of a 640×480 source into the inset rect* (spike line 17) — a different operation from a near-1:1 blit of a pixmap already at the inset's size. The real number is likely 3–5× smaller, which is exactly why it is measured rather than argued.

### F. Preview and export parity

They share one composite (CLAUDE.md, `composite/`), and the avatar keeps that true:

- **Both** resolve the avatar image from `open.folder.join(project.avatar)` on the bus, as a snapshot on the job, like `highlights` and `scoreboard` already are (`export.rs:60-98`, `preview.rs:83-108`). `ExportJob` and `PreviewJob` each gain `avatar: Option<PathBuf>`.
- **Both** call `avatar::open` once for the run, at the run's own output size — 1920×1080 for export, 1280×720 for preview (`preview.rs:75-76`). The ratios in `layout.rs` do the rest, which is what that module exists for.
- **Both** build their pulse tables in job setup (D5) and index them by `frame − entry.start_frame`.
- **Both** hand the same `OverlayFrame.avatar` to the same `OverlayRenderer::render`.
- **Preview must not request, place or link the PiP pad for an avatar clip.** All three sites take `clip.shows_camera_pip()` (B3). An avatar recording has no video pad, and **an unfed mixer pad stalls everything** [measured, Phase 8 E2].

The consequence is the property that matters: what the coach checks in the preview is what the file gets, at a different size.

### G. The UI

**G1. Picking the image** lives in the Devices popover, beside the Camera and Microphone lists (`app.slint:3337-3364`), because that is already "where a take's inputs are chosen".

- A third section, **Inset**, holding: the picked image as a thumbnail with its file name, a **Choose…** button that opens a file dialog (`rfd`, already a dependency), and a **Remove** button.
- With no image picked, the section reads "Camera — no avatar picked", and **Choose…** is the only control. There are no radio rows: the image is the mode (B1).
- With the image file missing, the thumbnail is replaced by "avatar.png is missing".
- **Remove** clears `Project.avatar` and deletes the project's copy (A2's Remove); the coach's original file is untouched. The section goes back to "Camera".
- Both changes are saved with the project.

**G2. The corner while recording** shows the avatar, and it pulses.

The existing self-view `Image` is reused as it stands (`app.slint:1659-1667, 2643-2651`): it is already placed by `place_self_view(content, aspect)` → `layout::pip_rect_over_picture` (`main.rs:1176-1194`), already sits over the picture and under the drawings, and already takes no input. Three changes:

1. **The picture.** In an avatar project the app sets `self-view` once, at the start of the take, from `media::decode_still(avatar_path)` → `slint::Image::from_rgba8`, which is the constructor the self-view frames already use (`crates/video-coach-app/src/video.rs:199-211`). **Not `slint::Image::load_from_path`:** the workspace builds slint with `default-features = false` (`Cargo.toml:20`) and no image-decoder feature, so `load_from_path` has no decoder to reach for. Going through `decode_still` is also what makes "validated at pick time" mean something — the corner and the export are then fed by the same decoder, and a file that passes the pick cannot fail in one and work in the other. Straight alpha here, premultiplied only where tiny-skia needs it (A3).
2. **The size follows the voice.** `place-self-view` gains a third argument, the level:
   `place-self-view(PictureRect, aspect: float, level: float) -> PictureRect`,
   and the Rust callback (`main.rs:1176-1194`) returns `avatar_rect(pip_rect_over_picture(picture, aspect), level)`.
   **A camera take passes `level = 1.0`**, which `avatar_rect` maps to exactly `pip_rect` — so the self-view is unchanged, with no branch in the callback and no second placement path. An avatar take passes the smoothed live level (D5).
   The level is a UI property set from the `Event::Level` handler, so the corner follows the voice at the meter's 10 Hz without a new timer.
3. **The quiet timer gets the avatar case.** `self_view_shown` is `recording && self_view_at.elapsed() < SELF_VIEW_QUIET` (`main.rs:64, 2184-2189`), and `self_view_at` is stamped by an arriving camera frame (`main.rs:2155-2156`, from `video.rs:115-116`). One second without a frame hides it, because a frozen camera picture would lie about the camera. **An avatar take has no frames and must not be hidden**: the rule becomes "shown while recording, if this is an avatar take *or* a camera frame arrived within `SELF_VIEW_QUIET`". A still image is not a lie about anything — and it is not still anyway, since its size is following the microphone, which is the honest signal.

**G3. The inspector** changes by one word. The existing "Show webcam in export" checkbox (`app.slint:466-468, 2909, 2925`) reads "Show avatar in export" when the selected clip's `inset` is `Avatar`. Same property, same command, same undo (B3). A clip whose project has lost its avatar image also shows a one-line note under it, so the coach is told before they export rather than after.

**G4. Nothing else moves.** The recording bar, the level meter (still `peak_db`, D1), the elapsed clock, the drawing tools, zoom, tagging, the transcript row and the export sheet are untouched.

### H. Format: v10

Following Match Vision F, and the guard as it stands (`store.rs:15-26`).

| Version | Change |
|---|---|
| v10 | `Project.avatar: Option<String>`; `Clip.inset: Inset` |

Two fields, not three: `Preferences.avatar_for_new_recordings` is deleted (B1).

- **`MIN_READABLE_FORMAT_VERSION` stays 7.** Both changes are additive and read as they stand: a v7–v9 file has no `avatar` key (`None`) and no `inset` on a clip (`Inset::Camera`).
- **An older build refuses a v10 project** with `StoreError::TooNew { found: 10, supported: 9 }`. That is the whole reason to bump (F3): serde ignores unknown keys, so a v9 build would otherwise open a v10 project, drop the avatar and the per-clip inset, and save them away.
- **The first save at v10 keeps a way back.** `store::write` already copies `project.json` to `project.json.v9` before raising the version, once, never overwriting (`store.rs:175-193`). Restoring it reopens the project in the older build, losing the changes since.
- **The graceful floor, if a v10 project ever reaches an older build another way:** an avatar recording has no video track, `Pip::open`'s probe says `NoVideo`, and the entry gets the transparent filler. No inset, no crash, no wrong face.
- **Source edits need no new remapping.** `avatar` is one file for the project and `inset` is a clip's own field, so `move_source`, `remove_source` and `source_is_referenced` are unchanged (`project.rs:272-357`).

### I. Edge cases

**I1. An avatar clip's recording played by an older build.** It can't open the project (H), and if it could, the audio-only recording gets the filler and no inset.

**I2. An avatar clip beside a camera clip in one compilation.** The PiP pad carries GL for the camera entries and the 1×1 GL filler for the avatar entries — a size change on a pad whose caps feature never changes, which is the case the filler was made GL for (`export.rs:556-567`). The overlay carries the avatar on the avatar entries only. Both are per-entry decisions taken where every other per-entry decision is taken (`export.rs:304-320`).

**I3. A silent take.** Every window's RMS is at or below the floor, `s` stays 0, every level is 0, and the avatar sits still at `1/1.10` of the inset. Correct and quiet.

**I4. A very loud take.** `level` saturates at 1 for the loud stretches, and the drawn rect is exactly `pip_rect`. It cannot go further: `s ∈ [0, 1]` by construction, so `avatar_rect` returns a rect between `pip / PULSE_GROWTH` and `pip` — **never larger than the camera's inset**. The inset's footprint is therefore identical to a camera clip's, `layout.rs` gains no constant, and the live corner uses the same placement. The cost is that the resting avatar is 91% of the inset rather than 100%; the alternative (rest at 100%, grow past it) would put a loud frame 17 px into the 24 px gap above the text bar at 1080p, which is a layout change dressed as a pulse.

**I5. Transcription is unaffected — verified.** `transcribe.rs` reads a recording through the same `Reader` at 16 kHz mono (`transcribe.rs:629`), and `Reader::start` finds its audio stream from `decodebin3`'s stream collection and **selects it explicitly**, never requiring a video stream (`composite/audio.rs:346-380`; the comment at `:349-358` is about a file with **no audio**, which an avatar recording is not). An audio-only Matroska is if anything the easier case: there is no unselected video stream to post `not-linked`. The model, the queue, the preemption rules and the transcript row are all unchanged, and no new test is owed for it.

**I6. The avatar image is replaced mid-project.** Every avatar clip renders with the new image from the next preview or export. That follows from "one avatar image per project". A running export keeps the image it opened, because the job carries a snapshot (F).

**I7. Reel and whole-match entries.** `clip_id: None`, so no clip, no `inset`, no commentary and no inset at all — the existing filler path (Match Vision R4, W2). The avatar is a property of a commentary take, and those entries have none.

**I8. A huge image.** It is decoded at native size once per job, which is a transient RGBA frame, then scaled down and released. A real avatar is a few megabytes. Bounding the decode by pixels is in Deferred.

---

## Crate responsibilities

| Crate | Contents |
|---|---|
| `video-coach-core` | `Inset`, `Clip::shows_camera_pip` / `shows_avatar`, `Project.avatar`, the v10 bump, the amended `project.rs` module comment. `avatar.rs`: `level_from_db`, `smooth`, `pulse` (samples in, one level per output frame out), `avatar_rect`, and the constants. **No new dependency** — the audit still lists exactly `serde`, `serde_json`, `thiserror`, `uuid`. |
| `video-coach-media` | The camera-less recorder branch and `CaptureSources`' optional video, on both arms. `level_dbs` (peak **and** rms). `decode_still`, the one avatar decoder. `composite/avatar.rs`: the pre-scaled premultiplied `Pixmap`, and the pulse table via the existing `Reader`. The avatar draw step in `overlay.rs`. `Pip::open`'s predicate; preview's three PiP sites. A still-image fixture. |
| `video-coach-app` | Bus: avatar mode in `capture_sources` (both capture kinds), the camera refusal's new wording, picking, copying and removing the image, `avatar` on both jobs. UI: the Devices popover's Inset section, the pulsing corner during a take, the inspector's label. |
| `video-coach-harness` | An avatar take end to end: record with test sources, get a clip, export it. |

Core touches no pixel and no file. Media hands it a slice of `f32` and a `Rect`; everything that decides the motion is tested on CI with synthetic input.

## Testing

- **Core:**
  - `pulse`: silence is all 0; a full-scale tone saturates at 1; a step from silence rises within two frames and decays over about seven; an empty slice and a `frames` longer than the audio both give 0 tails; **every output is within `[0, 1]`** as a property over random input.
  - `smooth`: rises on the attack constant and falls on the release; `dt = 0.1` and `dt = 1/30` reach the same steady state; a `dt` of 0 changes nothing.
  - `level_from_db`: the floor is 0, the ceiling is 1, below and above clamp.
  - `avatar_rect`: equals `pip` at `level == 1`, is centred on `pip` and `1/PULSE_GROWTH` of it at `level == 0`, is monotone in `level`, and **never exceeds `pip`** as a property over random levels.
  - `shows_camera_pip` / `shows_avatar`: the four combinations, and never both true.
  - Format: **one** test, `v7_to_v9_files_load_under_v10`, extending the existing `v7_and_v8_files_load_under_v9` (`crates/video-coach-core/tests/project_format.rs:265`) — a v7, a v8 and a v9 file each load with `avatar: None` and every clip `Inset::Camera`, and a v10 file round-trips. `store.rs`'s version guard is already covered by `swift_era_v6_is_refused`, `newer_format_is_refused_as_too_new` (which reads `CURRENT_FORMAT_VERSION + 1`) and `write_stamps_the_current_format_version`; cloning those four for v10 would test serde's version number, not this feature.
  - `add_recorded_clip` takes `inset` from `project.avatar.is_some()`, as it takes `show_pip` from the preference.
- **Media** (generated fixtures only — no real camera, microphone or network, ever):
  - `fixtures::still_image(dir, w, h, format)` writes a PNG and a JPEG with `videotestsrc num-buffers=1 ! pngenc|jpegenc ! filesink`. `decode_still` reads both, keeps a non-square aspect, and errors on a non-image and on a missing file.
  - **Premultiplication:** a still with a half-transparent region, decoded and opened, has `pixmap` bytes whose colour channels are scaled by alpha — drawn over white it reads as the blend, not as the full colour.
  - The recorder with `CaptureSources::Test { video: None }` writes a playable Matroska with an audio track and **no** video track; `t0_ns > 0`; `StopOutcome.duration` matches the audio; `self_view_pipeline()` is `None`; `FirstBuffer` arrives.
  - `level_dbs` returns both fields, and on a loud tone `peak_db > rms_db`.
  - Export of a one-entry avatar compilation: the inset's pixels land inside `pip_rect` on a loud frame and inside a strictly smaller rect on a silent frame, read back with `fixtures::decode_rgb`. Properties, not golden images, as in Phase 9.
  - **One** mixed compilation, avatar-then-camera: the asserted frame count, no stall. (The reverse order exercises the same pad through the same filler; running it both ways tests the test.)
  - An avatar clip whose image file is gone exports with no inset and no failure.
  - Preview of an avatar clip runs and does not stall (the unrequested PiP pad).
  - The `draw_pixmap` measurement of E4, recorded in the commit message rather than asserted in a test.
- **Harness:**
  - Record in an avatar project with `CaptureKind::Test`, get a clip with `inset == Avatar` and an audio-only recording, export it end to end.
  - Record in a camera project with `CaptureKind::Test`: the clip is `Inset::Camera` and the recording has video. (Together these prove the `Test` arm honours the mode — C1.)
- **Manual (batched, the user's eyes):**
  - Pick a real avatar, record a take, watch the corner while talking, then the preview and the export. The one thing only the user settles is whether the pulse reads as talking: `PULSE_GROWTH`, the attack and the release (Q1).
  - A transparent PNG avatar over a bright pitch.
  - A portrait photo straight off a phone, for the EXIF rotation (A3).

## Risks

1. **The overlay blit costs more than the measurement predicts.** E4 takes the number before any of it is built, and the GL pad is the fallback.
2. **The pulse constants are a guess.** They are named `const`s in core with no stored copy, so retuning them changes every render and nothing in any file. Q1.
3. **The live and rendered estimators disagree visibly.** They share the thresholds, the filter and the constants, but not the rate or the exact measure (10 Hz element RMS against 30 Hz frame RMS). The coach sees them minutes apart, so a small difference is invisible; a large one would mean a bug in one of them, and the manual check above is where it shows.
4. **A very large image decodes large before it is scaled.** A pathological JPEG could decode to a few hundred MB transiently. Bounding the decode by pixels is in Deferred.

## Deferred

- **Switching an existing clip between camera and avatar.** The design makes it nearly free — one `ClipEdit::Inset` variant, one checkbox, one undo case — because the avatar is drawn from the project's image and the recording's audio, and a camera clip has both. Not in v1 because a clip should say what it was recorded with. Q5.
- **More than one avatar per project**, or a per-clip avatar. Contradicts the user's decision; revisit only if a project ever has two commentators.
- **Bounding the image decode by pixels** (Risk 4).
- **A background plate, a border or a rounded mask for the avatar.** The image is drawn as it is. Q3.
- **Other motions** (a bob, a tilt, a mouth swap, two-frame talking). The user asked for a pulse.
- **The avatar in the corner while scanning**, outside a recording. Q4.
- **Animated avatars.** A multi-frame file yields its first frame (A3).
- **Recording with a camera *and* an avatar**, picking per take at the Record button. The image is the mode, so switching is a Choose…/Remove away; a per-take control would be a second place to look.

---

## Open questions for the user

Each has a default, and **the plan proceeds on it unless the user says otherwise.**

- **Q1. Is the pulse the right size and speed?** The default is 10% growth (`PULSE_GROWTH = 1.10`), a 60 ms attack and a 220 ms release, over −45 to −12 dBFS. Too small and it reads as a wobble; too big and it reads as a cartoon.
  **Default:** those numbers, tuned once by eye on the user's first real take. They are `const`s in core and store nothing, so retuning them costs a rebuild and no file change.
- **Q2. Does "avatar mode" belong to the project or to the machine?** It is the project's by construction now: the image lives in the project folder and its presence is the mode (B1).
  **Default:** the project's.
- **Q3. Should a cut-out avatar get a plate behind it?** A transparent PNG currently floats directly over the picture, with no rectangle and no border.
  **Default:** no plate. Add one only if a cut-out is hard to read over a bright pitch.
- **Q4. Should the avatar show in the corner while scanning, outside a recording?** Today the corner is the recording's self-view and is empty otherwise.
  **Default:** no. The corner means "a take is running".
- **Q5. Should a clip recorded on camera be switchable to the avatar afterwards (and back)?** It is nearly free to add (Deferred), but it means an exported clip can show a face that was never in that take, or hide one that was.
  **Default:** no for v1. Say the word and it is one field, one checkbox and one undo case.
