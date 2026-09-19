# Linux Port — Phase 6 Plan (Drawing During Recording)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-6-design.md` (decisions D1–D6)
**Status:** Reviewed. Simplification and correctness passes are applied; the Slint and lyon behaviour was probed.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the whole workspace on its own.

**Known facts. Don't re-derive these.**
- **Slint 1.18 / lyon** (probed):
  - `Path` re-parses `commands` and rebuilds the Skia path on **every** change, so rebuild the model only when something changes.
  - `fit: preserve` short-circuits before any bounding-box or stroke-width fitting, so content-rect px are correct and no viewbox is needed. `fill` must stay unset.
  - `M x y` alone draws nothing; `M x y L x y` plus a round cap draws a dot.
  - Slint visits children **front to back**, and a `TouchArea` grabs the pointer on press. The drawing area must therefore be declared **after** `zoom-area` inside `player`, or presses never reach it.
  - A `TouchArea` with no `scroll-event` rejects wheel events, so they fall through to the sibling behind: pan and Ctrl-zoom keep working.
  - `enabled: false` forwards and ignores before hover or the cursor, so an off-recording drawing area is fully transparent.
  - Moves are coalesced to about one per event-loop turn.
- **Accepted side effects** while the drawing area is enabled: a scroll during a stroke is swallowed, and `zoom-area` stops receiving hover, so the 2 and 3 zoom keys pivot on the centre rather than the pointer while recording.
- **The UI:**
  - `handle-key` (`ui/app.slint:~658`) returns early for the entire Ctrl branch, so the C check goes after it; `root.text-editing` already yields at the top.
  - `root.recording` means "not idle", so it includes `Starting`. Gate drawing on the phase property.
  - The player exposes `picture` and `content` (`app.slint:~1042`).
  - `main.rs:~113` already runs a 30 Hz `TICK`; reuse it.
  - `main.rs:102` hardcodes `CaptureKind::Devices`, so there is **no way to drive a real recording without the camera**. Don't plan one.
- **Core:**
  - `visible_strokes` has **no production callers yet**, only core tests.
  - `tests/stroke_replay.rs:~106` `auto_clear_boundary_is_inclusive` is written against the old anchor and must be re-anchored.
  - `Zoom::content_fraction` (and `Viewport::fraction`) take **player-area** coordinates, so they are wrong for a content-relative drawing area. Normalize directly.
- **The bus:** `log_zoom` appends whenever `self.recording.is_some()`. Commands carry a UI-captured `host_ns`.
- **macOS parity:** a point rejected by either gate does **not** update the last kept point.

---

## Task 1 — Core: the replay rule and the log methods

1. **`stroke_replay.rs`:** auto-clear counts from pen-up (hidden once `at ≥ ev.record_time + auto`). The signature keeps `&Clip`.
   - Correct the module and `VisibleStroke` doc comments, including the stale claim that a single point needs a filled circle.
   - Update `stroke.rs`'s `auto_clear_after_seconds` doc.
2. **`RecordingLog::stroke(host_ns, stroke)` and `clear_all(host_ns)`.**
3. **Tests:**
   - a stroke held 7 s with auto 5 is fully drawn (every point) until 5 s after pen-up, and hidden at exactly `record_time + 5`. It fails under the old rule;
   - re-anchor `auto_clear_boundary_is_inclusive`;
   - the two log methods.

Commit: `feat(core): strokes clear from pen-up; log stroke and clear-all events`.

## Task 2 — Bus: the commands, and harness coverage

1. `Command::Stroke { host_ns, stroke }` and `Command::ClearAll { host_ns }`, on the recording guard's allow-list, logged like `log_zoom`.
2. **Harness tests:** during a recording, both land in the clip's events with the expected record times; both are dropped when no recording is active.

Commit: `feat(app): stroke and clear-all commands on the bus`.

## Task 3 — App: capture, the live overlay, the controls

1. **`drawing.rs`** (new, beside `zoom_input.rs`), pure and unit-tested. It owns the in-progress stroke:

   ```rust
   pub struct InProgress { start_ns: u64, points: Vec<(f64, f64, f64)> }   // content px, t seconds
   impl InProgress {
       pub fn start(start_ns: u64, x: f64, y: f64) -> Self;
       pub fn moved(&mut self, x: f64, y: f64, now_ns: u64);               // 1/60 s and 1 px gates
       pub fn release(self, x: f64, y: f64, now_ns: u64, rect: (f64, f64), auto: Option<f64>) -> Stroke;
       pub fn commands(&self, /* current rect */) -> String;
   }
   pub fn path_commands(points: &[StrokePoint], w: f64, h: f64) -> String; // normalized -> px
   ```

   - `release` either appends the release point or moves the last point's `t` to the release time (spec D2), then normalizes with the clamp.
   - `path_commands` emits `M x y L x y` for a single point.
2. **Capture** in `app.slint` and `main.rs`:
   - a drawing `TouchArea` as the **last** child of `player`, sized to `content`, `enabled` only while the phase is `Recording`, with a crosshair cursor;
   - press, move and release per D2;
   - on release: send `Command::Stroke { host_ns: start_ns + last.t, stroke }`, and push the stroke into the live list with its expiry.
3. **The live list** in `UiState`: `Vec<(Stroke, Option<Instant>)>`, plus the in-progress stroke.
   - It is cleared on every `Event::Recording` transition and on Clear, and both also drop the in-progress stroke.
   - The 30 Hz tick drops expired entries and rebuilds the model only then.
4. **The live layer:** one `Path` per live stroke plus one for the in-progress stroke, inside the content rect, per D5. Rebuild on a finish, a clear, an expiry or a resize.
5. **Controls:** an Auto-clear checkbox (default on) and a Clear button, always mounted and disabled unless the phase is `Recording`; the C key after the Ctrl branch, ignoring repeat.
6. **Render check** (no camera, no recording): a scratch Slint probe under the scratchpad that draws a two-point path and a single-point path with the production `path_commands`, `fit: preserve` and a round cap. Screenshot it and confirm the line and the dot. Delete the probe's output, and kill only your own PID.

Commit: `feat(app): draw on the picture while recording`.

## Task 4 — Closeout

1. Adversarial review of the Phase 6 diff (`git diff <plan commit>..HEAD -- crates`); apply the fixes and backlog any deferrals.
2. Write "### Task 4 notes" with the hands-on checklist items for the user:
   - draw with the touchpad while recording, and check the line follows the pointer;
   - a plain click leaves a dot;
   - auto-clear on: the drawing fades 5 s after the pen lifts; off: it stays until Clear;
   - Clear and the C key;
   - two-finger pan and Ctrl+scroll zoom still work while recording;
   - drawing is impossible when not recording, where drag still pans;
   - export a clip with drawings (Phase 8 burns them in; Phase 6 only logs them).

## Deliberately not in this phase

- A palette, stroke undo, shapes: macOS had none.
- Drawing in preview: Phase 7.
- BACKLOG #25 (the Wayland latency spike).
