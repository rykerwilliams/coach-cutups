# Linux Port — Design

**Date:** 2026-09-19
**Status:** Reviewed — two adversarial review passes and four deliberation passes applied
**Supersedes:** `rust/docs/plans/2026-04-30-rust-rewrite-phase-7-source-transport.md` (docs-only; no code was ever committed)
**Evidence:** `docs/superpowers/spikes/2026-09-19-compositing-throughput.md`, `docs/superpowers/spikes/2026-09-19-seek-latency.md`

---

## Goal

Coach Cuts runs natively on Linux, with the same workflow it has on macOS: scan match film, tag moments, record webcam + mic commentary with synchronized freehand telestration, and export one clip per tag with scoreboard, PiP, drawings and zoom burned in.

The macOS app is not maintained in parallel. This is a replacement.

## Locked decisions

| Decision | Choice | Consequence |
|---|---|---|
| Primary platform | **Linux, native** | PipeWire capture, VA-API/NVENC encode, AppImage or Flatpak. |
| Language + stack | **Rust + GStreamer + Slint** | One media dependency covers decode, encode, capture and mux. |
| Transcription | **whisper.cpp, no summarization** | The `summarize` half of the intelligence seam is deleted, not stubbed. |
| Project format | **Clean slate** | No migration from Swift-era v1–v6. |
| Pixel work | **GStreamer owns full-frame pixels on the GPU; Rust owns the edit and the vector overlay** | Measured, not assumed. See "The compositor decision". |
| Output codec | **H.264 High in MP4** | HEVC deferred; YouTube re-encodes on ingest anyway. |

## Non-goals

- macOS support. The Swift tree stays as the reference implementation.
- Summarization of commentary transcripts.
- Any WSL target. Capture through WSL2 is unreliable and this app is capture-heavy.
- **Android.** Not a target. The stack doesn't foreclose it — Slint and GStreamer both run there — but the scrub-and-tag-with-keyboard workflow is a different product, so revisit only as a companion viewer for exported clips, if at all.

---

## Starting state

The `rust/` directory contains one plan document and no code. It references "Phase 5's `compose.rs`" and "Phase 6's File-menu wiring" as completed, but nothing was committed. **The port starts from zero Rust.** Its architecture sketch is adopted below; its phase numbering is abandoned.

| Area | LOC | Disposition |
|---|---|---|
| `VideoCoachCore` pure logic | ~1,950 | **Translate**, with invariants recorded below. |
| `VideoCoachCore` media-bound | ~1,500 | **Rebuild** on GStreamer + tiny-skia. |
| `apple/App` (SwiftUI + AppKit) | ~8,625 | **Rebuild** in Slint. |
| `VideoCoachCore` tests | ~6,692 | 43% translate; 57% is a rewrite. See "Test strategy". |

---

## Architecture

### Crate layout

```
crates/
  video-coach-core/     pure logic, zero media deps, no I/O beyond serde
  video-coach-media/    GStreamer: source player, capture, export, compositor
  video-coach-app/      Slint UI, command bus, event layer
  video-coach-harness/  headless integration tests driven over the bus
```

**Core isolation.** `video-coach-core` declares no media dependency — not GStreamer, not an image or font crate, not a feature that pulls one in. CI runs `cargo test -p video-coach-core` on a machine with no GStreamer installed, which fails loudly if one is ever added. That is the enforcement; a `--no-default-features` flag would test nothing, because there are no media features to turn off.

### Command bus

The UI dispatches `Command` values onto an async bus; the bus task owns the media objects and emits `Event` values back.

**Bus contract:**

- **Event-log-bound commands carry caller-captured timestamps.** `Command::RecordPlay { host_time_ns, source_time_seconds }` and its `RecordPause` twin are constructed with both values read on the UI thread *immediately before* the transport call — never timestamped when the bus handles them. `RecordingController.appendPlay(atHostTime:sourceTime:)` (`apple/App/Recording/RecordingController.swift:42-57`) exists precisely because of this, and its call site (`ContentView.swift:774-781`) captures `CACurrentMediaTime()` and a *synchronous* position query before calling `play()`, with a comment (`:753-773`) explaining that the cached observable position lags by up to a frame and puts drawings behind the ball on replay. Queue delay would reintroduce exactly that drift. This requires the UI thread to hold a handle for `query_position` on the running pipeline; that is the **only** direct pipeline access permitted outside the bus task — graph construction, state changes and relinking stay on the bus.
- **`Command` is not serde-serializable.** The superseded plan required it and wrote round-trip tests per variant. Nothing here serializes a command; `Debug` for tracing is the real requirement.

*Deferred — see Open questions:* who owns `Project`, and how Slint reads state.

### Media pipelines

Element names below are verified against GStreamer 1.24.2 unless marked otherwise.

**Source playback (scan).**

```
filesrc location=<path> ! decodebin3
  ├─ video pad (pad-added) → video/x-raw(ANY) ! queue ! glupload ! glcolorconvert ! <GL sink>
  └─ audio pad (pad-added) → queue ! audioconvert ! audioresample
                                   ! volume name=scan_volume ! autoaudiosink
```

`decodebin` exposes video and audio as separate sometimes-pads via `pad-added`; it is not a `tee` (which duplicates one stream and does not demux). The `queue` on each branch is required — without it both branches share a streaming thread and the audio sink's blocking stalls video.

**`decodebin3` is required, and `decodebin` is not an acceptable fallback.** Measured on Intel VA-API (seek-latency spike): `decodebin` auto-plugs the same hardware decoder but negotiates **system memory** into `glupload`, running ~6× slower (117 vs 739 fps at 1440p) and making accurate seeks ~5× slower. If `decodebin3` is ever unusable, the fallback is an explicitly built `demux ! parse ! <hw decoder>` chain, which measured the same as `decodebin3` (722 fps, DMABuf).

**Select the video stream by caps, never by pad order.** `decodebin3 ! <consumer>` links whichever pad appears first, which is frequently audio; during measurement this silently timed audio seeks and left the video decoder unlinked. The `video/x-raw(ANY)` filter (or explicit stream selection) is part of the contract.

**Capture.**

```
pipewiresrc (camera) → videoconvert → x264enc → h264parse ─┐
                                                           ├→ matroskamux → filesink
pipewiresrc (mic)    → audioconvert → opusenc            ─┘
```

Both sources share one pipeline clock. macOS used a single `AVCaptureSession` whose synchronization clock timestamped both media types (`CaptureSessionController.swift:205-218`); two independent PipeWire sources will drift without an explicit shared clock and live-source handling. `v4l2src` + `pulsesrc` is the fallback when PipeWire is absent. Matroska rather than MP4 because a crash mid-record leaves a playable file.

Camera format is pinned to the highest-resolution 16:9 format ≤1280 wide supporting 30 fps, with min and max frame duration locked to 1/30 (`CaptureSessionController.swift:180-201`). This bounds PiP quality and file size deliberately; the equivalent is a caps filter plus `videorate`.

**Export.**

Rust owns the **edit** (which source frame appears at each output PTS) and the **vector overlay**. GStreamer owns **all full-frame pixel work** on the GPU. Decoded video never enters a Rust-owned CPU buffer.

```
per source:  filesrc ! parsebin ! <hw decoder> ! appsink name=src
per clip:    filesrc ! parsebin ! <hw decoder> ! appsink name=cam

  ┌── Rust frame pump, once per output frame N (PTS = N/30) ─────────────┐
  │ segment(N) from the flat segment list:                               │
  │   .play   → pull next decoded source buffer                          │
  │   .freeze → re-push the held buffer                                  │
  │ push that GstBuffer UNCHANGED (ref + restamp) → appsrc base          │
  │ zoom(N) = zoomAt(recordTime) → set base branch transform             │
  │ rasterize strokes + text bar + scoreboard (tiny-skia) → appsrc ovl   │
  └──────────────────────────────────────────────────────────────────────┘

appsrc base ! glupload ! glcolorconvert ! gltransformation ! glvideomixer.sink_0
appsrc cam  ! glupload ! glcolorconvert                    ! glvideomixer.sink_1
appsrc ovl  ! glupload ! glcolorconvert                    ! glvideomixer.sink_2
glvideomixer ! <h264 encoder> ! h264parse
             ! video/x-h264,stream-format=avc,alignment=au ! mp4mux faststart=true ! filesink
```

`h264parse` with explicit caps between encoder and `mp4mux` is not optional — without it the muxer either refuses to link or emits a file that plays in VLC and close to nowhere else.

**Zoom is a crop plus a scale on the base branch, before the mixer.** `Zoom.clamped()` (`Zoom.swift:18-25`) guarantees the visible window lies entirely inside the source — at `pan = ±(s−1)/2s` the window edge lands exactly on 0 or 1 — so there is no edge handling.

**Sub-pixel geometry is a requirement.** `zoomAt` lerps between keyframes every frame, so pans are continuous; an integer-pixel crop stair-steps visibly on a slow pan. Verified: `gltransformation`'s `scale-x`/`scale-y` and `translation-x`/`translation-y` are all `Float` (translation in universal [0-1] coordinates). *Acceptance test: a 20-second pan at scale=3 shows no stair-stepping.*

**Overlay alpha.** tiny-skia emits premultiplied RGBA. Verified: `glvideomixer` sink pads expose `blend-function-src-rgb`/`blend-function-dst-rgb` and `blend-equation-rgb`, so premultiplied-over is configured on the pad (`src = one`, `dst = one-minus-src-alpha`) rather than demultiplying every pixel in Rust.

**Rejected: `cairooverlay` and any "draw on the frame" element.** They need the frame in system memory, forcing a GPU download of every full frame — the exact round-trip removed in May (below). `gloverlaycompositor` (present) is a legitimate later optimization; the third mixer pad is uniform across preview and export and easier to test.

**Software fallback, and the CI path.** The same graph runs with `videoconvertscale` + `compositor` when GL interop is unworkable. That is also the CI path, since CI has no GPU. Slower, and its crop is integer-only; both acceptable for CI.

**Export audio.**

```
source decode → audioconvert ! audioresample ! F32LE/48k/2ch → appsink ─┐
                                                                        ├→ [Rust mixer] → appsrc → avenc_aac ─┐
webcam decode → audioconvert ! audioresample ! F32LE/48k/2ch → appsink ─┘                                     ├→ mp4mux
                                                                                                               │
(video branch above) ──────────────────────────────────────────────────────────────────────────────────────────┘
```

Three behaviors carry over from `CompilationExporter.swift:198-205, 357-414`:

1. **Source audio plays only during `.play` segments.** Freezes are silent by design.
2. **5 ms ramps at segment boundaries.** Without them there is an audible click at every play/freeze transition, and every clip has at least one, because `appendInitialPause` fires at recordTime 0 (`RecordingController.swift:96-98`) so the first segment of every clip is a freeze.
3. **Per-clip mic audio at a flat `commentaryVolume`.** Both volumes come from `Preferences.previewSourceVolume` / `previewCommentaryVolume` — export reuses the *preview* preferences (`ExportSheet.swift:631-632`). Do not invent a separate export setting.

**Gain is applied in Rust, on the PCM.** Not because control bindings are unsuitable — they handle sparse timed keyframes fine — but for single timeline authority: the mixer already owns the splice, so a gain is one multiply, whereas an element graph is a second timeline that must agree with the video driver's segment list exactly. A 5 ms ramp is ~240 samples; sample-accurate arithmetic is exact. And the ramp curve becomes a pure function in `video-coach-core`, testable with no GStreamer present.

**Ramp rule, stated once:** *every contiguous audio region, on either track, gets a 5 ms linear fade-in at its start and a 5 ms linear fade-out at its end, clamped to the output timeline at t=0.* macOS ramps only at interior boundaries *within* an entry (`:372-402`), leaving clip→clip boundaries and every mic start/stop unramped. The uniform rule reproduces macOS where it ramps and additionally removes the clip-boundary click. Same machinery, fewer special cases, strictly fewer clicks.

AAC-LC 48 kHz stereo at 192 kbps via `avenc_aac`. Source and mic sum with no limiter and both volumes default to 1.0, so a loud passage can clip; macOS behaves identically. Hard-clamp at the F32→encoder boundary and do not add dynamics processing.

### Export frame driver

AVFoundation did the splicing on macOS: `insertTimeRange` for `.play`, a one-tick slice plus `scaleTimeRange` for each `.freeze` (`CompilationExporter.swift:186-226`). The port owns that loop; it is the core of the export phase.

**Flat segment list, absolute output clock.** Flatten the plan into one sorted list before decoding anything:

```rust
struct OutSegment {
    out_start: f64, out_end: f64,      // absolute output timeline
    kind: Play | Freeze,
    source_index: usize, source_start: f64,
    entry: usize, entry_out_start: f64,
}
```

built by a single cumulative walk over `plan.entries[].segments[].out_duration`. `CompilationPlan.Entry.compositionStart` is a *separate* cumulative sum (of `recordingDuration`) and is **not** used for timing — it is UI metadata. The Swift exporter threaded one `CMTime` cursor end-to-end precisely to stop those two sums from disagreeing and leaving phantom inter-clip gaps (`:144-156`). One walk over the real segment durations removes the class of problem.

**Output is frame-indexed, not cursor-advanced.** Frame `n` has output time `t = n / 30`; binary-search the segment list for the segment containing `t`. No accumulating cursor means no drift, and a segment shorter than one frame interval simply has no frame land in it — which is what replaces the sub-tick skip at `:196`, an artifact of AVFoundation rejecting an empty `insertTimeRange`. Delete it; do not port it.

**Record time is output time minus entry start.** `record_time = t - segment.entry_out_start`. That single value drives `visibleStrokes`, the zoom lookup, the PiP frame index and the scoreboard clock.

**Decoding is seek-driven with forward reuse.** One decode pipeline per distinct `sourceIndex`, alive for the whole export. For each `.play` frame the driver needs the source frame containing `source_start + (t - out_start)`:

- at or ahead of the decoder's position and within ~2 s → pull and drop frames forward;
- otherwise → `ACCURATE` seek.

A freeze consumes **zero source time**, so after play→freeze→play the decoder already sits on the frame the next `.play` wants; the common case costs no seeks. Seeks occur at clip boundaries and `.skip` events — tens per export. A linear pass is the wrong shape: a 90-minute source would decode 90 minutes to emit ten.

**Freeze holds the last decoded frame**, pulling nothing. The exception is a freeze with no predecessor — the first segment of nearly every clip — where the driver seeks and decodes once. macOS hit this from the other direction: before freeze inserts existed, a clip starting on a freeze exported as **black** until the first `.play` populated the frame cache (`:172-186`).

**The one-tick freeze bias does not port.** `:207-221` biases each freeze slice one tick past its anchor because `insertTimeRange` selects the sample with PTS *strictly* less than the slice start, delivering the frame *before* the one the user saw — drawings then landed one motion step behind the ball. State the intent directly instead: **the frame for anchor `s` is the last frame with `PTS <= s`.** Assert it with a fiducial golden test; the bug is invisible to any test that doesn't check exact frame identity.

**Output frame rate is fixed at 30 fps.** A format decision, not an inherited default. It cannot be "match the source": one compilation can interleave clips from different source files at different frame rates (`CompilationPlan.buildPlan` orders by `sortIndex` across all sources), so there is no single rate to match. Fixing it also makes the frame-index clock and the ETA math exact. A 25 fps source duplicates every fifth frame; 60 fps drops every other. AVFoundation did the same at `frameDuration = 1/30` (`:297`) and it has never been a complaint. Stroke replay and zoom are time-parameterized, so nothing in the overlay stack depends on the value. Put the constant in `video-coach-core` with two consumers — macOS duplicated it in `ExportSheet.swift` with a comment admitting the exporter doesn't expose its own rate.

**Progress** is exact and monotonic: `frames_emitted / ceil(total_duration * 30)`, replacing the 5 Hz poll of `AVAssetExportSession.progress` (`:441-458`).

### The compositor decision

The macOS compositor runs two stages per output frame: a Core Image pass (base + zoom + PiP) and a Core Graphics pass (strokes, text-bar glyphs, scoreboard). **That split is not a decomposition — it is a workaround.** Core Image cannot rasterize glyphs or stroke paths; Core Graphics cannot do a GPU composite. The cost is visible in the code: the text bar's background is drawn in stage 1 and its glyphs in stage 2 (`CompilationCompositor.swift:152-166`, `:316-377`); there are three coordinate flips in one frame (`:233-234`, `:361-363`, `ScoreboardDraw.swift:121-131`) on top of `CIImage`'s bottom-left origin; `Zoom` carries a second transform variant purely to cancel that origin (`Zoom.swift:109-130`); and `PreviewCompositor` pre-decodes every freeze frame (`PreviewCompositor.swift:23-28`) only because AVPlayer calls `startRequest` out of temporal order. The two compositors are kept in sync by a comment (`:116-119`) and **have already drifted** — export honors `showPiP` (`:101`), preview does not (`PreviewCompositor.swift:163`).

**The port collapses this to one pull-based compositor, shared by preview and export, in a single top-left coordinate space.** Per output frame it evaluates `recordTime`, derives `sourceTime(atRecordTime:)`, pulls the matching frames, and draws in one pass. Preview differs from export only in output resolution and in where the result goes.

This is a genuine improvement to bank. On macOS the two paths were *forced* apart: `ClipPreviewBuilder.swift:266` documents that AVPlayer on macOS 26 strips the custom compositor's instruction subclass, so preview had to be rebuilt on the built-in compositor, which then silently ignores `setTransformRamp` (`:292`), forcing a stepwise-keyframe workaround. One shared graph deletes that entire class of "preview doesn't match export" bug.

**Why the measurement puts the split where it does.** This project already ran this experiment. `docs/superpowers/specs/2026-05-12-compositor-gpu-render-design.md:14-27` records that a 1080p export spent **78 of 87 wall-seconds** in `createCGImage` (GPU→CPU readback, 26%) and `CGContextDrawImage` (CPU rasterization, 55%) — 81% of total — and moved base+PiP to the GPU for a 3–4× win, *deliberately keeping strokes and the text bar on the CPU path* because "they're small and don't dominate the profile." An independent 2026-09-19 benchmark reproduced the same ratio in Rust: base blit with zoom 37.56 ms, PiP 2.58 ms, text-bar fill 0.51 ms, 40-segment stroke 0.83 ms, two text runs 0.22 ms — 41.71 ms total, 0.80× realtime, with full-frame resampling at 90% and the entire vector layer at 1.63 ms. Overlay-only rasterization measured 3.62 ms/frame (9.2× realtime) on a busier frame. Composing everything in Rust would have re-introduced, verbatim, the regression this repo measured and removed in May.

**Draw order (single pass, back to front):**

1. Opaque black fill of the output rect.
2. Base source frame, letterbox-fitted and zoom-transformed.
3. Text-bar background — **before** the PiP, so the PiP is not darkened by the bar tint.
4. Webcam PiP, bottom-right. **Not affected by zoom.**
5. Strokes.
6. Text-bar glyphs — on top of the PiP.
7. Scoreboard, top-left — on top of everything.

**Layout constants the port must reproduce** (all ratios, so they survive the preview↔export resolution change):

| Element | Constant | Value | Source |
|---|---|---|---|
| Text bar | height | `0.08 × outH` | `CompilationCompositor.swift:161`, `:317` |
| Text bar | fill | black, α `0.6`, full width, flush bottom | `:163` |
| Text bar | font size | `0.5 × barH`, white | `:339-341` |
| Text bar | inset | `0.15 × barH` both axes | `:354-355` |
| Text bar | empty string | draws nothing; background still drawn | `:332` vs `:160` |
| PiP | width | `0.22 × outW` | `:172` |
| PiP | height | `pipW × camH / camW` | `:173` |
| PiP | margin | `0.022 × outH` from bottom and right | `:174`, `:182-185` |
| PiP | corners | square | (absent) |
| PiP | visibility | honors `showPiP` in **both** paths | `:101` |
| Stroke | line width | `stroke.lineWidth × outH` (height) | `:276` |
| Stroke | caps/joins | round / round | `:310-311` |
| Scoreboard | bar | `0.36 × outW` × `0.08 × outH`, inset `0.015 × outH` | `ScoreboardDraw.swift:11-15` |
| Scoreboard | accent strip | `0.08 × barH`, home and away cells only | `:17`, `:41-43` |
| Scoreboard | columns | home `.30`, score `.20`, away `.30`, clock `.20` | `:20-23` |
| Scoreboard | team font | `min(fit(home), fit(away))`, desired `0.55 × barH`, 6pt floor | `:48-54`, `:89-94` |
| Scoreboard | score/clock font | `0.55 × barH`, bold | `:57`, `:60` |

**Base image fit — letterbox, not stretch.** Uniform scale `min(outW/srcW, outH/srcH)`, centered, black bars where aspects differ. macOS disagrees with itself: the mpv record/scan path letterboxes (`MPVSourcePlayer.swift:482-487`, `panscan=0`) while the export compositor stretches non-uniformly (`CompilationCompositor.swift:130-134`). They agree only for 16:9-into-16:9, and a non-16:9 source exported at a fixed 1920×1080 comes out **anamorphically distorted today**. `ContentView.swift:306-313` names the reason letterbox is right: the aspect-locked player exists so "recording and playback render pixel-identical at every zoom." (This is `min(sx, sy)`, not the crop-fill `max(sx, sy)` that `PreviewCompositor.swift:126-128` warns against. Letterbox never crops.)

**Two coordinate spaces:**

- **Content space** — the letterboxed, centered source rect *before* zoom. **Strokes live here.** They are captured normalized to the aspect-locked player view (`DrawingOverlayView.swift:77`, `:101`, inside the aspect-locked ZStack at `ContentView.swift:314-367`) and must denormalize against the content rect, not the output rect, or they drift off the picture on a non-matching aspect. Strokes are **not** zoom-transformed — the user drew on the already-zoomed picture. Stored coordinates are **top-left** normalized (capture Y-flips out of AppKit's unflipped view, `flipY: true`; the compositor does not flip again, `flipY: false`).
- **Output space** — the full delivered frame. **Text bar, PiP and scoreboard live here** (see Open questions). Stroke *line width* stays `lineWidth × outputHeight`, so stroke weight is constant per delivery resolution.

### Decoder selection

Symmetric with encoder selection. Auto-pluggers pick by plugin rank, and whether hardware wins by default **depends on the distro and GStreamer version**. On the reference laptop (Ubuntu 24.04, GStreamer 1.24.2) the `va` plugin's `vah265dec`/`vah264dec` rank **PRIMARY + 1**, above software `avdec_*` at PRIMARY, so hardware is selected with no intervention — contrary to this spec's original assumption. Older GStreamer, the legacy `vaapi` plugin (present at rank NONE) and other distros can still leave software decode on top, and software decode fails the seek gate by ~2.5× (seek-latency spike). So the probe below stays, as a safety net rather than a correction. macOS treated this as a gated decision worth recording in source (`MPVSourcePlayer.swift:36-37`, `hwdec = "videotoolbox"`, "recorded here as the source of truth"); Linux has no equivalent yet.

At media-subsystem init, probe `vah265dec`/`vah264dec`, then `nvh265dec`/`nvh264dec`, then `v4l2slh265dec`, and raise the rank of the first that instantiates to `GST_RANK_PRIMARY + 1` via `gst_plugin_feature_set_rank`. Software `avdec_*` remains the final fallback. Log the selected factory and its negotiated caps feature, and surface both in diagnostics alongside the encoder — a user on software decode should be told, not left to infer it from the scrubbing.

### Encoder selection

Output is **H.264 High profile in MP4**, chosen by runtime probe:

1. `vah264enc` / `vaapih264enc` (Intel, AMD)
2. `nvh264enc` (NVIDIA)
3. `x264enc` (software, `speed-preset=medium`)

**HEVC does not ship in the first cut.** The destination is YouTube, which re-encodes on ingest and recommends H.264; HEVC buys a smaller intermediate file and nothing else. Against that: a second probe chain, a second set of parser caps (`mp4mux` requires `stream-format=hvc1` — `hev1` produces a VLC-only file), and a far patchier hardware-encoder story across Linux GPUs. Additive later behind a setting, not before someone asks.

Licensing does not distinguish them: `x264enc` and `x265enc` are both `gst-plugins-ugly` wrappers around GPL-2.0-**or-later** libraries, which combine with this repo's AGPL-3.0 identically. Whichever software fallback ships, it ships as a GPL dependency on the same terms.

**Bitrate targets** come from `ExportSettings.bitrate` — 6/12/24 Mbps at 1080p for low/medium/high, halved at 720p — **but that table has never reached an encoder.** `CompilationExporter` documents `quality` as "currently ignored" (`:45-50`); `presetName(for:quality:)` returns `AVAssetExportPresetHEVC1920x1080` for every pair and never reads `quality` (`:508-518`); and `ExportSettings.bitrate` has *zero* production call sites — its only references are its own unit test. The Quality picker in `ExportSheet` currently changes nothing about the output file. **The Linux port is therefore the first implementation of quality control, not a port of one.** These are plausible targets to validate against real encoder output, not known-good values; the export phase must include an A/B pass and the numbers may move.

The ladder is `base1080 × {r720: 0.5, r1080: 1.0, r2160: 3.0}` — 3/6/12, 6/12/24, 18/36/72 Mbps. The ×3 rather than ×4 reflects sub-linear bitrate scaling at constant perceptual quality; medium at 2160p lands at 36 Mbps, inside YouTube's 35–45 Mbps SDR ingest range. Subject to the same validation caveat.

Report the selected encoder in the export sheet. Quality-at-bitrate differs between VA-API, NVENC and x264, so the presets will not look identical across machines; accepted, and per-encoder tuning tables are not worth it.

---

## Logic to port verbatim

Each gets its Swift test file ported alongside it.

**`PlaybackTimeline` — `sourceTime(atRecordTime:)` and `playbackSegments(sourceDuration:)`.**

- `.play`/`.pause` carry a captured `sourceTime` anchor that *overrides* the wall-clock cursor, because player latency makes it drift by tens of milliseconds.
- Playing past EOF splits into a `.play` tail plus a `.freeze`.
- `freezeMaxSource = max(0, sourceDuration − 0.05)` (`:63`). The `max(0, …)` is not decoration — it keeps a sub-50 ms source from producing a negative anchor, and sub-50 ms sources are exactly what synthetic fixtures are. The macOS rationale was AVFoundation-specific (`:53-62`) and does not transfer, but the constant does: a pull-based compositor asks the decoder for a frame *at* `sourceStart`, and exactly `sourceDuration` is past the last frame.
- **Source time is clamped to `[0, sourceDuration]` on BOTH paths.** macOS clamps `.skip` in `playbackSegments` (`:130`) but not in `sourceTime(atRecordTime:)` (`:38`, and the unbounded rate integration at `:29`, `:42`). The asymmetry is unpinned by any test and survives only because `sourceTime` has one production caller today. Under the golden rule below it becomes the sole input to the match clock on both paths, so give it the same signature and bounds as the segment builder. Two functions answering "what source time is on screen" must not disagree about what happens past EOF.
- Only `.play`/`.pause`/`.skip` split segments. Zoom and stroke events must **not**, or a pinch gesture explodes the segment count.

**`ScoreboardState.scoreboardState(absoluteTime:config:events:)`.** Derives period, clock, stoppage, break and fulltime positionally, with no sport-specific hard-coding, including the P1 back-anchor offset.

> **Golden rule — the match clock is a function of SOURCE time, never record time.**
> ```
> absTime(clip, recordTime) = project.absSeconds(clip.sourceIndex,
>                                                clip.sourceTime(atRecordTime: recordTime))
> ```
> The clock **holds during a freeze and jumps on a skip**, exactly as the footage does. Both preview and export call this one function.

macOS gets this wrong and the port must not carry it over: `CompilationCompositor.swift:253` uses `clipStartAbsSeconds + recordTime` — a per-clip constant plus wall-clock — ignoring every pause and skip. Since `appendInitialPause` lands at recordTime 0 on every recording (`CompilationExporter.swift:180-181`), the exported scoreboard runs ahead of the exported footage on essentially every clip. Preview (`ScoreboardReplayOverlay.swift:80-85`) and live scan (`ScoreboardOverlayView.swift:17-19`) already use the source-time rule; export is the outlier. **Port the preview formula and delete the `clipStartAbsSeconds` field** — a per-clip constant is what made the bug expressible. Pin it: a clip with a mid-clip pause of N seconds must show the same clock at recordTime `p` and `p + N`.

The goal-counting window is **half-open until the match is fully tagged.** Lower bound is period 0's start, inclusive (`ScoreboardState.swift:92`, `:136`). Upper bound is `.infinity` **unless** the interpreted start/stop count exactly equals `expectedStartStopEvents`, in which case it is the final whistle (`:128-130`). A goal tagged after the last tagged stop still counts while the match is partly tagged — the normal case while a coach works through film.

**`MatchInterpret.interpret(_:format:)`.** Stable-sorted by `absSeconds` with input-order tie-break (`:27-33`), then **truncated to `2 × totalPeriods`** (`:35-36`); even positions `.start`, odd `.end`. Each result carries `originalIndex` (`:11-14`) so `startStopRoles(in:)` (`:53-63`) can key roles by record `UUID` without re-interpreting per row — port both. `setAutoBackAnchorP1` inserts at index 0 so the flagged event wins that tie-break, and **deliberately bypasses both the cap and the scoreboard-configured guard** that `appendStartStop` enforces (`MatchEvent.swift:92-97` vs `:106-118`). Pinned by `ScoreboardTests.swift:324-340`, whose comment says outright the tests exist to stop a future reader "fixing" it. Port those tests with the comment intact.

**`Zoom`.** Clamping (floor 1.0, cap 10.0, pan limit `(s−1)/(2s)`, pan forced to 0 at scale 1), cursor-anchored zoom, snap notches `[1, 1.25, 1.5, 2, 3, 5, 7.5, 10]` at 3% tolerance applied on interactive commit only, never on replay.

> **Exactly ONE transform survives, and it is not the one the live macOS compositors call.** There are three: `transform(sourceSize:destSize:)` (`:75-84`, dead outside tests), `deltaTransform(viewportSize:)` (`:94-107`) and `deltaTransformForCIImage(viewportSize:)` (`:121-130`, a sign flip for bottom-left origin). All three map source point `0.5 + pan` to viewport center. They disagree on two things: **base fit** (`transform` letterbox-fits; the delta variants assume the caller pre-stretched) and **what `pan` is a fraction of** (`transform` uses the displayed image; the delta variants use the viewport). Identical only when image == viewport.
>
> `sourcePoint(atViewPosition:)` (`:48-55`) and the pan limit define pan in normalized **source** space, and mpv documents `video-pan-x` as a fraction of the scaled source (`MPVSourcePlayer.swift:469-474`). Since the port letterboxes, **`transform`'s formula is the correct one**:
> ```
> k  = min(outW / srcW, outH / srcH)
> s  = k * zoom.scale
> dx = (outW - srcW * s) / 2  -  zoom.panX * srcW * s
> dy = (outH - srcH * s) / 2  -  zoom.panY * srcH * s
> ```
> Both delta variants are **deleted**. They are correct only under the stretch assumption the port abandons, and a porter who reaches for `deltaTransform` because it is what the live compositors call will get pan wrong on every source whose aspect differs from the output's.

**`ClipZoomLookup.zoomAt(recordTime:)` — zoom replay is a LINEAR INTERPOLATION, not a step function** (`:7-32`). Scale and both pan components lerp with alpha clamped to `[0,1]`; holds first value before the first keyframe, last after the last, identity when empty. The lookup `break`s on the first keyframe past `t` (`:17`), so the event log must stay sorted by `recordTime`.

Three recorder rules exist **only because** of the lerp and port with it (`RecordingController.swift:128-137`):

1. **Anchor keyframe.** If >100 ms since the last distinct capture, emit a keyframe at `(t − 1 ms)` holding the *previous* value before emitting the new one. Without it the lerp ramps smoothly across the whole quiet period instead of holding then snapping.
2. **Dedupe.** Skip a capture equal to the last captured value (`:130`).
3. **No throttling, ever.** An earlier version throttled to ~20 Hz and it was a real visual bug (`:101-115`): the user pans while drawing on the ball and sees a smooth 60 Hz picture at record time, but replay is keyframe-stepped, so the drawing ends up offset by the unmatched pan delta. The segment builder already refuses to split on `.zoom`, so a dense track costs disk bytes and nothing else.

**`StrokeReplay.visibleStrokes(in:atRecordTime:)`.**

> **A `.stroke` event's `recordTime` is when the stroke FINISHED, not when it started.** It is appended on mouse-up (`DrawingOverlayView.swift:106-116` → `RecordingController.swift:71-73`), and each point's `t` is seconds since stroke start. Replay back-computes `firstT = ev.recordTime − points.last.t` (`StrokeReplay.swift:24`), and **everything** keys off `firstT`. A port treating event time as stroke start makes every stroke appear late by its own duration — proportional to how long the coach held the pen, so long strokes are visibly wrong while flicks look fine. Nasty to catch by eye.

Exact inequalities, all load-bearing (`:25-30`): visible from `t >= firstT`; auto-clear inclusive at `t >= firstT + auto`; a `clearAll` cancels only when `firstT < clearAllTime <= t` (**strictly** after `firstT`); points drawn = count with `t <= elapsed` (**strictly** greater in the index search). **A single-point stroke renders as a FILLED CIRCLE of diameter `lineWidth`**, not a stroked path (`CompilationCompositor.swift:283-299`) — zero-length paths don't rasterize with round caps in CoreGraphics, and tiny-skia behaves the same.

**Where each module lands.** Phase 1 ports only the modules defining the contract the media layer must satisfy: `PlaybackTimeline`, `CompilationPlan`, `Zoom`, `ClipZoomLookup`, `StrokeReplay`, `SkipCoordinator`. The rest port in the phase that first consumes them — `UndoController` and `TagAggregation` in Phase 3, `ExportProgress` in Phase 8, `ScoreboardState`/`MatchInterpret`/`MatchFormat` in Phase 9. `Project`/`ProjectStore`/`Tag.normalize`/`cumulativeOffset`/`absSeconds` are Phase 0 — they are the format. `ExportProgress` moves for a concrete reason: `Status.done(encodeWallSeconds:averageFps:)` and `.active(fractionCompleted:)` are modelled on `AVAssetExportSession.progress`, and the frame driver reports differently.

---

## Project format v2

Folder layout unchanged: `project.json` plus `recordings/` — plus `recordings/.trash`, which holds the one deleted recording undo can restore and is shredded on every project open (`Workspace.swift:182`, `:662-668`).

| Field | v6 | v2 | Why |
|---|---|---|---|
| `SourceRef.bookmark` | `Data` | `relativePath: String`, POSIX `/` separators | Bookmarks are macOS-only. May traverse `..`. POSIX separators so paths round-trip if Windows ever ships. |
| `recordingFilename` | `<uuid>.mov` | `<uuid>.mkv` | Crash resilience. |
| `Clip.summary` | `String` | *removed* | No summarizer ships. |
| `preferredCameraID` / `preferredMicID` | `AVCaptureDevice.uniqueID` | PipeWire node name or `/dev/v4l/by-id` path | Same hint/fallback/don't-clear semantics. |
| `Resolution` | `source` / `r1080` / `r720` | `r720` / `r1080` / `r2160` | `source` was ill-defined and partly broken: its `pixelSize` entry is unreachable dead code (`ExportSettings.swift:20`), its render size came from `sourceAssets[0]` rather than the clips in the plan (`CompilationExporter.swift:528`), its bitrate was the 1080p number at any frame size, and it has no meaning for a compilation mixing sources of different dimensions. `r2160` preserves the 4K capability with a defined size and bitrate. |
| `formatVersion` | 6 | **7** | Monotonic. See below. |

**Version stays monotonic.** Resetting to 1 was aesthetic and would have *created* the ambiguity a guard then had to patch: a Swift-era v1 file has no `formatVersion` key at all and decodes as 1 (`Project.swift:144-150`), so v1 and a reset "v2" are indistinguishable. Continuing at 7 makes the guard one numeric comparison — `formatVersion < 7 → refuse` — that cannot have holes. The proposed `bookmark`-key sniff would have missed a Swift file with an empty `sourceVideos` array, decoding it as valid.

**`CommentaryEvent.Kind::Unknown` round-trips its payload.** macOS writes nothing into the kind container for `.unknown` (`CommentaryEvent.swift:74-80`), emitting `{"recordTime": x, "kind": {}}` — so the event persists forever as an empty-kind record and only the *payload* is lost. Model it as `Unknown(serde_json::Value)` preserving the original verbatim. That is simpler than the hand-written Swift encoder, actually lossless, and removes the "old build silently destroys a new build's strokes" hazard.

---

## Phasing

Twelve phases in four milestones. Each gets its own plan document and follows the spec → review → plan → review → execute → review loop in `CLAUDE.md`.

**Milestone A — Foundations**

- **Phase 0. Workspace skeleton and conventions.** Four crates, CI (`cargo test` Linux; `cargo check` Windows, advisory), project format v2 read/write with the `formatVersion < 7` guard, `cumulativeOffset`/`absSeconds`/`Tag.normalize`, temp-project fixture, **and a `CLAUDE.md` Rust section**. No media, no UI.
- **Phase 1. Contract logic port.** `PlaybackTimeline`, `CompilationPlan`, `Zoom`, `ClipZoomLookup`, `StrokeReplay`, `SkipCoordinator` plus their tests (~594 LOC source, ~1,100 LOC tests). Headless. Establishes the Swift→Rust idioms reused by every later port.

**Milestone B — Scan and tag**

- **Phase 2. Source playback, transport, and project management.** `SourcePlayer`, Slint window, frame delivery, transport bar, scrubber, skip, keyboard, audio. **Plus:** create/open project (distinguishing "empty folder ⇒ create" from "unreadable `project.json` ⇒ refuse, do not overwrite", `Workspace.swift:163-185`), File menu, recents, add/remove/reorder sources with a `gst_discoverer` duration probe and the **aspect-match gate** (`aspectsMatch`, `:290-293`, 0.5% tolerance), **multi-source virtual-concat playback**, `sourceIndex` remap on delete (`:308-320`) and reorder (`:323-341`), missing-source relink. **Zoom rendering and the zoom gesture** (cursor anchoring, snap, clamping) land here — Phase 6 assumes the rendering exists. The bus contract is realised here.
- **Phase 3. Clips, tagging and undo.** Clip sidebar and inspector, tag field, tag overview and filter (`TagAggregation`), jump-to-clip, sort order, **`UndoController` + the `recordings/.trash` lifecycle** (move-on-delete, single-trashed-file eviction, restore, shred-on-open). `pushDelete` returns the evicted `DeletedClip` so the caller can shred its file (`UndoController.swift:88-98`, `:127-136`) — both halves of that contract land together. Ships against a fixture project: `Clip.recordingFilename` is non-optional (`Project.swift:58`), so a clip *is* a recording and the first user-created clip arrives in Phase 4.

**Milestone C — Record and review**

- **Phase 4. Capture.** PipeWire enumeration and selection, recording to `.mkv`, level meter, the `RecordingController` event log with its monotonic-clock guarantee, injected-clock testability, and **caller-captured play/pause timestamps**. `t0Seconds` is the running-time of the first buffer that reaches the muxer, and `recordingDuration` is read back from the finished file (`CaptureSessionController.swift:459-468`), not from wall clock — with a fallback for a `.mkv` reporting unknown duration after a crash.
- **Phase 5. Passthrough export.** One MP4 per clip from `playbackSegments`: decode the source range, encode, mux. No overlays. Encoder probe and fallback chain. **First output artifact, and the first validation of `PlaybackTimeline` against a real decoder** rather than a unit test. Under the hybrid architecture this graph *is* the full export graph minus the overlay branch — a skeleton, not a throwaway.
- **Phase 6. Drawing and zoom keyframes during recording.** Stroke capture overlay; zoom *keyframe emission* including the 100 ms anchor keyframe. (Zoom rendering and gesture are Phase 2.)
- **Phase 7. Clip preview.** Commentary + PiP + strokes + zoom composited live at preview resolution. First use of the shared compositor.

**Milestone D — Ship**

- **Phase 8. Full export.** Attach the overlay branch to the Phase 5 graph at full resolution; audio mixer and ramps; `ExportProgress` + `RunProjection`; **per-tag and all-clips targets** — the all-clips row is checked by default (`ExportSheet.swift:113`). macOS distinguishes them by a sentinel string `"__all-clips__"` compared in five places; in Rust this is `enum ExportTarget { AllClips, Tag(String) }`. Sequential export (macOS serialized because VideoToolbox saturates; on Linux the binding constraint is the overlay rasterizer and a single hardware encode session — same conclusion).
- **Phase 9. Scoreboard.** `ScoreboardState`/`MatchInterpret`/`MatchFormat` port, match inspector, team config, format editor, start/stop and goal tagging on the virtual timeline, back-anchor, overlay in preview and export.
- **Phase 10. Transcription.** `TranscriptionCoordinator` with the summarize half removed — its `Phase` enum collapses to one case and is deleted along with `currentPhase`, and `TranscriptionState` loses `.summarizing`. Its serial-queue semantics (one job in flight, FIFO behind it, idempotent enqueue) are the part worth keeping. `whisper-rs` behind `feature = "whisper"`, 16 kHz mono extraction, model acquisition UX, transcript editing. The `applyAIWrite` "saves but never pushes undo" contract carries over unchanged. **`ClipIntelligence` does not survive** — it exists solely because `Speech`/`FoundationModels` don't link in headless `swift test` (`ClipIntelligence.swift:3-8`); in Rust that is a Cargo feature, and with `summarize` gone it is a one-method trait. `TranscriptionWorkspace` likewise disappears. *Obsoletes BACKLOG #16.*
- **Phase 11. Packaging and Windows.**

---

## Test strategy

- **Pure logic:** 26 of 42 Swift test files (~2,845 of 6,692 LOC, **43%** — not "the bulk") translate directly to `#[test]`. One exception: `PlaybackTimelineTests.test_segments_subMillisecondEventGap_roundsToZeroCMTime` asserts that 0.5 ms rounds to zero ticks at timescale 600. GStreamer's nanosecond timebase has no such rounding — port it as "sub-millisecond segments are produced and callers must skip degenerate durations" and drop the `CMTime` assertion. `PlaybackTimelineTests` then needs no media import at all.
- **Media integration:** the other 16 files (~3,847 LOC) are a **rewrite, not a port** — `ClipPlaybackAccuracyTests` (672), `CompilationExporterE2ETests` (463), `CompilationCompositorZoomTests` (417), `CompilationExporterTests` (350), plus **954 LOC of fixture machinery** (`SyntheticAsset` 434, `FiducialAsset` 371, `SplitColorAsset` 149) that must be rebuilt on `videotestsrc` before any media test can be written. Budget the fixture rebuild as its own task.
- **Compositor:** property assertions first, golden frames only where geometry is the thing under test. The Swift suite proves this works — `ScoreboardRenderTests` asserts "drew inside the bar rect, left the outside untouched" and never compares an image; **the existing suite has zero golden-image tests.** Where a golden is genuinely clearest, three rules make it sound:
  1. **The overlay font is bundled in `video-coach-core` and loaded into a private `fontdb` by bytes.** System fonts are never consulted. `ScoreboardDraw.fittingFontSize` picks a per-team-name font *size* from measured width (`:50-52`, `:89`), so a different resolved font changes **layout**, not just antialiasing. This also converts the font-metrics risk from a per-machine variable into a one-time cost.
  2. **Golden frames composite overlays over a synthetic solid base, never over decoded video.** VA-API vs software decode and YUV→RGB matrix/range differences make decoded pixels machine-dependent.
  3. **Decoder and encoder ranks are pinned in tests** via `GST_PLUGIN_FEATURE_RANK`.
- **Harness:** `video-coach-harness` drives the bus headlessly and asserts on emitted events and on-disk state.

---

## Gates and risks

**Phase 2 gate — zero-copy decode-to-display. PASSED on the reference laptop, 2026-09-19** (i7-10610U, Intel UHD GT2, Ubuntu 24.04, GStreamer 1.24.2; `scripts/linux-gate-check.sh`). Hardware decode selected by default; frames reach `glupload` as `memory:DMABuf`; accurate seek through the GL path **10 / 22 ms** (median / worst) on the user's camera footage (HEVC 1440p30, 0.5 s GOP) and **92 / 149 ms** on a 2 s-GOP H.264 1080p60 recording. Re-run the script on any new target machine; it measures the display path, not a system-memory sink.

**Zero-copy is a seek-latency requirement, not only a throughput one.** An accurate seek decodes forward up to one GOP from the previous keyframe; if the decoder's output is system memory, every one of those frames is copied off the GPU even though all but the last are discarded. On the 2 s-GOP file that copy was 80% of the cost (447 → 92 ms median). Any path that lets decoded frames fall back to system memory — `decodebin`, a stray `videoconvert`, a sink without GL — regresses scrubbing, and it is invisible in a playback test.

**Phase 7 gate — first composite.** End-to-end preview at playback resolution sustains output fps with zoom active and a stroke-heavy overlay.

**There is no compositing-throughput gate.** It has been run; it failed; that is *why* the architecture is what it is. Do not re-run it.

1. **Scrub responsiveness — resolved in GStreamer's favour; the libmpv kill criterion is retired.** Measured on real hardware and real footage (Phase 2 gate above), GStreamer passes with a ~12× margin on the user's footage. The criterion also pointed at the wrong variable: when a seek is slow *with* hardware decode and zero-copy, the cost is decoding one GOP of frames, and libmpv with `hwdec=vaapi` would use the same decoder and decode the same frames. What actually decides accurate-seek cost is **GOP length × per-frame decode cost × whether output stays on the GPU.** Residual edge: 4K with a 2 s GOP on a 15 W iGPU measured 191 / 336 ms, so its worst case exceeds 250 ms; KEY_UNIT during drag (37 ms median there) covers live scrubbing, and only the settle-on-release pays the accurate cost. One property mpv gave macOS still has to be matched: a **synchronous, frame-accurate position query** for the play/pause anchors (`ContentView.swift:753-773`). GStreamer's `query_position` is synchronous; Phase 4 must verify it is frame-accurate after a seek.
2. **GL interop across drivers.** DMABuf/VA import into GL is solid on Mesa and dicier on the NVIDIA proprietary stack. Mitigation: the software `compositor` fallback, which is already built for CI.
3. **Font metrics.** cosmic-text will not reproduce CoreText's metrics, and because `fittingFontSize` derives a font *size* from measured width, a different font changes layout, not just pixels. Bundling the font (above) bounds this to a one-time tuning cost.
4. **Software-encode fallback on low-end hardware.** A machine with no VA-API or NVENC H.264 encoder falls back to `x264enc` and exports substantially slower than the macOS VideoToolbox path. Less likely than the HEVC version of this risk (H.264 encode is near-universal) but not zero. State it in the README.
5. **PipeWire device enumeration and permissions.** Portal-mediated camera access differs across Wayland compositors. Test on GNOME and KDE.
6. **whisper.cpp model distribution.** See Open questions.
7. **Slint at this UI complexity.** `ContentView` is 1,348 lines and `ExportSheet` 834. Phase 3 is the first real test; if the inspector-heavy UI fights the toolkit, that is the moment to reconsider — not Phase 9.

**Windows CI is advisory: a red Windows build does not veto a dependency that is right for Linux.** If a Linux-optimal choice breaks Windows, Windows is what gets solved (or dropped), in Phase 11. The tiny-skia + cosmic-text rationale rests on Linux merits: a pure-Rust rasterizer is one static blob, where bundling cairo + pango + harfbuzz + fontconfig into a relocatable package means matching ABI and font-config paths across distros — the most common source of "works on my distro" packaging failures. Strokes are literally Skia paths. Reproducible rasterization makes the overlay tests meaningful.

---

## Open questions — deferred to human

1. **Chrome coordinate space.** Should the text bar, PiP and scoreboard lay out in **output** space (overlapping the letterbox bars, reading like broadcast furniture) or **content** space (staying inside the picture)? Purely cosmetic, only differs for non-16:9 sources, and there is no existing evidence either way because the macOS stretch collapses the two. *Recommendation: output space.* Only the three chrome layers change; the stroke rule is unaffected either way.
2. **`Project` ownership.** Does the bus own `Project` (all mutations are commands), or does the UI own it and the bus own media only? *Recommendation: bus owns `Project` and the media objects; every mutation is a `Command`; the bus emits `Event::ProjectChanged(Arc<Project>)` and the UI derives Slint properties from the latest snapshot.* It matches what the superseded plan already assumed, makes undo unambiguously bus-side, and `Arc` snapshots avoid both locking and partial-update bugs. Cost is a full property re-derive per mutation, which is nothing at this project size. Settle it in the Phase 2 plan, after a Slint property-model prototype.
3. **whisper model distribution.** Bundle (~140 MB), download on first run, or require a user-supplied path? Note this does **not** newly break an offline guarantee — the macOS app already downloads a speech model inside `transcribe()` on first use (BACKLOG #19), so the README's "no network calls" claim is already inaccurate. "No FFmpeg" also becomes false once `gst-libav` ships for software decode of arbitrary match film. Both lines need correcting regardless. *Recommendation: download on first run with an explicit prompt and visible progress — strictly better than the macOS behavior BACKLOG #19 complains about — and reword to "runs entirely on your machine; one-time model download on first transcription."*
4. **Linux packaging target.** Flatpak sandboxing complicates camera, microphone and arbitrary-path file access, all three of which this app needs. *Recommendation: AppImage first.*
5. **Wayland vs X11 for the drawing overlay.** Freehand telestration wants low input latency. Worth a spike in Phase 6.
6. **Fate of the `apple/` tree.** Keep as reference until parity, then delete? Can wait until Milestone D.

---

## Known macOS bugs this port fixes

Recorded because they are easy to re-introduce by faithfully porting:

- **Scoreboard clock on export** ignores pauses and skips (`CompilationCompositor.swift:253`). Fixed by the source-time golden rule.
- **Non-16:9 sources are anamorphically distorted** at fixed export resolutions (`:130-134` + `ExportSettings.swift:20-23`). Fixed by letterboxing.
- **Preview ignores `showPiP`** while export honors it (`PreviewCompositor.swift:163` vs `:101`). Fixed by one shared compositor.
- **The Quality picker does nothing** (`CompilationExporter.swift:45-50`, `:508-518`). Fixed by implementing it for the first time.
- **Unknown commentary events persist as empty-kind records** and lose their payload (`CommentaryEvent.swift:74-80`). Fixed by round-tripping the raw value.

None are fixed in the Swift tree — it is the reference implementation and is not maintained in parallel. If that changes, the scoreboard fix is small: pass `clip` instead of `clipStartAbsSeconds` into `CompilationInstruction` and call the preview formula.

## What this spec does not settle

Effort. Phase 1 is the calibration point: well-understood work with a known test suite, so how long it actually takes is the best available predictor for everything after it. Note that Phase 1 is now roughly half its original size, which halves that calibration sample — a deliberate trade against porting 616 LOC of scoreboard tests that nothing consumes for eight phases.
