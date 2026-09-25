---
name: port-swift-module
description: Port a module from the macOS original, read out of the macos-reference git tag, into the Rust video-coach-core crate. Use when translating any Swift type, function, or test file to Rust.
argument-hint: "<Swift file or module name, e.g. ScoreboardState>"
---

# Port a Swift module to video-coach-core

The Swift tree is no longer in the working tree: it lives at the annotated tag
`macos-reference`, read with `git show macos-reference:<path>` or checked out
whole with `git worktree add /tmp/macos-reference macos-reference`. It is the
reference for behavior, not the spec — it has known bugs (BACKLOG #27) and
Apple-framework workarounds that must not be ported. The port spec is
`docs/superpowers/specs/2026-09-19-linux-port-design.md`; check its "Logic to
port verbatim" section and the phase plan for what this module owes.

## 1. Read before you write any signature

Read the Swift source **and** its test file (`apple/VideoCoachCore/Tests/
VideoCoachCoreTests/` inside the tag) in full before proposing a Rust API. An API written from memory of what a module "probably" does was
wrong once already: `SkipCoordinator` was planned with two entry points and an
enum result, and the real machine has three entry points, issues its coarse
seek from `seekCompleted`, and returns a seek and a debounce independently.
The planned tests would have passed against the wrong state machine.

Then write down every invariant with its `file:line`, and classify each:

- **Port faithfully** — load-bearing behavior (inequalities, anchors, clamps).
- **Deliberate fix** — a Swift bug or hole. Fix it, say so in a doc comment,
  and pin it with a test. Example: play/pause anchors were unclamped in Swift
  and handed the decoder a negative position.
- **Drop** — exists only because of an Apple constraint (AVFoundation tick
  rounding, Core Image's bottom-left origin, `Speech` not linking in
  `swift test`, dead parameters). Say why in the commit message.

Fidelity is not the goal when the original is wrong.

## 2. Rust traps that bit this port

- **`f64::clamp` panics** if either bound is NaN or `min > max`. Swift's nested
  `min`/`max` never trap. Any bound derived from user data (durations read
  from `project.json`) must be guarded: `x.clamp(0.0, dur.max(0.0))`. For
  NaN-tolerant clamping use `.min(hi).max(lo)` and
  `#[allow(clippy::manual_clamp, reason = "...")]`.
- **serde naming.** Every persisted struct needs
  `#[serde(rename_all = "camelCase")]` — including structs used as newtype
  enum payloads, because `rename_all_fields` does not reach through them
  (`Zoom` shipped as snake_case this way). Add a **literal wire-shape test**
  (`assert_eq!(to_string(&x), r#"{...}"#)`); a round-trip test cannot catch a
  wrong field name.
- **serde defaults.** Field-level `#[serde(default)]` gives `0.0`/`false`.
  Put `default` on the container with a hand-written `impl Default`, and never
  default a required field — a truncated file would load as empty and the
  next save would destroy data. Test defaults by deserializing `{}`.
- **Non-finite floats** serialize as `null` and then fail to deserialize
  (BACKLOG #28). Don't produce them; `debug_assert!(x.is_finite())` at
  producers.
- **Event logs are sorted by `record_time`.** Call
  `crate::event::debug_assert_sorted(&events)` at every entry point that walks
  one. Fixtures must be sorted too — the log is ordered by pen-up time, so a
  clear-all at a stroke's *start* precedes the stroke's own event.
- **Core has no media dependency.** No GStreamer, image, font or date crate.

## 3. Port the tests honestly

- Mark each test **ported** (name the Swift test) or **new**. Don't claim a
  Swift file covers something it doesn't.
- Skip tests that assert an AVFoundation artifact (e.g. 600 Hz `CMTime`
  rounding); replace with the behavior-level invariant.
- For anything with two functions that must agree, write a differential test
  over a grid of inputs with an **exact** tolerance and named exceptions — a
  blanket tolerance lets a uniform drift regression pass.
- Check boundary semantics explicitly: half-open spans, inclusive vs strict
  inequalities.

## 4. Finish

Run the `verify` skill. Commit with a message that states every deliberate
divergence from Swift and why.
