# Linux Port — Design

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Supersedes:** `rust/docs/plans/2026-04-30-rust-rewrite-phase-7-source-transport.md` (docs-only; no code was ever committed)

---

## Goal

Coach Cuts runs natively on Linux, with the same workflow it has on macOS: scan match film, tag moments, record webcam + mic commentary with synchronized freehand telestration, and export one clip per tag with scoreboard, PiP, drawings and zoom burned in.

The macOS app is not being maintained in parallel. This is a replacement, not a second front-end.

## Locked decisions

| Decision | Choice | Consequence |
|---|---|---|
| Primary platform | **Linux, native** | PipeWire capture, VA-API/NVENC encode, AppImage or Flatpak. Windows stays compiling in CI but is not a release target until Phase 10. |
| Language + stack | **Rust + GStreamer + Slint** | One media dependency covers decode, encode, capture and mux on both platforms. |
| Transcription | **whisper.cpp, no summarization** | Keeps the offline guarantee. The `summarize` half of the old `ClipIntelligence` seam is dropped, not stubbed. |
| Project format | **Clean slate (v2 lineage)** | No migration from Swift-era `project.json` v1–v6. Reader hard-errors on legacy files instead of misreading them. |

## Non-goals

- macOS support. The Swift tree stays in the repo as the reference implementation and is not deleted until the port reaches feature parity, but it is not ported back to.
- Summarization of commentary transcripts.
- Any WSL target. Webcam and microphone capture through WSL2 is unreliable, and this app is capture-heavy. Native Linux or nothing.
- **Android.** Not a target, but the stack does not foreclose it: Slint and GStreamer both run on Android, and `video-coach-core` is plain Rust with no platform dependency. What would not port is the workflow — scrub-and-tag with keyboard shortcuts, a webcam PiP recorded over a desktop player, and multi-hour source files on device storage are a different product, not a rebuild of this one. The realistic Android shape is a companion viewer for exported clips. Revisit after Milestone D, if at all.
- Multi-camera, cloud sync, team sharing, or anything else the macOS app does not already do.

---

## Starting state

The `rust/` directory contains one plan document and no code. It references "Phase 5's `compose.rs`" and "Phase 6's File-menu wiring" as completed work, but nothing was committed — `find rust -type f` returns a single `.md`. **The port starts from zero Rust.** Its architecture sketch is still the best starting point and this spec adopts it, but its phase numbering is abandoned.

What exists to port from:

| Area | LOC | Disposition |
|---|---|---|
| `VideoCoachCore` pure logic (Foundation-only) | ~1,950 | **Translate.** Semantics preserved exactly; this is the load-bearing IP. |
| `VideoCoachCore` media-bound | ~1,500 | **Rebuild** on GStreamer + tiny-skia. |
| `apple/App` (SwiftUI + AppKit) | ~8,625 | **Rebuild** in Slint. |
| `VideoCoachCore` tests | ~6,692 | **Port the ~27 pure-logic files**; rewrite the 15 AVFoundation-bound ones against GStreamer fixtures. |

---

## Architecture

### Crate layout

```
crates/
  video-coach-core/     pure logic, zero media deps, no I/O beyond serde
  video-coach-media/    GStreamer: source player, capture, compositor, export
  video-coach-app/      Slint UI, command bus, event layer
```

`video-coach-core` must stay buildable and testable with no GStreamer on the machine. That boundary is what made the Swift core portable in the first place and it is the single most important structural rule in this port.

### Command bus

Adopted from the prior plan: the UI dispatches serde-serializable `Command` values onto an async bus; the bus task owns the media objects and emits `Event` values back. This keeps Slint's single-threaded event loop away from GStreamer's threading, and makes headless harness tests possible — a test drives the bus directly with no window.

### Media pipelines

**Source playback (scan).**

```
filesrc → decodebin → tee ─┬─ videoconvert → RGBA capsfilter → appsink   → UI frame
                           └─ audioconvert → audioresample → volume → autoaudiosink
```

Hybrid seek policy, carried over from the prior plan: `ACCURATE` for skip buttons and keyboard shortcuts, `KEY_UNIT` during live scrubber drag, `ACCURATE` on release.

**Capture (commentary recording).**

```
pipewiresrc (camera) → videoconvert → encoder → ─┐
                                                 ├→ matroskamux → filesink  (recordings/<uuid>.mkv)
pipewiresrc (mic)    → audioconvert → opusenc  → ─┘
```

`v4l2src` + `pulsesrc` as the fallback path when PipeWire is absent. Matroska rather than MP4 because a crash mid-record leaves a playable file.

**Export.**

```
source decode  → appsink ─┐
                          ├→ [Rust compositor] → appsrc → encoder → mp4mux → filesink
webcam decode  → appsink ─┘
```

The composite happens in Rust, not in a GStreamer element graph. See below.

### The compositor decision

The macOS compositor runs two stages per output frame: a Core Image pass that composes base video + zoom transform + PiP, then a Core Graphics pass that draws strokes, the text bar and the scoreboard over the result. That two-stage shape is worth keeping — it is already the right decomposition.

**Chosen mapping: `appsink` → compose in Rust with `tiny-skia` + `cosmic-text` → `appsrc`.**

Rejected alternative: wiring GStreamer's own `compositor` + `cairooverlay` elements with `GstControlBinding` animating the zoom properties. That is more idiomatic GStreamer, but per-frame parametric control of a live element graph is the hardest thing to get right in GStreamer, and zoom here is a dense keyframe track (the recorder emits up to ~60 zoom events/second during a pinch). Driving that through control bindings trades a tractable problem for an intractable one.

Why `tiny-skia` + `cosmic-text` over `cairo` + `pango`:

- Pure Rust, no system C libraries beyond GStreamer itself. Windows packaging stays simple.
- Deterministic rasterization across platforms, which makes golden-frame tests meaningful on both.
- `tiny-skia` is a Skia path-rendering port; strokes are literally paths, so the mapping from `CGContext` stroke drawing is near-mechanical.

Cost, stated plainly: this is **CPU compositing**, where macOS used a GPU Core Image pipeline. At 1080p30 that is a real throughput question, not a theoretical one. Phase 7 measures it before building on it, and `glvideomixer` plus a GL shader zoom is the documented escape hatch if the number is bad.

### Encoder selection

HEVC availability on Linux varies by GPU, driver and distro packaging. The export path probes at runtime and picks the first available:

1. `vaapih265enc` / `vah265enc` (Intel, AMD)
2. `nvh265enc` (NVIDIA)
3. `x265enc` (software, `preset=medium`)

If none can be constructed, fall back to the same chain for H.264 and tell the user in the export sheet which encoder was selected. Bitrates carry over unchanged from `ExportSettings.swift`: 6/12/24 Mbps at 1080p for low/medium/high, halved at 720p.

Note that quality-at-bitrate differs meaningfully between these encoders, so the three quality presets will not look identical across machines. That is accepted; the alternative is per-encoder tuning tables, which is not worth it.

---

## Logic to port verbatim

These carry semantics that were expensive to get right and must not be re-derived. Each gets its Swift test file ported alongside it.

**`PlaybackTimeline` — `sourceTime(atRecordTime:)` and `playbackSegments(sourceDuration:)`.** The event-log walk that turns a commentary recording into play/freeze segments. Three subtleties that must survive:
- `.play`/`.pause` carry a captured `sourceTime` anchor that *overrides* the wall-clock cursor, because player latency makes the computed cursor drift by tens of milliseconds.
- Playing past EOF splits into a `.play` tail plus a `.freeze`, mirroring what the player shows on screen.
- `freezeMaxSource = sourceDuration - 0.05` pulls out-of-bounds freeze anchors back inside the source. On macOS an out-of-range slice stalled the compositor; the GStreamer equivalent will differ, but the clamp is correct regardless.
- Only `.play`/`.pause`/`.skip` split segments. Zoom and stroke events must **not**, or a pinch gesture explodes the segment count.

**`ScoreboardState.scoreboardState(absoluteTime:config:events:)`.** Derives period, clock, stoppage, break and fulltime from tagged start/stop events by position, with no sport-specific hard-coding. Includes the P1 back-anchor offset for footage that missed kickoff, and the goal-counting window bounded by first start and final whistle.

**`MatchInterpret.interpret(_:format:)`.** Positional assignment of start/stop events to period roles, stable-sorted with input-order tie-break. `setAutoBackAnchorP1` depends on inserting at index 0 to win that tie-break — a detail that looks arbitrary and is not.

**`Zoom`.** Clamping (hard floor 1.0, cap 10.0, pan limit narrowing as scale → 1), cursor-anchored zoom, snap notches at 3% tolerance, and the transform math. The macOS code carries two transform variants because Core Image uses bottom-left origin; in Rust there is **one** top-left-origin transform and the second variant is deleted. This is a genuine simplification the port should bank.

**`StrokeReplay.visibleStrokes(in:atRecordTime:)`.** Which strokes are visible at a given record time, honoring `autoClearAfterSeconds` and intervening `clearAll` events, and how many points of each are drawn so far.

**Also ported:** `CompilationPlan`, `TagAggregation`, `SkipCoordinator`, `UndoController`, `ExportProgress` (including `RunProjection` ETA math), `MatchFormat`, `ClipZoomLookup`, `Tag.normalize`.

---

## Project format v2

Clean slate. Folder layout is unchanged — `project.json` plus a `recordings/` subdirectory — because that part was right.

Changes from the Swift v6 schema:

| Field | v6 | v2 | Why |
|---|---|---|---|
| `SourceRef.bookmark` | `Data` (security-scoped bookmark) | `relativePath: String` | Bookmarks are a macOS concept. Path is relative to the project folder, may traverse `..`, breaks if the user moves the file. |
| `recordingFilename` | `<uuid>.mov` | `<uuid>.mkv` | Matroska for crash resilience. |
| `Clip.summary` | `String` | *removed* | No summarizer ships. A field nothing writes is cruft; re-add it additively if summarization ever lands. |
| `preferredCameraID` / `preferredMicID` | `AVCaptureDevice.uniqueID` | PipeWire node name or `/dev/v4l/by-id` path | Same "hint, fall back to default, do not clear" semantics. |
| `formatVersion` | 6 | 1 | New lineage. |

`Clip.transcript` stays, and stays user-editable.

**Legacy guard.** A `project.json` containing `sourceVideos[].bookmark` is a Swift-era file. The reader detects that key and fails with an explicit "this project was made by the macOS version and cannot be opened" error rather than decoding it into something subtly wrong. Roughly three lines; worth it.

`CommentaryEvent.Kind` keeps its unknown-variant tolerance — decoding an unrecognized event kind yields `Unknown` and is dropped on save, so a future event type does not brick an older build.

---

## Phasing

Four milestones. Each phase gets its own plan document and follows the normal spec → review → plan → review → execute → review loop from `CLAUDE.md`.

### Milestone A — Foundations

**Phase 0. Workspace skeleton.** Three crates, CI running `cargo test` on Linux and `cargo check` on Windows, project format v2 read/write with the legacy guard, and a temp-project test fixture. No media, no UI.

**Phase 1. Pure-logic port.** All of "Logic to port verbatim" above, with the ~27 portable Swift test files translated. Headless. **This is the phase that de-risks everything downstream** — if the clock semantics and segment builder are right and tested, the rest is plumbing. It is also the phase most likely to be under-estimated because the code is small and the invariants are not.

### Milestone B — Scan and tag

**Phase 2. Source playback + transport.** GStreamer `SourcePlayer`, Slint window, frame sink, transport bar, scrubber, skip buttons, keyboard shortcuts, audio with volume. Essentially the old Phase 7 scope, rebuilt on Phase 0–1 foundations.

**Phase 3. Clips and tagging.** Clip sidebar, clip inspector, tag field with normalization, tag overview and filter, jump-to-clip shortcuts, sort ordering, undo.

### Milestone C — Record and review

**Phase 4. Capture.** PipeWire camera + mic enumeration and selection, recording pipeline to `.mkv`, input level meter, the `RecordingController` event log with its monotonic-clock guarantee and injected-clock testability.

**Phase 5. Drawing and zoom during recording.** Stroke capture overlay, zoom gesture with cursor anchoring and snap, zoom keyframe emission including the 100ms-gap anchor keyframe.

**Phase 6. Clip preview.** Play back a recorded clip with its commentary, PiP, strokes and zoom composited live. First use of the Rust compositor, at preview resolution.

### Milestone D — Ship

**Phase 7. Export.** The compositor at full resolution, encoder probe and fallback chain, per-tag MP4 output, progress and ETA reporting, sequential export. **Gate: measure CPU compositing throughput at 1080p30 before building the rest of the phase on it.**

**Phase 8. Scoreboard.** Match inspector panel, team configuration, match format editor, start/stop and goal tagging, back-anchor checkbox, scoreboard overlay in preview and export.

**Phase 9. Transcription.** `whisper-rs` behind the existing `ClipIntelligence`-shaped seam, audio extraction to 16 kHz mono, model acquisition UX, transcript display and editing.

**Phase 10. Packaging and Windows.** AppImage or Flatpak for Linux; Windows capture backend (Media Foundation via `mfvideosrc`), encoder probe for NVENC/QSV, and an installer.

---

## Test strategy

- **Pure logic:** direct translation of the Swift XCTest files to `#[test]`. This is the bulk of the existing suite and it ports cleanly.
- **Media integration:** GStreamer `videotestsrc`-generated fixtures replacing the Swift `SyntheticAsset` / `SplitColorAsset` / `FiducialAsset` helpers. Same idea — synthesize an asset with known pixel values at known times, then assert on output pixels.
- **Compositor:** golden-frame tests. `tiny-skia` rasterizes deterministically, so a golden PNG is valid on both Linux and Windows. Port `PixelSampling` as the comparison helper.
- **Harness:** drive the command bus headlessly, assert on emitted events and on-disk state. No window required.
- **Feature gating:** `video-coach-core` tests must run with `--no-default-features` and no GStreamer present. CI enforces this.

---

## Risks

1. **Scrub responsiveness under GStreamer.** The macOS app migrated its scan player to mpv specifically because the platform framework's seek behavior was unacceptable on this footage. GStreamer is a different stack and should be fine, but this is unproven for this workload. *Kill criterion: if accurate seek on 4K HEVC exceeds ~250 ms in Phase 2, switch the scan player to libmpv and keep GStreamer for export.* libmpv is cross-platform, so this fallback costs a second decoder stack but not the port.
2. **CPU compositing throughput.** Measured at the Phase 7 gate. Escape hatch is `glvideomixer` plus a shader zoom.
3. **Font metrics.** `cosmic-text` will not reproduce CoreText's metrics, so every scoreboard and text-bar layout constant needs re-tuning against rendered output. Expect this to be fiddly and budget for it rather than discovering it late.
4. **HEVC encoder availability.** Highly variable on Linux. Mitigated by the probe chain, but a user on a machine with only software x265 will find exports much slower than the macOS app's VideoToolbox path. This is a real product regression on low-end hardware and should be stated in the README.
5. **PipeWire device enumeration and permissions.** Portal-mediated camera access on Wayland differs from X11 and across desktop environments. Phase 4 should test on at least one GNOME and one KDE system.
6. **whisper.cpp model distribution.** A `ggml-base.en` model is ~140 MB. Bundling it bloats the package; downloading it on first run breaks the "no network calls" claim in the README. Needs a decision — see Open questions.
7. **Slint at this UI complexity.** The macOS `ContentView` is 1,348 lines and `ExportSheet` is 834. Slint is young and its widget set is thinner than SwiftUI's. Phase 3 is the first real test; if the inspector-heavy UI fights the toolkit, that is the moment to reconsider, not Phase 8.

---

## Open questions — deferred to human

1. **whisper model distribution.** Bundle (~140 MB package), download on first run (breaks the offline claim at install time only), or require the user to supply a path? Recommendation: download on first run with an explicit prompt, and reword the README's "no network calls" to "no network calls during normal use; one-time model download on first transcription."
2. **Linux packaging target.** Flatpak sandboxing complicates camera, microphone and arbitrary-path file access — all three of which this app needs. AppImage avoids that at the cost of a worse update story. Recommendation: AppImage first, Flatpak later if it proves tractable.
3. **Wayland vs X11 for the drawing overlay.** Freehand telestration wants low input latency. Worth an early spike in Phase 5 rather than an assumption now.
4. **Fate of the `apple/` tree.** Keep as reference until parity, then delete? Or keep indefinitely as the macOS build? Deleting is the honest choice if nobody runs it, but that decision can wait until Milestone D.

---

## What this spec does not settle

Effort. The rebuild is roughly 10,000 lines of macOS-bound Swift replaced by Rust and Slint, plus ~1,950 lines translated. Rust UI code tends to run longer than SwiftUI for the same screen, so the Slint side will likely exceed the 8,625 lines it replaces. Any number past that is a guess, and the phase plans are where it becomes real. Phase 1 is a good calibration point: it is well-understood work with a known test suite, so how long it actually takes is the best available predictor for everything after it.
