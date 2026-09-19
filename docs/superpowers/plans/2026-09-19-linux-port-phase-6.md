# Linux Port — Phase 6 Plan (Drawing During Recording)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-6-design.md` (decisions D1–D6)
**Status:** Draft, pre-review.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the whole workspace.

**Known facts. Don't re-derive these.**
- **Slint 1.18 / lyon** (probed):
  - `Path` parses `commands` at runtime on every change and rebuilds the Skia path, so rebuild the model only when something changes.
  - `fit: preserve` short-circuits before any bounding-box or stroke-width fitting, so content-rect px coordinates are correct and no viewbox is needed.
  - `M x y` alone yields no line segment and draws nothing; `M x y L x y` keeps a degenerate segment, drawn as a dot by a round cap.
  - `fill` must stay unset on an open polyline.
  - A `TouchArea` grabs the pointer on press, so moves that leave it still arrive.
  - A `TouchArea` that doesn't accept scroll lets it bubble to the parent handler.
  - Moves are coalesced to about one per event-loop turn.
- **The UI:**
  - `handle-key` (`ui/app.slint:~658`) returns early for the whole Ctrl branch, so the C check goes after it. `root.text-editing` already yields at the top.
  - `root.recording` means "not idle", so it includes `Starting`. Use the phase property for drawing.
  - The player exposes `picture` and `content` rects (`app.slint:~1042`); `content` is what strokes normalize against.
  - `zoom-area` handles press, drag, scroll (`main.rs:~556`, `zoom_input.rs`).
  - `UiState.recording_t0` already holds `t0_ns` (`main.rs:~59`).
- **The bus:** `log_zoom`/`log_playing` append whenever `self.recording.is_some()` (`bus/recording.rs:~188`). Commands carry a UI-captured `host_ns`.
- **Core:** `visible_strokes(clip, at)` is at `stroke_replay.rs:35`; `Zoom::content_fraction` clamps to [0,1]; `Viewport::fraction` wraps it in `zoom_input.rs`.
- **macOS parity detail:** in `DrawingOverlayView`, a point rejected by either gate does **not** update the last kept point.

---

## Task 1 — Core: replay rule, log methods

1. **`stroke_replay.rs`:**
   - `visible_strokes(events: &[CommentaryEvent], at_record_time: f64)`. Update every caller.
   - Auto-clear counts from pen-up: hidden once `at ≥ ev.record_time + auto`.
   - Update the module and `stroke.rs` doc comments (the old rule was "from the first point").
2. **`RecordingLog::stroke(host_ns, stroke)` and `clear_all(host_ns)`,** matching the existing appends.
3. **Tests** (`tests/stroke_replay.rs`, `tests/recording.rs`):
   - a stroke held 7 s with auto 5 is fully visible until 5 s after pen-up (it fails under the old rule);
   - the clear-all interaction is unchanged;
   - progressive point reveal is unchanged;
   - the two log methods.

Commit: `feat(core): strokes clear from pen-up; log stroke and clear-all events`.

## Task 2 — App: capture, live overlay, controls

1. **`drawing.rs`** (new, beside `zoom_input.rs`), pure and unit-tested:
   - `keep_point(last: StrokePoint, x, y, t, px_per_unit) -> bool`: the 1/60 s and 1 px gates, with macOS's "a rejected point doesn't update last" semantics;
   - `path_commands(points: &[StrokePoint], rect: Rect) -> String`: content-rect px, with the single-point `M x y L x y`;
   - the in-progress stroke's state machine, if it is cleaner as a type here.
2. **`Viewport::fraction`** becomes `pub`.
3. **Capture** in `main.rs` and `app.slint`:
   - a drawing `TouchArea` as a child of the player, sized to `content`, enabled only while `recording-phase == Recording`, with a crosshair cursor;
   - press, move and release per D2, with `now_ns()` read once at release;
   - on release, send `Command::Stroke { host_ns: start_ns + last.t, stroke }` and append to the UI mirror.
4. **The mirror:** `UiState` keeps `Vec<CommentaryEvent>` for this recording, appended when a command is sent and cleared on every `Event::Recording` transition.
5. **The live layer:** one `Path` per visible stroke plus one for the in-progress stroke, inside the content rect, per D5. Rebuild the model when a stroke finishes, on a clear, on an auto-clear expiry, or on a resize. The 30 Hz tick only checks the next expiry.
6. **Controls:** an Auto-clear checkbox (default on) and a Clear button, always mounted and disabled unless recording; the C key after the Ctrl branch, ignoring repeat.
7. **Bus:** `Command::Stroke` and `Command::ClearAll`, on the allow-list, logged like `log_zoom`.
8. **Harness tests:** during a recording, `Stroke` and `ClearAll` land with the expected record times; both are dropped when not recording.
9. **Screenshot pass:** a scratch project, a callback driver (no input injection), recording started with test capture sources, a stroke sent through the command path, and a screenshot of the live overlay. Delete the scratch data, and kill only your own PID.

Commit: `feat(app): draw on the picture while recording`.

## Task 3 — Closeout

1. Adversarial review of the Phase 6 diff; apply and backlog.
2. The hands-on checklist items, in the Task 3 notes.

## Deliberately not in this phase

- A palette, stroke undo, shapes: macOS had none.
- Drawing in preview: Phase 7.
- BACKLOG #25 (the Wayland latency spike).
