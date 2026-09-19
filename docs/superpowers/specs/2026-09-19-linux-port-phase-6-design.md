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
- **The drawing `TouchArea` is a child of the player, sized to the content rect** (the letterboxed picture at 1×), like the `Image`'s clip rectangle. So:
  - a press in the letterbox bars never reaches it, and pans as it does today;
  - `mouse-x/y` are already content-relative;
  - scroll isn't accepted, so it bubbles to the existing zoom handler and pan and zoom keep working while recording.
  - Slint grabs the pointer on press, so a drag that leaves the picture keeps delivering moves. With the clamp in D2 it draws along the edge.

### D2. Capture

The UI thread captures, on the recording's clock (`now_ns()`).

- **Press** inside the content rect starts a stroke: `start_ns = now_ns()`, and the first point at `t = 0`.
- **Move** adds a point `(x, y, t = (now_ns() − start_ns)/1e9)` only when at least 1/60 s **and** at least 1 px have passed since the last **kept** point. Either gate rejects without updating the last point, as macOS did.
  - Slint coalesces moves to one per event-loop turn, so the time gate rarely fires. Both gates are kept for parity, at no cost.
- **Release** reads the clock **once**. That reading gives both the final point's `t` and, through it, the event time. The release point is subject to the same 1 px rule, so a plain click stays a **single-point** stroke.
- **The event.** The UI sends `Command::Stroke { host_ns: start_ns + last.t, stroke }`.
  - Deriving `host_ns` from the last point keeps the invariant **`record_time` is the time of the last point**, which `visible_strokes` relies on (it back-computes the start as `record_time − last.t`).
  - macOS instead stamped at mouse-up and never stored the release point, so a stroke held still before release replayed late.
  - The `RecordingLog`'s clamp to the last event is a no-op here in practice: commands carry UI-captured times and arrive in order. If it ever fired, the whole drawing would shift later. That is documented, not engineered around.
- **Normalization.** Points come from the existing `Viewport::fraction` (which wraps core's `Zoom::content_fraction`), made public: the content rect, top-left, **clamped to [0, 1]**. macOS didn't clamp, so a drag past the edge could draw into the bars on export.
  - Strokes are **not** zoom-transformed: the coach draws on the zoomed picture as seen.
- **The stroke:** red (`Rgba::RED`), `line_width = 0.005` of height, and `auto_clear_after_seconds = Some(5.0)` when Auto-clear is on, else `None`.
- **Discarded** in-progress strokes: on stop, abort, or Clear (macOS parity).

### D3. Auto-clear counts from pen-up, in both live and replay

- **The macOS mismatch.** Live, a stroke vanished 5 s after mouse-up; replay hid it 5 s after its first point, so a stroke held over 5 s vanished mid-draw in export.
- **The rule.** A stroke is hidden once `t ≥ record_time + auto`. Drawing still starts at `record_time − last.t`, and the clear-all rule is unchanged (a Clear during a stroke discards it, so it is never logged).
- **One function for both.** `visible_strokes` takes **`&[CommentaryEvent]`** instead of `&Clip`. Export and preview pass `&clip.events`; the live overlay passes the UI's own mirror.
- **The UI mirror.** `UiState` keeps a `Vec<CommentaryEvent>` of the strokes and clears **this** recording has logged:
  - appended at the moment the command is sent, with `record_time = (host_ns − t0_ns)/1e9` from the already-stored `recording_t0`;
  - cleared on every `Event::Recording` transition (start, stop and abort).

  Without a mirror the UI would need a round trip to the bus, which would flicker at pen-up.
- **The format is unchanged.** Only the meaning of `auto_clear_after_seconds` is re-anchored, and nothing else reads it yet. The doc comments and the replay tests are updated.

### D4. The log

- **`RecordingLog`** gains `stroke(host_ns, stroke)` and `clear_all(host_ns)`, caller-captured like the rest.
- **The bus** gains `Command::Stroke { host_ns, stroke }` and `Command::ClearAll { host_ns }`, on the recording guard's allow-list, logged exactly as `log_zoom` does (whenever a recording is active). The UI is what restricts drawing to `Recording`; a second gate would be a second rule.

### D5. Live rendering

- **One Slint `Path` per visible stroke,** over the content rect:
  - `commands` is an SVG string in **content-rect logical px**;
  - **`fit: preserve`**, which short-circuits before any bounding-box fitting, so raw px are correct and no viewbox is needed;
  - `stroke: #ff3333` (exactly `Rgba::RED`), `stroke-width: content.height × 0.005`, round caps and joins;
  - **`fill` is left unset:** a fill on an open polyline fills the enclosed area.
- **A single-point stroke** is emitted as `M x y L x y`. A bare `M x y` produces no line segment and draws nothing (probed in lyon); the degenerate segment plus a round cap draws a dot. If the batched manual check shows no dot, fall back to a round `Rectangle`.
- **The in-progress stroke** is one more `Path`, rebuilt as points arrive.
- **Rebuild only on change:** a stroke finishing, a clear, an auto-clear expiry, or a resize. Slint re-parses `commands` and rebuilds the Skia path on every change, and a logged stroke's geometry is static, so rebuilding every tick would re-parse everything 30 times a second for nothing. The 30 Hz tick only compares "now" with the next expiry time.
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
| `video-coach-app` | `drawing.rs`: the thinning rule and the SVG path builder (both view concerns). `Viewport::fraction` made public. Bus: the `Stroke` and `ClearAll` commands. UI: the drawing TouchArea, the live `Path` layer, the mirror, Auto-clear, Clear, C, the crosshair. |
| `video-coach-harness` | Strokes and clears land in the log with the right record times. |

## Testing

- **Core:**
  - `visible_strokes` with the pen-up rule: a stroke held 7 s with auto 5 stays fully visible until 5 s after pen-up (this fails under the old rule);
  - the clear-all interaction is unchanged;
  - `RecordingLog::stroke` / `clear_all`.
- **App** (pure, in `drawing.rs`):
  - the thinning rule, including that a rejected point doesn't update the last one;
  - the path builder, including the single-point `M x y L x y`;
  - normalization and the clamp through `Viewport::fraction`.
- **Harness:** during a recording, `Stroke` and `ClearAll` land with the expected record times, and both are dropped when not recording.
- **Manual** (batched): draw with the touchpad while recording; auto-clear on and off; Clear and C; a plain click leaves a dot; two-finger pan and Ctrl+scroll zoom still work while recording.

## Risks

1. **Pointer rate.** Slint coalesces moves to one per event-loop turn, so a fast flick samples coarsely. That is fine for telestration. BACKLOG #25 (the Wayland overlay latency spike) stays deferred: the user runs X11.
2. **Very long strokes** re-parse an O(n) path string on each update while drawing. There is no cap, as on macOS. Revisit if it lags.

## Deferred

- A colour palette, stroke undo, arrows and shapes: macOS had none.
- Drawing in preview (Phase 7).
- BACKLOG #25.
