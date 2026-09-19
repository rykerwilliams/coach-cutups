# Linux Port — Phases 0 and 1

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md`
**Status:** Draft, pre-review

**Goal:** A Rust workspace whose `video-coach-core` crate holds the project format and the contract logic the media layer must satisfy, fully tested, with no GStreamer on the machine.

**Scope:** Phase 0 (workspace, format, conventions) and Phase 1 (contract logic port). No media, no UI, no bus. Everything here builds and tests with `cargo test -p video-coach-core` on a machine with no GStreamer installed — which is how the core-isolation rule is enforced.

**Why these two together:** Phase 1's modules are meaningless without the types Phase 0 defines (a `Clip` with an event log), and Phase 0's format is untestable without something that reads it. They are one reviewable unit.

---

## Layout

```
Cargo.toml                          workspace root
crates/
  video-coach-core/
    Cargo.toml                      serde, serde_json, uuid, thiserror. NO media deps.
    src/
      lib.rs
      project.rs                    Project, Clip, SourceRef, Preferences, Resolution, Quality
      event.rs                      CommentaryEvent, EventKind
      stroke.rs                     Rgba, StrokePoint, Stroke
      zoom.rs                       Zoom (data + behavior)
      scoreboard_config.rs          TeamConfig, ScoreboardConfig, MatchFormat, MatchEventRecord
      store.rs                      read/write + version guard
      tag.rs                        normalize
      timeline.rs                   PlaybackSegment, source_time, playback_segments
      zoom_lookup.rs                zoom_at (lerp)
      stroke_replay.rs              visible_strokes, VisibleStroke
      plan.rs                       CompilationPlan, Entry
      skip.rs                       SkipCoordinator
  video-coach-media/                stub only in Phase 0-1 (lib.rs + a doc comment)
  video-coach-app/                  stub only
  video-coach-harness/              stub only
```

Media/app/harness crates exist from Phase 0 so the workspace shape is fixed and CI wires up once, but carry no code until Phase 2.

**Rust edition 2021, `rust-version = "1.90"`** (well under the toolchain in use; avoids a floor nobody can meet).

---

## Phase 0

### Task 0.1 — Workspace skeleton and CI

**Files:** `Cargo.toml`, `crates/*/Cargo.toml`, `crates/*/src/lib.rs`, `.github/workflows/rust.yml`

1. Workspace root `Cargo.toml` with `members = ["crates/*"]` and a `[workspace.package]` block (version, edition, license = `"AGPL-3.0-or-later"`, rust-version).
2. `video-coach-core/Cargo.toml` depends on exactly: `serde` (derive), `serde_json`, `uuid` (v4, serde), `thiserror`. **Nothing else.** No image crate, no font crate, no GStreamer, no feature that pulls one in.
3. The other three crates get a `lib.rs` containing only a module doc comment stating what the crate will hold and that it is empty until Phase 2.
4. CI workflow:
   - `core` job: `cargo test -p video-coach-core` on `ubuntu-latest` **with no GStreamer installed**. This is the core-isolation enforcement — if a media dependency is ever added, this job fails to build.
   - `workspace` job: `cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`.
   - `windows` job: `cargo check -p video-coach-core` only, marked `continue-on-error: true`. Advisory, per the spec — a red Windows build does not veto a Linux-right dependency, and the media crate would need GStreamer dev libraries on the runner to typecheck at all, which is not worth setting up for a non-target.

**Verify:** `cargo build --workspace && cargo test --workspace` green. Commit.

### Task 0.2 — Data model

**Files:** `project.rs`, `event.rs`, `stroke.rs`, `scoreboard_config.rs`

Direct translation of the Swift types, with the v2 changes from the spec. Types only — behavior lands in later tasks.

```rust
// stroke.rs
pub struct Rgba { pub r: f64, pub g: f64, pub b: f64, pub a: f64 }
impl Rgba { pub const RED: Rgba = Rgba { r: 1.0, g: 0.2, b: 0.2, a: 1.0 }; }
pub struct StrokePoint { pub x: f64, pub y: f64, pub t: f64 }  // x,y normalized TOP-LEFT; t = s since stroke start
pub struct Stroke {
    pub id: Uuid, pub color: Rgba,
    pub line_width: f64,                        // normalized to frame HEIGHT
    pub points: Vec<StrokePoint>,
    pub auto_clear_after_seconds: Option<f64>,  // None = persist
}

// zoom.rs (data; behavior in Task 1.2)
pub struct Zoom { pub scale: f64, pub pan_x: f64, pub pan_y: f64 }

// event.rs
pub struct CommentaryEvent { pub record_time: f64, pub kind: EventKind }
pub enum EventKind {
    Play { source_time: f64 },
    Pause { source_time: f64 },
    Skip { delta: f64 },
    Stroke(Stroke),
    ClearAll,
    Zoom(Zoom),
    Unknown(serde_json::Value),   // round-trips the original payload verbatim
}
```

**`EventKind` serialization must match the Swift single-key-object shape** — `{"play": {"sourceTime": 1.5}}`, `{"clearAll": {}}` — because the v2 format keeps that encoding even though it drops v1–v6 compatibility. Hand-write `Serialize`/`Deserialize` (serde's externally-tagged enum is close but `ClearAll` must emit `{}`, not `null`). `Unknown(Value)` captures any unrecognized single-key object and re-emits it unchanged.

```rust
// project.rs
pub enum Resolution { R720, R1080, R2160 }     // NOTE: `source` is gone (spec)
pub enum Quality { Low, Medium, High }
pub struct Preferences {
    pub scan_volume: f64,                       // default 1.0
    pub preview_source_volume: f64,             // default 1.0
    pub preview_commentary_volume: f64,         // default 1.0
    pub last_export_resolution: Resolution,     // default R1080
    pub last_export_quality: Quality,           // default Medium
    pub preferred_camera_id: Option<String>,    // PipeWire node name or /dev/v4l/by-id path
    pub preferred_mic_id: Option<String>,
    pub pip_for_new_recordings: bool,           // default true
}
pub struct SourceRef {
    pub relative_path: String,                  // POSIX '/', may traverse '..'
    pub display_name: String,
    pub duration_seconds: f64,
}
pub struct Clip {
    pub id: Uuid, pub name: String, pub notes: String, pub tags: Vec<String>,
    pub source_index: usize,
    pub start_source_seconds: f64,
    pub recording_duration: f64,
    pub recording_filename: String,             // "<uuid>.mkv"
    pub events: Vec<CommentaryEvent>,
    pub show_pip: bool,
    pub sort_index: i64,
    pub created_at: DateTime<Utc>,              // RFC3339 in JSON
    pub transcript: String,                     // `summary` is gone (spec)
}
pub struct Project {
    pub format_version: u32,                    // 7
    pub name: String,
    pub source_videos: Vec<SourceRef>,
    pub clips: Vec<Clip>,
    pub preferences: Preferences,
    pub scoreboard: Option<ScoreboardConfig>,
    pub match_events: Vec<MatchEventRecord>,
}
```

JSON field names stay **camelCase** (`#[serde(rename_all = "camelCase")]`) to match the existing format shape. Every field that has a Swift default gets `#[serde(default)]` so a hand-edited file missing an optional key still loads.

`scoreboard_config.rs` carries `TeamConfig` (name + primary/secondary/font `Rgba`, font defaulting to secondary), `ScoreboardConfig` (home, away, format), `MatchFormat` (regulation periods/seconds, overtime periods/seconds, with `total_periods`, `expected_start_stop_events = 2 * total_periods`, `is_overtime`, `period_seconds`, `period_name`, `break_label`) and `MatchEventRecord` (id, kind, source_index, source_seconds, is_auto_back_anchor). These are **data plus their trivial derived accessors only** — the positional interpretation logic is Phase 9.

`period_name` keeps the soccer special case: `"1H"`/`"2H"` when `regulation_periods == 2`, else `"P1"`/`"P2"`…, overtime always `"OT1"`… `break_label` returns `"HT"` for soccer's first break, `"BREAK"` otherwise.

**Tests:** round-trip every type through `serde_json`. Specifically pin: `ClearAll` emits `{"clearAll":{}}`; `Unknown` round-trips an unrecognized kind byte-for-byte; `Option<f64>` on `auto_clear_after_seconds` distinguishes absent from null.

**Verify:** `cargo test -p video-coach-core`. Commit.

### Task 0.3 — Project store and the version guard

**Files:** `store.rs`

```rust
pub const CURRENT_FORMAT_VERSION: u32 = 7;

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    #[error("this project was created by the macOS version of Coach Cuts (format v{found}) and cannot be opened; v{minimum} or later is required")]
    LegacyProject { found: u32, minimum: u32 },
    #[error("project format v{found} is newer than this build supports (v{supported})")]
    TooNew { found: u32, supported: u32 },
    #[error("project.json is unreadable: {0}")]
    Malformed(String),
    #[error(transparent)] Io(#[from] std::io::Error),
}

pub fn read(project_dir: &Path) -> Result<Project, StoreError>;
pub fn write(project_dir: &Path, project: &Project) -> Result<(), StoreError>;
```

Guard order matters — read `formatVersion` from the raw JSON **before** attempting full deserialization, so a Swift-era file produces `LegacyProject` rather than a confusing field-level decode error:

1. Parse to `serde_json::Value`.
2. `format_version` = `.formatVersion` as u32, **defaulting to 1 when the key is absent** (a file with no version predates the field). This is what makes the single comparison airtight: every Swift-era file, including v1 which has no key at all, lands below 7.
3. `< 7` → `LegacyProject`. `> 7` → `TooNew`. Else deserialize.

`write` serializes pretty-printed with a trailing newline, **to a temp file in the same directory then renames** — an interrupted save must not leave a truncated `project.json`, since that is the file the "unreadable ⇒ refuse, do not overwrite" rule in Phase 2 keys on.

**Tests:**
- round-trip a populated project through `write`/`read`
- `formatVersion: 6` → `LegacyProject`
- **no `formatVersion` key** → `LegacyProject { found: 1 }` (this is the case the rejected `bookmark`-sniffing guard would have missed)
- `formatVersion: 8` → `TooNew`
- truncated JSON → `Malformed`
- a fixture helper `temp_project()` returning a `TempDir` with a valid minimal project, used by every later test

**Verify:** `cargo test -p video-coach-core`. Commit.

### Task 0.4 — Virtual timeline and tag normalization

**Files:** `project.rs` (impl block), `tag.rs`

```rust
impl Project {
    pub fn total_source_duration(&self) -> f64;
    pub fn cumulative_offset(&self, source_index: usize) -> f64;   // sum of durations[0..i], clamped
    pub fn abs_seconds(&self, source_index: usize, source_seconds: f64) -> f64;
}
pub fn normalize_tags(input: &str) -> Vec<String>;
```

`cumulative_offset` clamps `source_index` to `[0, len]` and returns 0 for an empty source list. `normalize_tags` splits on `,`, trims whitespace, lowercases, drops empties, and de-duplicates **preserving first-seen order**.

These are Phase 0 rather than Phase 1 because the scoreboard clock runs on this virtual timeline and `MatchEventRecord.source_index` is a persisted field — the virtual timeline is a property of the format, not of the player.

**Tests:** port `ProjectTests` and `WorkspaceCumulativeTests` coverage — empty list, index past end, index 0, mid-list accumulation; tag normalization including duplicates, mixed case, empty fragments, and a single untagged string.

**Verify:** `cargo test -p video-coach-core`. Commit.

### Task 0.5 — CLAUDE.md Rust section

**Files:** `CLAUDE.md`

Every execution task in every later phase runs in a fresh subagent whose only project context is this file. It currently says tests run via `swift test --package-path apple/VideoCoachCore` and that new files need `xcodegen generate`, and describes an architecture the port replaces.

1. Re-scope the existing build/test and architecture sections under a heading **"Reference implementation (`apple/`, not maintained)"**.
2. Add a **"Rust port (primary)"** section: `cargo test -p video-coach-core` (no GStreamer needed), `cargo test --workspace` (needs GStreamer), `cargo clippy --workspace -- -D warnings`, the four-crate layout, the **core-has-no-media-dependency rule**, the bus contract's caller-captured-timestamp rule, and a pointer to the spec.
3. Leave the workflow, review-pattern and user-values sections untouched — they are stack-agnostic and still govern.

**Verify:** re-read the file as if seeing it fresh; a subagent must be able to run the right test command without guessing. Commit.

---

## Phase 1

Every module below is a direct translation with its invariants preserved. Where the spec records a rule, the test that pins it is named.

### Task 1.1 — `timeline.rs`

```rust
pub enum SegmentKind { Play, Freeze }
pub struct PlaybackSegment {
    pub kind: SegmentKind,
    pub source_start: f64,
    pub out_duration: f64,
}
pub fn source_time(clip: &Clip, at_record_time: f64, source_duration: f64) -> f64;
pub fn playback_segments(clip: &Clip, source_duration: f64) -> Vec<PlaybackSegment>;
```

**`source_time` takes `source_duration` and clamps** — the spec's correction to the macOS asymmetry, where `playbackSegments` clamped `.skip` but `sourceTime` did not. Both now clamp to `[0, source_duration]`, including the rate integration. Two functions answering "what source time is on screen" must not disagree past EOF.

Invariants:
- `Play`/`Pause` **anchor** the cursor to the carried `source_time`, overriding the wall-clock computation.
- `freeze_max_source = (source_duration - 0.05).max(0.0)`. The `max(0)` matters for sub-50 ms sources, which is what synthetic fixtures are.
- Playing past EOF splits into a `Play` tail of the available source plus a `Freeze` for the remainder.
- **Only `Play`/`Pause`/`Skip` emit a segment boundary.** `Zoom`/`Stroke`/`ClearAll`/`Unknown` must not, or a pinch gesture explodes the segment count.
- Segments with `out_duration <= 0` are not emitted.

**Tests** (port `PlaybackTimelineTests`, minus the `CMTime` case): source time at rest / after play / after pause / after skip; anchor override beats wall-clock drift; FF past end produces in-bounds play ranges; a zoom-dense event log produces the same segment count as the same log without zoom events; sub-50 ms source does not produce a negative freeze anchor; **sub-millisecond segments are produced and degenerate-duration segments are skipped by callers** (replacing the AVFoundation 600 Hz rounding assertion, which has no GStreamer analogue).

### Task 1.2 — `zoom.rs` behavior

```rust
impl Zoom {
    pub const IDENTITY: Zoom;
    pub fn clamped(self) -> Zoom;
    pub fn snapped(self) -> Zoom;
    pub fn source_point(self, view_x: f64, view_y: f64) -> (f64, f64);
    pub fn zoomed_to_cursor(self, new_scale: f64, cursor_x: f64, cursor_y: f64) -> Zoom;
    /// The ONE surviving transform. Letterbox-fit; pan is a fraction of the
    /// DISPLAYED SOURCE RECT, not of the viewport.
    pub fn transform(self, src_w: f64, src_h: f64, out_w: f64, out_h: f64) -> Affine;
}
pub const SNAP_NOTCHES: [f64; 8] = [1.0, 1.25, 1.5, 2.0, 3.0, 5.0, 7.5, 10.0];
```

`Affine` is a local 6-float struct (`a, b, c, d, tx, ty`) — no geometry crate, because `video-coach-core` takes no dependencies it does not need and this is six fields.

`clamped`: scale to `[1, 10]`; at scale 1 pan is forced to 0; otherwise pan clamped to `±(s−1)/(2s)`.
`snapped`: nearest notch within 3% *relative* tolerance, pan preserved. Interactive commit only — replay never snaps.
`transform`: exactly the spec formula, in **top-left** coordinates.

**Both delta variants are deliberately absent.** A porter reaching for `deltaTransform` because it is what the macOS compositors call would get pan wrong on every source whose aspect differs from the output. Leave a comment saying so at the top of the file.

**Tests** (port `ZoomTests`): clamp floor/cap; pan limit narrowing as scale → 1; pan forced to 0 at scale 1; snap within and outside tolerance; `zoomed_to_cursor` keeps the source point under the cursor fixed (round-trip against `source_point` — this is the test that pins the pan convention); `transform` at identity with matching aspect is the identity transform; `transform` letterboxes a 4:3 source into 16:9 with equal bars.

### Task 1.3 — `zoom_lookup.rs`

```rust
pub fn zoom_at(events: &[CommentaryEvent], record_time: f64) -> Zoom;
```

**Linear interpolation between adjacent keyframes**, alpha clamped to `[0,1]`; holds the first value before the first keyframe and the last after the last; `Zoom::IDENTITY` when there are no zoom events. Assumes the event log is sorted by `record_time` — assert that in a debug assertion rather than sorting defensively, since an unsorted log is a bug upstream.

The doc comment must state that the recorder's 100 ms anchor keyframe, its dedupe, and its no-throttling rule all exist *because* of this lerp, with a pointer to the spec. Without that note the anchor keyframe reads as cargo cult in Phase 6.

**Tests** (port `ClipZoomLookupTests`, `ZoomTests` lerp cases): empty → identity; before first; after last; exact keyframe hit; midpoint lerp on scale and both pans; anchor-keyframe pattern (value held across a gap then snapping) produces a hold, not a ramp.

### Task 1.4 — `stroke_replay.rs`

```rust
pub struct VisibleStroke<'a> {
    pub stroke: &'a Stroke,
    pub first_point_record_time: f64,
    pub drawn_point_count: usize,
}
pub fn visible_strokes(clip: &Clip, at_record_time: f64) -> Vec<VisibleStroke<'_>>;
```

`first_t = event.record_time - stroke.points.last().map_or(0.0, |p| p.t)` — the event is logged when the stroke **finishes**. Exact inequalities, all load-bearing:

- visible from `t >= first_t`
- auto-clear **inclusive**: hidden once `t >= first_t + auto`
- a `ClearAll` cancels only when `first_t < clear_time <= t` (**strictly** after `first_t`)
- `drawn_point_count` = count of points with `p.t <= elapsed`, i.e. the index of the first point with `p.t > elapsed`

Borrowing rather than cloning avoids copying point vectors per frame on the export hot path.

**Tests** (port `StrokeTests`, `StrokeReplayTests`): stroke invisible before `first_t`; fully drawn after its duration; partially drawn mid-stroke; auto-clear boundary is inclusive; `ClearAll` exactly at `first_t` does **not** clear; `ClearAll` one tick later does; a point whose `t` exactly equals elapsed **is** drawn.

### Task 1.5 — `plan.rs`

```rust
pub struct PlanEntry {
    pub clip_id: Uuid,
    pub index_in_output: usize,
    pub composition_start: f64,   // UI metadata only -- NOT a timing source
    pub segments: Vec<PlaybackSegment>,
    pub recording_duration: f64,
}
pub struct CompilationPlan { pub total_duration_seconds: f64, pub entries: Vec<PlanEntry> }

pub enum ExportTarget { AllClips, Tag(String) }
pub fn compilation_plan(project: &Project, target: &ExportTarget,
                        source_durations: &HashMap<usize, f64>) -> CompilationPlan;
```

`ExportTarget` replaces macOS's `"__all-clips__"` sentinel string, which was compared in five places. Clips are filtered by target, ordered by `sort_index`, and each entry's segments come from `playback_segments`. The source-duration fallback when a source is missing stays `start_source_seconds + recording_duration` — the smallest value guaranteed to cover any in-range position the clip visits at rate 1.

**`composition_start` carries a doc comment saying it is UI metadata and must not be used for timing** — the export frame driver builds its own cumulative walk over real segment durations, and conflating the two sums is what forced macOS to thread a single cursor end-to-end.

**Tests** (port `CompilationPlanTests`, `TagAggregationTests` plan cases): empty project; single clip; ordering by `sort_index` not insertion order; tag filter; `AllClips`; cumulative `composition_start`; missing source duration uses the fallback.

### Task 1.6 — `skip.rs`

```rust
pub struct SkipCoordinator { /* burst state */ }
impl SkipCoordinator {
    pub fn new(burst_window: Duration) -> Self;
    pub fn press(&mut self, delta: f64, now: Instant, current: f64) -> SkipAction;
    pub fn settle(&mut self, now: Instant) -> Option<SkipAction>;
}
pub enum SkipAction { SeekExact(f64), SeekCoarse(f64), None }
```

Port the burst state machine: the first press of a sequence seeks **exact**; follow-up presses inside the 150 ms window accumulate a target and switch to **coarse**; a debounce settles exact. The header comment explaining *why* coarse-then-refine was rejected — on long-GOP HEVC the coarse landing visibly snaps to the keyframe before the target and the debounce then jumps the rest of the way, reading as a double-seek for one keypress — ports with it.

**The 150 ms window and the exact-first policy were tuned against mpv + VideoToolbox on Apple Silicon.** Mark the constants as `pub` with a doc comment saying they must be re-measured against the chosen Linux decoder in Phase 2. Do not silently inherit them as tuned truth.

Take the clock as a parameter (`now: Instant`) rather than reading it internally, so tests are deterministic — the same injected-clock pattern `RecordingController` uses.

**Tests** (port `SkipCoordinatorTests`): single press → exact; two presses inside the window → coarse with accumulated target; press after the window → exact again; settle after a burst → exact at the accumulated target; settle with no pending burst → `None`.

---

## Done when

- `cargo test -p video-coach-core` green on a machine with **no GStreamer installed**.
- `cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check` all green.
- `video-coach-core/Cargo.toml` lists exactly four dependencies, none media-related.
- `CLAUDE.md` tells a fresh subagent the right test command without guessing.
- Every invariant the spec records under "Logic to port verbatim" has a test that fails if it is broken.

## Deliberately not in these phases

- Any GStreamer code, any Slint code, the command bus, the harness crate's contents.
- `UndoController`, `TagAggregation` (Phase 3); `ExportProgress` (Phase 8); `ScoreboardState`, `MatchInterpret` interpretation logic (Phase 9) — only the `MatchFormat` data accessors land here.
- Project open/create semantics, source management, the aspect-match gate (Phase 2).
- `recordings/.trash` lifecycle (Phase 3, with `UndoController`).
