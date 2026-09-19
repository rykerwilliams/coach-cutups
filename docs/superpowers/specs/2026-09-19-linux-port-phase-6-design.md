# Linux Port — Phase 6: Drawing During Recording

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 6)
**Evidence:** the macOS inventory of `DrawingOverlayView.swift`, `ContentView.swift`'s recording overlay, `RecordingController.swift` and `StrokeReplay.swift`; the Slint 1.18 source.

---

## Goal

While recording, the coach draws on the picture with the mouse or touchpad (click-drag). Drawings are red, appear live, fade 5 s after the pen lifts (unless auto-clear is off), and can be cleared at once. Each stroke lands in the clip's event log, so preview (Phase 7) and export (Phase 8) replay it exactly as seen.

## Done when

1. **Drawing.** While recording, click-drag on the picture draws a red line that follows the pointer. Two-finger scroll still pans and Ctrl+scroll still zooms (user decision, 2026-09-19).
2. **Auto-clear.** With "Auto-clear" on (the default), a drawing disappears 5 s after the pen lifts. With it off, drawings stay until **Clear** (button, or the C key).
3. **The log.** A clip's `project.json` holds a `stroke` event per drawing and a `clearAll` event per clear. `visible_strokes` at any record time reproduces what was on screen then.
4. **Outside recording,** click-drag pans as before, and nothing draws.

---

## Decisions

### D1. When drawing is possible

- **Only while `Recording`**, not while `Starting`, matching macOS, where the overlay exists only in `.recording`.
- **Playing or paused makes no difference.**
- **Left-drag on the picture draws** instead of panning. Panning stays available on two-finger or wheel scroll, and Ctrl+scroll zooms, as today. On macOS the overlay likewise covered drag-pan while recording.
- **A press in the letterbox bars** doesn't start a stroke.

### D2. Capture

The UI thread captures, using the same monotonic clock as the rest of the recording (`now_ns()`).

- **Press** inside the content rect starts a stroke. Record `start_ns = now_ns()` and the first point at `t = 0`.
- **Move** adds a point `(x, y, t = (now_ns() − start_ns)/1e9)` when at least 1/60 s **and** at least 1 px have passed since the last kept point (macOS's thinning).
- **Release** always adds the release position as the final point; macOS dropped it. It then sends `Command::Stroke { host_ns: start_ns + last.t, stroke }`.
  - That `host_ns` is **the time of the last point**, not a fresh clock read at release. macOS stamped the stroke at mouse-up, so holding still before release made the whole stroke replay late.
- **Normalization.** Points are in the **content rect**: the letterboxed picture rect at 1×, which is the Phase 2 `content` rect. They are top-left normalized and **clamped to [0, 1]**, so a drag past the edge draws along it. macOS didn't clamp, so export could draw into the bars.
  - Strokes are **not** zoom-transformed: the coach draws on the zoomed picture as seen (parent spec, "Content space").
- **The stroke itself:** red (`Rgba::RED`), `line_width = 0.005` (a fraction of height), and `auto_clear_after_seconds = Some(5.0)` when Auto-clear is on, else `None`.
- **When the stroke is thrown away:** stopping or aborting the recording with a stroke in progress discards it (macOS parity), and so does **Clear**.

### D3. Auto-clear counts from pen-up, in both live and replay

- **The mismatch.** On macOS the live stroke vanished 5 s after mouse-up, but replay (`StrokeReplay`) hid it 5 s after its **first** point. A stroke held for more than 5 s disappeared mid-draw in export.
- **The fix.** The port changes the replay rule to **hidden once `t ≥ event record_time + auto`**. The event's `record_time` is pen-up. Drawing still starts at `record_time − last.t`.
- **One function for both.** The live overlay computes what to show with **the same `visible_strokes`**, over the strokes and clears this recording has logged so far, at the current record time. Live and replay then agree **by construction**, not by keeping two implementations in step.
- **The format is unchanged.** Only the meaning of `auto_clear_after_seconds` is re-anchored. The v7 format is unshipped, so no migration is needed.

### D4. The log

- **`RecordingLog`** gains `stroke(host_ns, stroke)` and `clear_all(host_ns)`. Both are caller-captured, like play, pause, skip and zoom, and both use the existing record-time clamp to the last event.
- **The bus** gains:
  - `Command::Stroke { host_ns, stroke }` and `Command::ClearAll { host_ns }`, both on the recording guard's allow-list;
  - both are logged only while `Recording` (after the first video frame), and dropped otherwise.

### D5. Live rendering

- **One Slint `Path` per visible stroke,** in a layer over the picture's content rect:
  - `commands` is an SVG path string (`M x y L x y …`) built in Rust from the points, in logical px of the content rect;
  - `fit: preserve` (the default `contain` rescales the path);
  - `stroke: #ff3333`, `stroke-width: content.height × line_width`;
  - round caps and joins.
  - A **single-point** stroke draws as a filled circle of that diameter, since export fills a circle too.
- **The in-progress stroke** is one more `Path`, rebuilt as points arrive (O(n) per update, cheap at realistic sizes).
- **Refresh.** The UI recomputes the visible set in the existing 30 Hz tick, so auto-clear fades on time. It also recomputes when the window resizes, because the px coordinates depend on the content rect.
- **Why this renderer:** tiny-skia into an `Image` would share Phase 7's rasterizer, but it re-uploads a full RGBA frame per update. macOS also used different live (CAShapeLayer) and export (CGContext) renderers. **What must match is the geometry and timing, which core owns.**

### D6. UI

- **While recording,** the transport shows an **"Auto-clear"** checkbox (default on, UI state, not persisted) and a **"Clear"** button.
- **The C key** clears, with the same yield rules as the other shortcuts: never while a text field has focus. This is a small addition to macOS, which had no drawing keys.
- **The cursor** over the picture becomes a crosshair while recording.

---

## Crate responsibilities

| Crate | Phase 6 contents |
|---|---|
| `video-coach-core` | `stroke_replay`: auto-clear from pen-up (D3). `RecordingLog::stroke` / `clear_all`. Pure helpers: `stroke_path(points, rect) -> String`, and the thinning rule (`keep_point(last, new, dt, px)`). |
| `video-coach-media` | Nothing. |
| `video-coach-app` | Bus: the `Stroke` and `ClearAll` commands (on the allow-list). UI: drag-to-draw while recording, capture and normalization, the live `Path` layer, the Auto-clear checkbox, the Clear button, the C key. |
| `video-coach-harness` | Strokes and clears land in the log with the right record times. |

## Testing

- **Core:**
  - `visible_strokes` with the pen-up rule: a stroke held for 7 s with auto 5 is fully visible until 5 s after pen-up. Update the existing tests.
  - `RecordingLog::stroke` / `clear_all`.
  - `stroke_path` output.
  - The thinning rule.
- **Harness:** during a recording, send `Stroke { host_ns }` and `ClearAll`, stop, and check the clip's events: `stroke` at the right record time (from `host_ns`) and `clearAll`. Both are dropped when not recording.
- **App** (pure): normalization, content rect → [0, 1] with the clamp, and the letterbox rejection of presses.
- **Manual** (batched): draw with the touchpad while recording; watch auto-clear; Clear and C; two-finger pan and Ctrl+scroll zoom still work while recording.

## Risks

1. **Pointer rate.** Slint coalesces moves to one per event-loop turn, so a fast flick is sampled coarsely. That is fine for telestration; the Wayland/X11 latency spike (BACKLOG #25) stays deferred, since the user runs X11.
2. **Path re-parsing cost** for very long strokes. There is no cap, as on macOS; revisit if it lags.

## Deferred

- A colour palette, stroke undo, arrows and shapes: macOS had none.
- Drawing in preview (Phase 7): drawings are captured only while recording.
- BACKLOG #25, the Wayland overlay latency spike.
