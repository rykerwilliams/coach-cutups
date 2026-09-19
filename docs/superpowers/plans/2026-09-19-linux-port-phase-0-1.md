# Linux Port — Phases 0 and 1

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md`
**Status:** Reviewed — two adversarial passes applied
**Task 0.1 and 0.5 are already executed** (`08c9b4b`, `1a282ca`).

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
                                    (no date crate -- see Task 0.2, created_at)
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
2. `video-coach-core/Cargo.toml` depends on `serde` (derive), `serde_json`, `uuid` (v4, serde), `thiserror`, plus `tempfile` as a **dev**-dependency. No image crate, no font crate, no date crate, no GStreamer, no feature that pulls one in.

   The rule is **"no media dependency"**, not a dependency count. A count invites contradiction (the first draft of this plan said "exactly four" and then used `DateTime<Utc>`, a fifth) and will break at the first legitimate addition.
3. The other three crates get a `lib.rs` containing only a module doc comment stating what the crate will hold and that it is empty until Phase 2.
4. CI workflow:
   - `core` job: `cargo test -p video-coach-core` on `ubuntu-latest` **with no GStreamer installed**. This is the core-isolation enforcement — if a media dependency is ever added, this job fails to build.
   - `workspace` job: `cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`.
   - `windows` job: `cargo check -p video-coach-core` only, marked `continue-on-error: true`. Advisory, per the spec — a red Windows build does not veto a Linux-right dependency, and the media crate would need GStreamer dev libraries on the runner to typecheck at all, which is not worth setting up for a non-target.

**Verify:** `cargo build --workspace && cargo test --workspace` green. Commit.

### Task 0.2 — Data model

**Files:** `project.rs`, `event.rs`, `stroke.rs`, `scoreboard_config.rs`

**Derives.** Every type here carries `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`. **`Eq` and `Hash` are unavailable** — these types contain `f64`, and the Swift originals are `Hashable` only because Swift permits it. Any collection keyed on a clip or a stroke keys on its `Uuid`, never on the struct.

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
```

`Zoom`'s `PartialEq` is load-bearing and must stay **bit equality**. The Phase 6 recorder dedupe is `if z == last_captured { return }` (`RecordingController.swift:129`), and its intent is exactly "the gesture fired but `snapped().clamped()` collapsed to the same notch" (`:117-121`). An epsilon comparison would suppress genuinely distinct keyframes and break the anchor-keyframe pattern. Note this in the file so nobody "fixes" it later.

```rust
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

**Serialization is mostly derived, not hand-written.** Serde's externally-tagged representation already produces the Swift shape when `ClearAll` is declared as an **empty struct variant** `ClearAll {}` — that emits `{"clearAll":{}}`, matching `CommentaryEvent.swift:70`. (A unit variant would emit the bare string `"clearAll"`, which is why the first draft wrongly concluded a full hand-written codec was needed.) Only `Unknown(Value)` resists derivation, because `#[serde(other)]` cannot carry a payload:

```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
enum KnownKind { Play { source_time: f64 }, Pause { source_time: f64 },
                 Skip { delta: f64 }, Stroke(Stroke), ClearAll {}, Zoom(Zoom) }
// EventKind::deserialize: Value::deserialize, try KnownKind::deserialize(&v), else Unknown(v).
// EventKind::serialize:   delegate to KnownKind, or emit the Value verbatim.
```

Three traps to honor:

1. **Enum-level `rename_all` renames variants only.** `source_time` → `sourceTime` needs `rename_all_fields` (serde ≥ 1.0.186) or per-variant attributes. Pin it with a test asserting the literal string `{"play":{"sourceTime":1.5}}`.
2. **A malformed *known* key must also fall through to `Unknown`.** Swift uses `try?` on every branch (`CommentaryEvent.swift:45-60`), so `{"play":{}}` decodes as `.unknown` rather than throwing. Matching the key name and then hard-erroring on the payload breaks the forward-compatibility the variant exists for. The `KnownKind::deserialize(&v)`-then-fallback shape above gets this right for free.
3. macOS writes `.unknown` as `{"recordTime":x,"kind":{}}` — the event survives, only the payload is lost. v2 keeps the payload instead.

```rust
// project.rs
pub enum Resolution { R720, R1080, R2160 }     // `source` is gone (spec)
pub enum Quality { Low, Medium, High }
pub struct Preferences {
    pub scan_volume: f64,
    pub preview_source_volume: f64,
    pub preview_commentary_volume: f64,
    pub last_export_resolution: Resolution,
    pub last_export_quality: Quality,
    pub preferred_camera_id: Option<String>,
    pub preferred_mic_id: Option<String>,
    pub pip_for_new_recordings: bool,
}
pub struct SourceRef {
    pub relative_path: String,                  // POSIX '/', may traverse '..'
    pub display_name: String,
    pub duration_seconds: f64,                  // THE authority -- see Task 1.4
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
    pub created_at: String,                     // RFC3339, opaque
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

**`created_at` is an opaque RFC3339 `String`, not a datetime type.** It has **zero readers** anywhere in the Swift tree — grep finds it only in `Project.swift`'s own declaration, init, `CodingKeys` and decoder. Ordering is by `sort_index`. Pulling in a date crate for a field nothing reads would violate the no-unneeded-dependency rule for no benefit. If a real datetime type is ever wanted, it lands with the first consumer.

**`source_index` is `usize`.** Swift uses `Int` and `cumulativeOffset` clamps both ends (`Project.swift:183`), but the lower clamp is provably dead under `usize`, so **drop `WorkspaceCumulativeTests`' `sourceIndex: -1` case rather than porting it to `0usize` where it proves nothing.** A negative `sourceIndex` in JSON is a corrupt file and `Malformed` is the honest answer; Phase 2 owns remap correctness.

**Defaults are per-field, never blanket.** `#[serde(default)]` means `Default::default()`, which is `0.0` and `false` — so a blanket rule silently mutes all three volumes and turns PiP off. Write `impl Default for Preferences` by hand matching `Project.swift:20-36` (volumes `1.0`, `R1080`, `Medium`, `pip_for_new_recordings: true`), and use `#[serde(default = "...")]` naming a function per non-zero default.

**Only genuinely optional keys get a default at all**: `preferences`, `scoreboard`, `match_events`, and the two device-ID `Option`s. **`name`, `clips`, `source_videos` and `Clip`'s required fields are NOT defaulted** — a missing one is `Malformed`. Swift's `decodeIfPresent` machinery exists solely for v1→v6 migration (`Project.swift:136-139` says so); v2 has zero legacy files, so porting the blanket rule ports the reason without the reason. Worse, defaulting `clips` means a truncated `project.json` loads as an empty project and the next `write` destroys the user's data — which is the exact hazard the atomic write in Task 0.3 exists to prevent.

JSON field names stay **camelCase** (`#[serde(rename_all = "camelCase")]`).

**`scoreboard_config.rs` carries data only.** `TeamConfig` (name + `primary_color`/`secondary_color`/`font_color`, all required `Rgba` — Swift defaults `fontColor` to `secondaryColor` in its *initializer*, which serde cannot express; on a clean-slate format make it required and let the Phase 9 constructor supply the default), `ScoreboardConfig` (home, away, format), `MatchFormat` (the four `regulation_*`/`overtime_*` fields), `MatchEventKind { StartStop, HomeGoal, AwayGoal }` (string-encoded), and `MatchEventRecord` (id, kind, source_index, source_seconds, is_auto_back_anchor).

**No derived accessors.** `total_periods`, `expected_start_stop_events`, `is_overtime`, `period_seconds`, `period_name`, `break_label` and the soccer `"1H"`/`"HT"` special cases are **Phase 9**, where the spec puts them and where their only consumers live. Porting them here is 60 lines of Phase 9 logic plus `MatchFormatTests`, eight phases early.

**Tests:** serde round-trip every type, including `StrokeTests.test_strokeRoundtripsThroughJSON` (which is a serde test, not a replay test). Plus, specifically:
- the literal shape `{"play":{"sourceTime":1.5}}` and `{"clearAll":{}}`
- `Unknown` round-trips `{"recordTime":1.0,"kind":{"futureKind":{"someField":42}}}` byte-for-byte — reuse the fixture from `CommentaryEventZoomTests.test_unknown_kind_decodes_as_unknown_case_not_error`
- `{"play":{}}` (missing payload) decodes as `Unknown`, not an error
- **`serde_json::from_str::<Preferences>("{}")` yields all three volumes `== 1.0`, `R1080`, `Medium`, `pip_for_new_recordings == true`** — a round-trip test structurally cannot catch a wrong default, and this single test covers the whole class
- a `Project` JSON missing `clips` is an error, not an empty project
- `auto_clear_after_seconds` distinguishes absent from null

**Verify:** `cargo test -p video-coach-core`. Commit.

### Task 0.2b — Virtual timeline and tag normalization

**Files:** `project.rs` (impl block), `tag.rs`

```rust
impl Project {
    pub fn total_source_duration(&self) -> f64;
    pub fn cumulative_offset(&self, source_index: usize) -> f64;   // sum of durations[0..i], upper-clamped
    pub fn abs_seconds(&self, source_index: usize, source_seconds: f64) -> f64;
}
pub fn normalize_tags(input: &str) -> Vec<String>;
```

`cumulative_offset` clamps `source_index` to at most `len` and returns 0 for an empty list. `normalize_tags` splits on `,`, trims, lowercases, drops empties, de-duplicates **preserving first-seen order**.

Folded into Task 0.2 rather than standing alone: these are ~25 lines on types Task 0.2 just defined, and a separate subagent would reload the whole of `project.rs` to add three one-line methods.

**Tests:** port `ProjectTests` and `WorkspaceCumulativeTests` — empty list, index past end, index 0, mid-list accumulation (**minus the `-1` case**, per above); tag normalization including duplicates, mixed case, empty fragments, single untagged string.

### Task 0.3 — Project store and the version guard

**Files:** `store.rs`

```rust
pub const CURRENT_FORMAT_VERSION: u32 = 7;

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    #[error("no project.json in {0}")]
    MissingProjectJson(PathBuf),
    #[error("this project was created by the macOS version of Coach Cuts (format v{found}) and cannot be opened; v{minimum} or later is required")]
    LegacyProject { found: u32, minimum: u32 },
    #[error("project format v{found} is newer than this build supports (v{supported})")]
    TooNew { found: u32, supported: u32 },
    #[error("project.json is unreadable: {0}")]
    Malformed(String),
    #[error(transparent)] Io(#[from] std::io::Error),
}

pub fn read(project_dir: &Path) -> Result<Project, StoreError>;
pub fn write(project_dir: &Path, project: &mut Project) -> Result<(), StoreError>;
```

**`MissingProjectJson` is a named variant, not a folded `Io`.** Phase 2's central rule — "empty folder ⇒ create" versus "unreadable `project.json` ⇒ refuse, do not overwrite" (`Workspace.swift:163-185`) — is *defined* by this distinction. Swift has the variant (`ProjectStore.swift:3-6`); folding it into a transparent `Io` would force Phase 2 to match on `ErrorKind::NotFound` inside it.

Guard order — read the version from raw JSON **before** full deserialization, so a Swift-era file produces `LegacyProject` rather than a field-level decode error:

1. Parse to `serde_json::Value`.
2. Resolve `formatVersion` in **three** cases, not two:
   - key absent → `1` (a file with no version predates the field)
   - key present and an integer → that value
   - key present and anything else (a JSON float like `7.0`, a string, `null`) → **`Malformed`**

   The third case matters: the obvious `.and_then(Value::as_u64).unwrap_or(1)` maps `"formatVersion": 7.0` to `1` and then tells the user their valid v7 project was made by the macOS app — a confidently wrong error, the worst kind. Several generic JSON writers emit integral numbers as floats.
3. `< 7` → `LegacyProject`. `> 7` → `TooNew`. Else deserialize.

`write` must do three things Swift does and the first draft omitted:

1. **Stamp `format_version = CURRENT_FORMAT_VERSION`** (`ProjectStore.swift:31-32`) — hence `&mut Project`.
2. **Create the `recordings/` subdirectory** (`:34-37`). Phase 2's create flow and Phase 3's `.trash` both assume it exists.
3. **Write atomically** — temp file in the same directory, then rename. An interrupted save must not leave a truncated `project.json`, since that is the file the refuse-to-overwrite rule keys on.

**Tests:** round-trip; `formatVersion: 6` → `LegacyProject`; **no `formatVersion` key → `LegacyProject { found: 1 }`** (the case a `bookmark`-sniffing guard would have missed); `formatVersion: 8` → `TooNew`; **`formatVersion: 7.0` → NOT `LegacyProject`**; truncated JSON → `Malformed`; missing file → `MissingProjectJson`; `write` bumps a v1 in-memory project to 7 and the file reads back; `write` creates `recordings/`; second write over an existing file does not corrupt it. Plus a `temp_project()` fixture helper used by later tests.

**Verify:** `cargo test -p video-coach-core`. Commit.

---

## Phase 1

### Task 1.1 — `timeline.rs`

```rust
pub enum SegmentKind { Play, Freeze }
pub struct PlaybackSegment { pub kind: SegmentKind, pub source_start: f64, pub out_duration: f64 }

pub fn source_time(clip: &Clip, at_record_time: f64, source_duration: f64) -> f64;
pub fn playback_segments(clip: &Clip, source_duration: f64) -> Vec<PlaybackSegment>;
```

**Clamping — state the placement, because it changes the answer.** Clamp at **each mutation**, mirroring `playback_segments`, not once on the returned value:

- `.skip` → clamp the cursor to `[0, source_duration]` (`PlaybackTimeline.swift:130`)
- `.play`/`.pause` anchors → **not clamped**, matching the Swift builder
- the trailing rate integration → clamped to `[0, source_duration]`

An end-of-function clamp diverges: with duration 1000, `skip(+1e6)` then `skip(-10)` gives 990 from the segment walk and 1000 from a return-value clamp.

**Two authorities, deliberately 50 ms apart at EOF.** Even with per-mutation clamping the two functions do not agree exactly, because `playback_segments` caps every freeze's `source_start` at `freeze_max_source` (`:88`, `:95`). Example from `PlaybackTimelineTests.swift:69-82`: start 998, `skip(+100)`, duration 1000 → the builder answers 999.95, `source_time` answers 1000.0.

> **Decision:** `freeze_max_source` is a **decoder-safety pullback, not a semantic answer.** `playback_segments` is authoritative for *which frame to pull*; `source_time` is authoritative for *the clock*. They agree to within 50 ms at EOF by design. Add a test that pins the 50 ms gap **deliberately**, so a later reader does not "fix" one side and leak a decoder detail into the match clock.

Other invariants:
- `.play`/`.pause` **anchor** the cursor to the carried `source_time`, overriding the wall-clock computation.
- `freeze_max_source = (source_duration - 0.05).max(0.0)`, applied as a **cap** (`min(cursor, freeze_max)`) on freeze `source_start` only — **not** to `.play` segments, and with **no lower bound**, so a negative pause anchor yields a negative freeze anchor exactly as Swift does.
- Playing past EOF splits into a `Play` tail plus a `Freeze`.
- **Only `Play`/`Pause`/`Skip` emit a segment boundary.** Zoom/Stroke/ClearAll/Unknown must not.
- `emit`'s early return on `out_duration <= 0` **also skips the `record_cursor` update** (`:66-68` vs `:99`) while the caller still applies the state change. Hoisting the cursor update out of the guard — the natural Rust refactor — changes behavior for two events at the same `record_time`.
- Both functions assume the event log is sorted by `record_time`. Put a shared `Clip::debug_assert_sorted_events()` in `project.rs` and call it from `source_time`, `playback_segments` and `zoom_at`.

**Tests** (port `PlaybackTimelineTests`, minus the `CMTime` case): source time at rest / after play / pause / skip; anchor override beats wall-clock drift; FF past end yields in-bounds play ranges; a zoom-dense log produces the same segment count as the same log without zoom events; sub-50 ms source does not produce a negative freeze anchor; sub-millisecond segments are produced and degenerate ones are skipped by callers (replacing the AVFoundation 600 Hz rounding assertion). **Plus two new tests the first draft lacked:**
- a **differential property test** — for a log containing a play past EOF, a pause and a backward skip, `source_time(clip, t)` matches the position implied by walking `playback_segments`, at a grid of `t`, to within the 50 ms EOF allowance. Every other test listed here passes with the two functions disagreeing.
- the deliberate 50 ms EOF gap, asserted as intended behavior.

### Task 1.2 — `zoom.rs` (data behavior + replay lookup)

Folded together because `Zoom::lerp` lives in `ClipZoomLookup.swift:42-50` but is a `Zoom` method — the original module boundary cuts through one type.

```rust
impl Zoom {
    pub const IDENTITY: Zoom;
    pub fn clamped(self) -> Zoom;
    pub fn snapped(self) -> Zoom;
    pub fn lerp(a: Zoom, b: Zoom, alpha: f64) -> Zoom;
    /// `content_x`/`content_y` are fractions of the DISPLAYED (letterboxed)
    /// source rect -- NOT of the viewport. A caller holding a window-space
    /// cursor must convert first.
    pub fn source_point(self, content_x: f64, content_y: f64) -> (f64, f64);
    pub fn zoomed_to_cursor(self, new_scale: f64, content_x: f64, content_y: f64) -> Zoom;
    /// The ONE surviving transform. Letterbox-fit; pan is a fraction of the
    /// DISPLAYED SOURCE RECT, not of the viewport.
    pub fn transform(self, src_w: f64, src_h: f64, out_w: f64, out_h: f64) -> Affine;
}
pub const SNAP_NOTCHES: [f64; 8] = [1.0, 1.25, 1.5, 2.0, 3.0, 5.0, 7.5, 10.0];

pub fn zoom_at(events: &[CommentaryEvent], record_time: f64) -> Zoom;
```

`Affine` is a local 6-float struct — six fields, and a geometry crate would violate the no-unneeded-dependency rule. Since `b == c == 0` always, the affine is exactly the rect `(tx, ty, src_w·s, src_h·s)`, which is the form both `gltransformation` and tiny-skia want. **`transform` at `Zoom::IDENTITY` is also the content rect** that strokes denormalize against — one function covers both needs; do not add a second.

`snapped` returns the **first** notch within 3% relative tolerance in array order. The table's ≥20% spacing makes the windows disjoint so first == nearest, but only by accident of the table — say so, or a future notch insertion silently changes semantics.

`zoom_at` **linearly interpolates** between adjacent keyframes, alpha clamped to `[0,1]`; holds the first value before the first keyframe and the last after the last; `IDENTITY` when empty. The doc comment must state that the Phase 6 anchor keyframe, dedupe and no-throttling rules exist *because* of this lerp — without that note the anchor reads as cargo cult.

**Both delta transform variants are deliberately absent.** A porter reaching for `deltaTransform` because it is what the macOS compositors call would get pan wrong on every non-matching aspect. Say so at the top of the file.

**Tests.** Note which are ports and which are new, because the first draft said "port `ZoomTests`" for things that file does not contain:
- *Ports:* clamp floor/cap; pan limit narrowing as scale → 1; pan forced to 0 at scale 1; `zoomed_to_cursor` ∘ `source_point` round-trip (`ZoomTests.swift:34-57`); `test_zoomedToCursor_preserves_cursor_pivot_through_chained_zooms` (`:48`); identity transform with **equal source and dest sizes** (`:61-70` — not "matching aspect"; a 640×360 source into 1920×1080 gives `a = 3`); `transform` letterboxes 4:3 into 16:9 with equal bars.
- *New — `snapped()` has no test anywhere in the Swift repo:* notch hit inside tolerance, miss outside, pan preserved, replay path never snaps.
- **New and load-bearing — the pan-convention test.** `ZoomTests.test_deltaTransform_with_pan_matches_sourcePoint` is the only test tying `pan` to a rendering matrix, and it covers the variant being deleted. Replace it: `transform` must map source pixel `((0.5+pan_x)·src_w, (0.5+pan_y)·src_h)` to the **output centre**, for a non-square source and non-zero pan. Without it nothing pins the `- pan_x * src_w * s` term, and dropping the `* s` gives a pan that drifts toward centre with scale.
- Choose a cursor/scale **inside** the pan limit for the round-trip tests, or `.clamped()` binds and the test is flaky by construction.
- *Ports from `ClipZoomLookupTests`:* empty → identity; before first; after last; exact keyframe hit; midpoint lerp; `test_ignores_non_zoom_events`; `test_unknown_kind_does_not_appear_in_zoom_lookup`; anchor-keyframe pattern produces a hold then a snap, not a ramp.

### Task 1.3 — `stroke_replay.rs`

```rust
pub struct VisibleStroke<'a> {
    pub stroke: &'a Stroke,
    pub first_point_record_time: f64,
    pub drawn_point_count: usize,
}
pub fn visible_strokes(clip: &Clip, at_record_time: f64) -> Vec<VisibleStroke<'_>>;
```

`first_t = event.record_time - stroke.points.last().map_or(0.0, |p| p.t)` — the event is logged when the stroke **finishes**. Exact inequalities: visible from `t >= first_t`; auto-clear **inclusive** at `t >= first_t + auto`; a `ClearAll` cancels only when `first_t < clear_time <= t`; `drawn_point_count` = count of points with `p.t <= elapsed`.

**`clear_all` times must be collected in a separate pass BEFORE the stroke loop** (`StrokeReplay.swift:16-19`). This is the single most important implementation detail in the file and the four inequalities alone do not imply it: a natural single forward pass adds a stroke to the output before it reaches the later `ClearAll` event that should have removed it.

**Tests** (port `StrokeReplayTests`): **`test_laterClearAll_clearsEarlierStrokesEvenInForwardOrder` — port it with its comment intact**; it is the test that fails a single-pass implementation and the first draft omitted it. Plus: invisible before `first_t`; fully drawn after its duration; partially drawn mid-stroke; auto-clear boundary inclusive; `test_clearAllAffectsEarlierStrokesButNotLater`. *New:* `ClearAll` exactly at `first_t` does **not** clear; a point whose `t` exactly equals elapsed **is** drawn. (`StrokeTests.swift` is a JSON round-trip and belongs in Task 0.2, not here.)

### Task 1.4 — `plan.rs`

```rust
pub struct PlanEntry { pub clip_id: Uuid, pub segments: Vec<PlaybackSegment>, pub recording_duration: f64 }
pub struct CompilationPlan { pub total_duration_seconds: f64, pub entries: Vec<PlanEntry> }

pub enum ExportTarget { AllClips, Tag(String) }
pub fn compilation_plan(project: &Project, target: &ExportTarget,
                        source_durations: &HashMap<usize, f64>) -> CompilationPlan;
```

**`composition_start` is deleted, not documented.** Across the whole Swift tree it has exactly one non-constructor read — `CompilationExporter.swift:377`, as a fallback at precisely the site the spec forbids using it. A field whose doc comment says "do not use this for the thing it looks like" should not exist; Phase 8's flat segment list is the authoritative cumulative walk. **`index_in_output` is deleted too** — it is just the entry's index in `entries`, used as a dictionary key and for `"i+1 / N"` display.

**`total_duration_seconds` is the sum of segment `out_duration`s**, not of `recording_duration`s. Swift accumulates `recordingDuration` (`CompilationPlan.swift:61-80`) and asserts at `:16` that the two agree — which holds only when every event's `record_time` lies within `[0, recording_duration]`. An out-of-range event makes `emit(to: recording_duration)` produce `dur <= 0` and emit nothing, so the segment sum exceeds it. Since the spec makes this value the export-progress denominator, that disagreement shows up as progress past 100%. Defining it from segments removes the class for one line.

Clips are filtered by target, ordered by `sort_index` via **`sort_by_key`** (stable, so ties resolve to insertion order — Swift's `sorted(by:)` is not documented stable, so this is a free determinism improvement worth stating as an invariant and testing).

**Duration authority.** `SourceRef.duration_seconds` is the single authority, and Phase 2's `gst_discoverer` probe **writes it back** into the project on add and relink. The `source_durations` map parameter exists only for the missing-source fallback and for tests. Otherwise preview clamping against the persisted value and export clamping against a probed value give different clock readings at EOF for the same clip — the exact bug class the spec's golden rule exists to kill. The fallback when a source is missing stays `start_source_seconds + recording_duration`.

`ExportTarget` replaces macOS's `"__all-clips__"` sentinel compared in five places, and collapses two near-duplicate entry points into one function — strictly less code than it replaces.

**Tests** (port `CompilationPlanTests`): empty project; single clip; ordering by `sort_index` not insertion order; ties resolve to insertion order; tag filter; `AllClips`; missing source duration uses the fallback. *New:* a clip with an event at `record_time > recording_duration` still has `total_duration_seconds == sum(segment out_durations)`.

### Task 1.5 — `skip.rs`

**The first draft of this task invented an API.** The real machine is three event-driven entry points plus `reset`, and `nowMonotonicSeconds` is accepted by all three Swift methods and **read by none** — there is no time comparison anywhere in the class. `burst_window` is never compared against a clock; it is only *handed back* to the caller so the caller can arm its own timer.

```rust
pub struct SeekParams { pub target_seconds: f64, pub exact: bool }
pub struct SkipDecision { pub seek: Option<SeekParams>, pub arm_debounce: Option<Duration> }

impl SkipCoordinator {
    pub fn new(burst_window: Duration) -> Self;
    pub fn request_skip(&mut self, delta: f64, current: f64, clip_duration: f64) -> SkipDecision;
    pub fn seek_completed(&mut self) -> SkipDecision;
    pub fn burst_ended(&mut self) -> SkipDecision;
    pub fn reset(&mut self);
}
```

Five things the invented API got wrong, each pinned by an existing test:

1. **No `seek_completed`.** *Every* coarse seek is issued from there (`SkipCoordinator.swift:115-127`), never from a press. Without it the coordinator emits no coarse seek at all.
2. **`SkipDecision` carries two independent fields.** A follow-up press returns `arm_debounce` with **no seek** (`:84`); the burst-mode switch returns a seek **and** a re-arm (`:118-119`). An `enum SkipAction` can express neither.
3. **`request_skip` needs `clip_duration`** — it clamps the target to `[0, clip_duration]` (`:68`).
4. **`burst_ended` during flight returns nothing and sets `exact_pending`** (`:137-149`), so the exact settle fires from the *next* `seek_completed` (`:106-114`).
5. **`reset()` exists**, used on player swap (`:155-157`).

**Drop the `Instant`/`now` parameter entirely** — that is the simplification to bank. Swift carried a dead parameter through three signatures; its absence is *why* the Swift tests are already deterministic, and `Instant` never needs to enter `video-coach-core`.

What makes the next press exact is `flying == None` (the previous seek *landed*), not window expiry.

Keep the header comment explaining why coarse-then-refine was rejected: on long-GOP HEVC the coarse landing visibly snaps to the keyframe before the target and the debounce then jumps the rest of the way, reading as a double-seek for one keypress. Mark `burst_window`'s 150 ms default `pub` with a doc comment saying it was tuned against mpv + VideoToolbox on Apple Silicon and **must be re-measured against the chosen Linux decoder in Phase 2**.

**Tests:** port **all 10** in `SkipCoordinatorTests.swift`, not the 5 the first draft listed — including `test_secondSkipDuringFlight_accumulatesAndArmsOnly`, `test_secondSkipDuringFlight_targetAccumulatesNotResetsToCurrent`, `test_burstEndedDuringFlight_thenSeekCompletes_firesExactSeek`, `test_skipBeforeZero_clampsToZero`, `test_skipPastDuration_clampsToDuration`, `test_reset_clearsAllStateAndAllowsFreshSeek`, `test_seekCompletedAfterReset_isSafeNoOp`.

---

## Done when

- `cargo test -p video-coach-core` green on a machine with **no GStreamer installed**.
- `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check` all green.
- `video-coach-core/Cargo.toml` lists no media dependency and no date crate.
- Every invariant the spec records under "Logic to port verbatim" has a test that fails if it is broken.

## Deliberately not in these phases

- Any GStreamer code, any Slint code, the command bus, the harness crate's contents.
- `MatchFormat`'s derived accessors, `ScoreboardState`, `MatchInterpret` (Phase 9) — only the data types land here.
- `UndoController`, `TagAggregation` (Phase 3); `ExportProgress` (Phase 8).
- Project open/create semantics, source management, the aspect-match gate (Phase 2).
- `recordings/.trash` lifecycle (Phase 3, with `UndoController`).
