# Linux Port — Phase 9: Scoreboard

**Date:** 2026-09-20
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 9; the scoreboard rows of the layout table; the `clipStartAbsSeconds` bug at lines 295 and 435)
**Builds on:** Phase 8 (the overlay rasterizer and its font system), Phase 7 (preview), Phase 3 (undo)
**Evidence:** the macOS inventory of `ScoreboardState.swift`, `MatchInterpret.swift`, `MatchFormat.swift`, `MatchEvent.swift`, `ScoreboardDraw.swift`, `MatchInspectorPanel.swift` and `KeyCommandView.swift`.

---

## Goal

The coach tags a match as they scan it — kick-off, half-time, full-time, and each goal — and every preview and export then carries a scoreboard: team names, the score at that moment, and the match clock, including stoppage time and half-time.

## Done when

1. **Tagging.** Pressing **E** enters event mode; **1**, **2** and **3** then tag a home goal, an away goal and a start/stop. Esc leaves event mode first.
2. **The panel.** A Match panel shows the live score and clock as you scan, three buttons for the same three actions, the list of events with their roles ("1H start", "HT", …), and seek and delete on each.
3. **Setup.** Team names, four colours each, and the match format (periods and their length) are editable and saved with the project.
4. **Burned in.** Preview and export draw the scoreboard top-left: `0.36 × width` by `0.08 × height`, with the team cells' accent strips, and the `+M:SS` tail in stoppage time.
5. **The clock is right inside a clip.** A clip that pauses for 20 s shows the same match time before and after the pause, because the clock follows the source frame, not the recording.
6. **Undo.** Tagging and deleting events are undoable.

---

## Decisions

### S1. The clock and score live in core, as a pure function of absolute time

`video-coach-core/src/scoreboard.rs` ports `ScoreboardState.swift`, `MatchInterpret.swift` and the derived parts of `MatchFormat.swift`:

- `PeriodRole`, `interpret(events, format)` and `start_stop_roles(project)`: start/stops sorted by absolute time with an input-order tie-break, truncated to `2 × total_periods`, even indices starting a period and odd ones ending it.
- `ClockDisplay::{Running, Stoppage { base, plus }, OnBreak(label), Fulltime}` and `format_clock`.
- `scoreboard_state(now_abs, config, events) -> Option<ScoreboardState>`, returning `None` before kick-off, with no config, **or with either team name empty** — the last one is what stops a half-configured scoreboard being burned into an export.
- Stoppage and half-time are **derived**, not stored: past the period's length it is stoppage; past `.end` it is the break, or full time on the last period.
- **The P1 back-anchor** is ported verbatim, including its deliberate lack of guards: it inserts a flagged start/stop at `(0, 0)` **at index 0** so it wins the tie-break, and shifts period 0's display so a recording that missed kick-off still reads 45:00 at the whistle. The UI gates it; the mutator does not. macOS's test pinning that, comment included, ports too.
- **Goals count inside `[first start, last end]`,** where the end is infinite unless the interpreted start/stops exactly fill the format — so a part-tagged match still counts late goals.

`scoreboard_config.rs` keeps the on-disk types. The mutators (`append_home_goal`, `append_away_goal`, `append_start_stop`, `set_auto_back_anchor_p1`) join `project.rs` beside the other edits.

### S2. Per frame, the drivers pass absolute time — and nothing else

`ExportJob` and `PreviewJob` gain `scoreboard: Option<ScoreboardContext>`, built once by the bus:

```rust
pub struct ScoreboardContext { config: ScoreboardConfig, events: Vec<AbsoluteMatchEvent>, source_offsets: Vec<f64> }
```

Each frame, the driver computes `abs = source_offsets[entry.source_index] + frame.source_time` and calls `scoreboard_state`.

**This avoids BACKLOG #27's export bug by construction.** macOS computed the clock as a per-clip constant plus the commentary's wall clock, so every pause and skip pushed the clock ahead of the footage — and since every recording starts with a pause, that was nearly always. The port has no such constant: `frame.source_time` already comes from the clip's own playback segments. **No per-entry absolute constant is added to `PlanEntry`; that field is the bug.**

`ScoreboardState` is `PartialEq`, so the renderer can reuse its layout while the state is unchanged, as the text bar's fitting already does.

### S3. Drawing: the same overlay, on top

The scoreboard joins `overlay.rs`'s single layer, drawn **after** the strokes and the bar's glyphs (macOS draws it on top of everything). `OverlayFrame` gains `scoreboard: Option<&ScoreboardState>`, and `core/src/layout.rs` gains the ratios its header already promised:

| Element | Value |
|---|---|
| Bar | `0.36 × outW` by `0.08 × outH`, inset `0.015 × outH`, top-left |
| Accent strip | `0.08 × barH`, over the home and away cells only |
| Columns | home `0.30`, score `0.20`, away `0.30`, clock `0.20` |
| Cells | score `#1a1a1a`, clock `#0d0d0d` at 0.95 alpha |
| Team font | `min(fit(home), fit(away))`, desired `0.55 × barH`, floor 6 px |
| Score and clock font | `0.55 × barH`, bold |
| Stoppage tail | its own rect off the clock cell's right edge, `0.45 × barH` |

- **`DejaVuSans-Bold.ttf` is vendored** beside the regular face, with its licence: every scoreboard label is bold on macOS, and the overlay loads only embedded faces.
- **Team-name sizing comes from measured text,** which `overlay.rs` already has (`fit`, `width`). A different font means different sizes than macOS, so the tests assert properties ("inside the bar rect", "the left margin is untouched"), not golden images — which is what macOS's own render tests do.

### S4. Entry: event mode plus a Match panel

- **Keys,** mirroring macOS: `E` enters event mode (a UI flag); `1`, `2` and `3` then tag home goal, away goal and start/stop instead of zooming; **Esc leaves event mode first** in the existing cascade.
- **The Match panel** sits in the right-hand column beside the clip inspector and tag overview:
  - the live score and clock as a line of text, updated from the scan position;
  - three buttons for the same three tags, so the feature is discoverable without the keys;
  - the auto-back-anchor toggle;
  - the event list in match order, each row with its interpreted role, a seek button and a delete button;
  - a settings mode for team names, the four colours each, and the format, with a warning when more start/stops are tagged than the format expects.
- **No scoreboard is drawn over the scan picture** (user decision, 2026-09-20). The panel's line is the live readout; preview and export carry the bar itself. That keeps the scan view clean and avoids a second rasterizer in the UI toolkit.

### S5. Commands, undo and storage

- **Commands:** `TagMatchEvent { kind, source_index, source_seconds }` — with the position **captured on the UI thread**, per the bus contract — plus `DeleteMatchEvent(Uuid)`, `SetScoreboard(Option<ScoreboardConfig>)` and `SetAutoBackAnchorP1(bool)`.
- **Undo:** one `UndoAction::EditMatchEvents { before, after }` holding the whole list, as macOS did. The lists are small, and per-event undo would buy nothing.
- **Match events belong to the project, not to clips** — as the format already has them. A goal must appear on every clip spanning it, and the clock runs across all sources.
- **Source moves and deletions already remap them,** and removing a source an event points at is already refused. **Nothing in Phase 9 needs to touch that**; it is easy to redo by accident.

---

## Crate responsibilities

| Crate | Phase 9 contents |
|---|---|
| `video-coach-core` | `scoreboard.rs` (interpret, roles, clock, state); `MatchFormat`'s derived accessors; the event mutators; the scoreboard's layout ratios. |
| `video-coach-media` | The scoreboard in `overlay.rs`, drawn last; the vendored bold face; `ScoreboardContext` on both jobs. |
| `video-coach-app` | Bus: the four commands, `EditMatchEvents` undo, building the context. UI: event mode, the Match panel and its settings mode. |
| `video-coach-harness` | Tagging, deleting, undo, and the context reaching an export. |

## Testing

- **Core:** port macOS's ~830 lines — stoppage in both halves, HT and FT, quarters, overtime, the goal window, `interpret`'s tie-break and truncation, the roles map, `MatchFormat`'s names and labels, and the back-anchor test **with its comment**.
- **Media:**
  - property assertions on the drawing: inside the bar rect, the area left of it untouched, the accent strips only over the team cells, the stoppage tail present only in stoppage;
  - **the pause test** (the spec's own): a clip with a mid-clip pause of N seconds shows the same clock at record time `p` and `p + N`. This is the test that pins BACKLOG #27 shut.
- **Harness:** tag, delete, undo, and an export whose scoreboard context reaches the overlay.
- **Manual** (batched): tag a real match while scanning and check the clock against the footage.

## Risks

1. **The clock's correctness inside a clip** is the whole point of the phase, and it is one line (`offset + source_time`). The pause test is what keeps it.
2. **Half-configured scoreboards:** three distinct "draw nothing" cases (no config, an empty team name, before kick-off), all easy to miss.
3. **Modal keys.** Event mode is the first real mode in this UI. Its place in the Esc cascade needs checking against the existing one rather than assumed.

## Deferred

- A scoreboard over the scan picture (user decision).
- Manual clock offsets beyond the P1 back-anchor: macOS had none.
- Per-event undo.
