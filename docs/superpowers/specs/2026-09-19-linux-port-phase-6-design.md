# Linux Port — Phase 6: Drawing During Recording

**Date:** 2026-09-19
**Status:** Reviewed (simplify and correctness passes applied; the Slint and lyon behaviour was probed)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 6)
**Evidence:** the macOS inventory of `DrawingOverlayView.swift`, `ContentView.swift`, `RecordingController.swift` and `StrokeReplay.swift`; the Slint 1.18 and lyon sources.

---

## Goal

While recording, the coach draws on the picture with the mouse or touchpad (click-drag). Drawings are red, appear live, fade 5 s after the pen lifts (unless auto-clear is off), and can be cleared at once. Each stroke lands in the clip's event log, so preview (Phase 7) and export (Phase 8) replay exactly what was seen.

## Done when

1. **Drawing.** While recording, click-drag on the picture draws a red line that follows the pointer. Two-finger scroll still pans and Ctrl+scroll still zooms (user decision, 2026-09-19).
2. **Auto-clear.** With "Auto-clear" on (the default), a drawing disappears 5 s after the pen lifts. With it off, drawings stay until **Clear** (the button, or C).
3. **The log.** A clip's `project.json` holds a `stroke` event per drawing and a `clearAll` event per clear, and `visible_strokes` at any record time reproduces what was on screen.
4. **Outside recording,** click-drag pans as before and nothing draws.

---

## Decisions

### D1. When drawing is possible

- **Only while `RecordingPhase::Recording`,** never `Starting`. macOS mounted the overlay only in `.recording`, and the bus drops stroke events outside a recording anyway.
- **Playing or paused makes no difference.**
- **The drawing `TouchArea` is the player's *last* child, after `zoom-area`, sized to the content rect** (the letterboxed picture at 1×), like the `Image`'s clip rectangle. So:
  - a press in the letterbox bars never reaches it, and pans as it does today;
  - `mouse-x/y` are already content-relative;
  - scroll isn't accepted, so it bubbles to the existing zoom handler and pan and zoom keep working while recording.
  - Slint visits front to back, and a `TouchArea` grabs the pointer on press, so the drawing area must be declared **after** `zoom-area` or presses never reach it.
  - Slint grabs the pointer on press, so a drag that leaves the picture keeps delivering moves. With the clamp in D2 it draws along the edge.
  - **Two accepted side effects while the drawing area is enabled:** a scroll during a stroke is swallowed, and hover moves stop reaching `zoom-area`, so the 2 and 3 zoom keys pivot on the picture's centre rather than the pointer while recording.

### D2. Capture

The UI thread captures, on the recording's clock (`now_ns()`).

- **Press** inside the content rect starts a stroke: `start_ns = now_ns()`, and a first point at `t = 0`.
- **Move** adds a point only when at least 1/60 s **and** at least 1 px have passed since the last **kept** point. Either gate rejects without updating the last point, as macOS did.
  - Slint coalesces moves to about one per event-loop turn, so the time gate rarely fires. Both are kept for parity, at no cost.
- **Release** reads the clock once, giving the release `t`. Then:
  - if the release position passes the 1 px gate, it is appended as the final point;
  - otherwise **the last point's `t` is updated to the release `t`**, keeping its position.

  Either way **the last point's time is the real pen-up**. A plain click therefore stays a single point.
- **The event.** The UI sends `Command::Stroke { host_ns: start_ns + last.t, stroke }`.
  - This keeps the invariant **`record_time` is the time of the last point**, which `visible_strokes` relies on (it back-computes the start as `record_time − last.t`).
  - **Why the release must move the time.** With a gate that only appends, a coach who draws and then holds still for 5 s before lifting would stamp the stroke 5 s early. The live overlay would clear 5 s after the real pen-up and replay 5 s after the stamp, which is the very mismatch D3 removes. It would also stamp the stroke **before** any play, pause or zoom logged during the hold, so `RecordingLog`'s clamp to the last event would fire and shift the whole drawing.
  - macOS had the same bug from the other direction: it stamped at mouse-up but stored no release point.
- **Coordinates.** The drawing area **is** the content rect, so `mouse-x/y` are already relative to it.
  - The in-progress stroke is buffered in **content px** plus `t`, and normalized once at pen-up: `(x / width, y / height)`, each clamped to [0, 1].
  - Do **not** route this through `Viewport::fraction`: that takes player-area coordinates and subtracts the content origin itself, which would shift every stroke by the letterbox offset.
  - macOS didn't clamp, so a drag past the edge could draw into the bars on export.
  - Strokes are **not** zoom-transformed: the coach draws on the zoomed picture as seen.
- **The stroke:** red (`Rgba::RED`), `line_width = 0.005` of height, and `auto_clear_after_seconds = Some(5.0)` when Auto-clear is on, else `None`.
- **The in-progress stroke is discarded** on stop, on abort, and on Clear (macOS parity).

### D3. Auto-clear counts from pen-up

- **The macOS mismatch.** Live, a stroke vanished 5 s after mouse-up; replay hid it 5 s after its first point, so a stroke held over 5 s vanished mid-draw in export.
- **The replay rule.** In `visible_strokes`, a stroke is hidden once `t ≥ record_time + auto`. Drawing still starts at `record_time − last.t`, and the clear-all rule is unchanged (a Clear during a stroke discards it, so it is never logged).
- **The live overlay keeps its own small model,** not a mirror of events: a list of finished strokes, each with an optional expiry `Instant` stamped at pen-up (`now + auto`), plus the in-progress stroke.
  - Clear empties the list; the tick drops expired entries.
  - That **is** D3's rule for the live case, in about fifteen lines. Live, "now" only moves forward and a finished stroke is always fully drawn, so `visible_strokes`' progressive reveal and clear-all folding would be dead weight.
  - So `visible_strokes` keeps its `&Clip` signature for preview (Phase 7) and export (Phase 8), which always replay a saved clip.
- **The format is unchanged.** Only the meaning of `auto_clear_after_seconds` is re-anchored, and nothing else reads it yet. The doc comments and the replay tests are updated, including `auto_clear_boundary_is_inclusive`, which is written against the old anchor.

### D4. The log

- **`RecordingLog`** gains `stroke(host_ns, stroke)` and `clear_all(host_ns)`, caller-captured like the rest.
- **The bus** gains `Command::Stroke { host_ns, stroke }` and `Command::ClearAll { host_ns }`, on the recording guard's allow-list, logged exactly as `log_zoom` does (whenever a recording is active). The UI is what restricts drawing to `Recording`; a second gate would be a second rule.

### D5. Live rendering

- **One Slint `Path` per visible stroke,** over the content rect:
  - `commands` is an SVG string in **content-rect logical px**;
  - **`fit: preserve`**, which short-circuits before any bounding-box fitting, so raw px are correct and no viewbox is needed;
  - `stroke: #ff3333` (exactly `Rgba::RED`), `stroke-width: content.height × 0.005`, round caps and joins;
  - **`fill` is left unset:** a fill on an open polyline fills the enclosed area.
- **A single-point stroke** is emitted as `M x y L x y`. A bare `M x y` produces no line segment and draws nothing (probed in lyon); the degenerate segment plus a round cap draws a dot. `stroke_replay.rs`'s doc comment, which claims a single point needs a filled circle, is corrected.
- **The in-progress stroke** is one more `Path`, rebuilt as points arrive.
- **Rebuild only on change:** a stroke finishing, a clear, an auto-clear expiry, or a resize. The existing 30 Hz `TICK` does the expiry check; no new timer. Slint re-parses `commands` and rebuilds the Skia path on every change, and a logged stroke's geometry is static, so rebuilding every tick would re-parse everything 30 times a second for nothing. The 30 Hz tick only compares "now" with the next expiry time.
- **Why not tiny-skia into an `Image`:** it re-uploads a full RGBA frame per update. macOS also used different live and export renderers. **What must match is geometry and timing, which core owns.**

### D6. UI

- **The Auto-clear checkbox (default on) and the Clear button are always mounted,** and disabled unless recording. macOS learned this: mounting them only while recording changed the player's height on mode switch, "which made it harder to land precise drawing strokes". Here the player rect feeds the content rect that strokes normalize against, so a resize on mode switch would be worse than cosmetic.
- **The C key** clears. It is placed **after** `handle-key`'s Ctrl branch, so Ctrl+C doesn't clear, ignores auto-repeat, and yields to text fields like every other shortcut.
- **Drawing, the crosshair cursor, Clear and C are gated on `recording-phase == Recording`,** not on the broader `recording` property, which includes `Starting`.

---

## Crate responsibilities

| Crate | Phase 6 contents |
|---|---|
| `video-coach-core` | `stroke_replay`: `visible_strokes(&[CommentaryEvent], at)` and the pen-up auto-clear rule. `RecordingLog::stroke` / `clear_all`. |
| `video-coach-media` | Nothing. |
| `video-coach-app` | `drawing.rs`: the in-progress buffer (content px), the thinning rule, normalization at pen-up, and the SVG path builder. Bus: the `Stroke` and `ClearAll` commands. UI: the drawing TouchArea, the live `Path` layer and its expiry list, Auto-clear, Clear, C, the crosshair. |
| `video-coach-harness` | Strokes and clears land in the log with the right record times. |

## Testing

- **Core:**
  - `visible_strokes` with the pen-up rule: a stroke held 7 s with auto 5 stays fully visible until 5 s after pen-up (this fails under the old rule);
  - the clear-all interaction is unchanged;
  - `RecordingLog::stroke` / `clear_all`.
- **App** (pure, in `drawing.rs`):
  - the thinning rule, including that a rejected point doesn't update the last one;
  - the release rule: a rejected release still moves the last point's `t`, and a click yields one point;
  - the path builder, including the single-point `M x y L x y`;
  - normalization at pen-up, with the clamp.
- **Harness:** during a recording, `Stroke` and `ClearAll` land with the expected record times, and both are dropped when not recording.
- **Manual** (batched): draw with the touchpad while recording; auto-clear on and off; Clear and C; a plain click leaves a dot; two-finger pan and Ctrl+scroll zoom still work while recording.

## Risks

1. **Pointer rate.** Slint coalesces moves to one per event-loop turn, so a fast flick samples coarsely. That is fine for telestration. BACKLOG #25 (the Wayland overlay latency spike) stays deferred: the user runs X11.
2. **Very long strokes** re-parse an O(n) path string on each update while drawing. There is no cap, as on macOS. Revisit if it lags.

## Deferred

- A colour palette, stroke undo, arrows and shapes: macOS had none.
- Drawing in preview (Phase 7).
- BACKLOG #25.
