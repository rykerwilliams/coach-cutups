# Linux Port — Phase 9: Scoreboard

**Date:** 2026-09-20
**Status:** Reviewed (simplify and correctness passes applied)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 9; the scoreboard rows of the layout table, **whose font ratios this spec corrects**; the `clipStartAbsSeconds` bug at lines 295 and 435)
**Builds on:** Phase 8 (the overlay rasterizer and its font system), Phase 7 (preview), Phase 3 (undo)
**Evidence:** `apple/VideoCoachCore/Sources/VideoCoachCore/{ScoreboardState,MatchInterpret,MatchFormat,MatchEvent}.swift` and `Overlays/ScoreboardDraw.swift`; `apple/App/Views/Scoreboard/MatchInspectorPanel.swift`; `apple/App/Views/KeyCommandView.swift`.

---

## Goal

The coach tags a match as they scan it — kick-off, half-time, full-time, and each goal — and every preview and export then carries a scoreboard: team names, the score at that moment, and the match clock, including stoppage time and half-time.

## Done when

1. **Tagging.** Three keys tag a home goal, an away goal and a start/stop while scanning. The Match panel has the same three as buttons.
2. **The panel** shows the live score and clock, the event list with each event's role ("1H start", "HT", …), and seek and delete per row.
3. **Setup.** Team names, their three colours each, and the match format are editable in a sheet and saved with the project.
4. **Burned in.** Preview and export draw the scoreboard top-left, with the accent strip, and the `+M:SS` tail in stoppage time.
5. **The clock is right inside a clip.** A clip that pauses for 20 s shows the same match time before and after the pause.
6. **Undo.** Tagging and deleting are undoable.

---

## Decisions

### S1. The clock and score: one core module, a pure function of absolute time

`scoreboard_config.rs` **becomes** `scoreboard.rs` (its own header already says Phase 9 completes it), holding the on-disk types and the behaviour:

- `PeriodRole`, `interpret(events, format) -> Vec<(Uuid, PeriodRole)>`: start/stops stably sorted by absolute time with an input-order tie-break, truncated to `2 × total_periods`, even indices starting a period and odd ones ending it. **It returns ids**, so the panel doesn't index two parallel filtered lists the way macOS did.
- `ClockDisplay::{Running, Stoppage { base, plus }, OnBreak(label), Fulltime}` and `format_clock`.
- `scoreboard_state(now_abs, config, events) -> Option<ScoreboardState>`, where **`ScoreboardState` is `{ home_score, away_score, clock }`** — it does not carry the team configs, which the caller already has. Cheap per frame, so no memo is needed for it.
- **It returns `None`** when no start/stop has been tagged yet, or when the first tagged start is still ahead of `now`. Not being configured is `Option<ScoreboardConfig>` at the caller, and **empty team names are rejected by the command** (S5), so the render path has one guard.
- Stoppage and half-time are **derived**: past the period's length it is stoppage; past `.end` it is the break, or full time on the last period.
- **Goals count inside `[first start, last end]`,** the end being infinite unless the interpreted start/stops exactly fill the format, so a part-tagged match still counts late goals.
- **`ScoreboardContext::for_project(&Project)`** and **`state_at(source_index, source_time)`** live here too, so the context is assembled and the arithmetic done in one place.
- **`scoreboard_rects(out_w, out_h) -> ScoreboardRects { bar, accent, home, score, away, clock, tail }`** in `layout.rs`, beside `bar_rect` and `pip_rect`: the phase's fiddliest arithmetic, unit-tested without GStreamer.

**The P1 back-anchor is derived, not stored.** macOS inserted a flagged `(0, 0)` event at index 0, relied on `interpret`'s tie-break, bypassed its own cap, and then added an offset to the *displayed* number — which left the clock reading 50:00 while still counted as running, so stoppage never began. Instead:
- `ScoreboardConfig` gains `auto_back_anchor_p1: bool`, set in the setup sheet (it is setup: "my video starts after kick-off");
- `interpret` **truncates the stored start/stops to the format's capacity first**, then prepends a derived start, so the anchor never costs the coach a slot;
- that derived start is at `p1_end_abs − period_seconds(0)` once a first end is tagged, and **at absolute 0 before then**, so the clock runs from the start of the footage during the first half and snaps to the right alignment when half-time is tagged. Without the fallback there would be no clock at all through the half the coach most wants one.
- `interpret` returns `(Option<Uuid>, PeriodRole)`: the derived start has no record.

A back-anchored first period **ends at exactly `period_seconds(0)` and never enters stoppage** — that is what the anchor means, since it defines half-time as 45:00. macOS's version instead read 50:00 while still "running". `MatchEventRecord::is_auto_back_anchor` is removed (nothing writes it yet, serde ignores unknown keys, so the format stays at v7), and macOS's test pinning the old behaviour is **not** ported.

### S2. Per frame, the drivers pass absolute time

`ExportJob` and `PreviewJob` gain `scoreboard: Option<ScoreboardContext>`, built once by the bus:

```rust
pub struct ScoreboardContext { config: ScoreboardConfig, events: Vec<AbsoluteMatchEvent>, source_offsets: Vec<f64> }
```

Each frame the driver calls `context.state_at(entry.source_index, frame.source_time)`.

- **This is safe because a clip cannot span a source boundary:** a `Clip` has one `source_index` and every timeline mutation clamps within it.
- **The absolute events are derived once per job** and must never be cached across a source add, move, remove or **relink** (a relink can change a source's duration, and so every later offset).
- **It closes BACKLOG #27 by construction.** macOS computed the clock as a per-clip constant plus the commentary's wall clock, so every pause and skip pushed the clock ahead of the footage — and since every recording opens with a pause, that was nearly always. **No per-entry absolute constant is added to `PlanEntry`; that field is the bug.**
- **The clock is the displayed frame's source time,** i.e. `FrameSpec::source_time` from `playback_segments`, not `timeline::source_time`. Those two differ by up to 50 ms at a freeze near the end of a source, deliberately. `timeline.rs`'s module doc currently names *itself* as the scoreboard's authority: **amend it** in this phase, keeping its 50 ms note and changing its consumer, rather than shipping a file that contradicts the code.

### S3. Drawing: the same overlay, on top

The scoreboard joins `overlay.rs`'s single layer, drawn **after** the strokes and the text bar (macOS draws it on top of everything). `OverlayFrame` gains `scoreboard: Option<(&ScoreboardConfig, &ScoreboardState)>`, and `layout.rs` gains `SCOREBOARD_*` ratios — **named distinctly**, since the text bar already has a `BAR_HEIGHT_RATIO` that happens to be the same 0.08.

| Element | Value |
|---|---|
| Bar | `0.36 × outW` by `0.08 × outH`, inset `0.015 × outH`, top-left |
| Accent strip | `0.08 × barH`, **above** the cells, over the home and away columns only |
| Cells | height `scoreBarH = barH − accentH`, at `top + accentH` |
| Columns | home `0.30`, score `0.20`, away `0.30`, clock `0.20` |
| Cell fills | home and away `primary_color`, the accent `secondary_color`, score `#1a1a1a`, clock `#0d0d0d` at 0.95 alpha |
| Fonts | `0.55 × scoreBarH`, bold, in each team's `font_color` |
| Stoppage tail | its own rect, gap `0.025 × scoreBarH` off the clock cell, `0.45 × scoreBarH`, **not bold** (macOS used an absolute 2 pt gap) |
| Team name pad | `0.05 × scoreBarH` (macOS used an absolute 4 pt, which changes meaning with resolution) |

**These are fractions of `scoreBarH`, not `barH`** — the parent spec's table says `barH` and is ~9% too large. Correct both.

- **`DejaVuSans-Bold.ttf` is vendored** beside the regular face, with its licence: four of the five labels are bold.
- **Team names use a fixed size and ellipsize** (`overlay.rs`'s existing `fit`), rather than macOS's shrink-to-fit with a 6 px floor: at a cell 10.8% of the width, a shrunk long name is illegible anyway.
- **All five labels are centred** in their cells, so `draw_text` is generalized with colour and alignment; today it hardcodes white and left-aligns.
- **Weight is explicit** (`Attrs::weight`), since both faces load under one family: bold for the four labels, normal for the tail.
- **The fitting memo becomes a 3-slot array** keyed by `TextSlot { Bar, HomeName, AwayName }`: the scoreboard ellipsizes two names that share a size and width, so a second single slot would still thrash.

### S4. Entry: three direct keys, and a Match panel

- **No event mode.** macOS needed `E` then `1/2/3` because it had no free keys; this port does. **`z` tags a home goal, `x` an away goal, `v` a start/stop**, directly. That removes a UI mode, a branch in the Esc cascade and a second gate on the zoom keys.
  - They carry `!event.repeat` (a held key must not insert a goal per repeat), and they yield to text fields like every other shortcut.
  - They are gated exactly as the panel's buttons are: while scanning or recording, never while previewing or during a recording's start-up.
- **The Match panel** sits in the right-hand column beside the clip inspector and tag overview:
  - the live score and clock as text;
  - the same three actions as buttons, so the feature is discoverable;
  - the auto-back-anchor toggle;
  - the event list in match order, each row with its role, a seek and a delete;
  - **a warning when the format is shrunk below the number of start/stops already tagged.**
- **The panel's clock comes from the scan anchor** (`source_index` and the last good position), computed in the existing tick — **not** from the shared position properties, which a preview repurposes to record time within one clip. **While a preview is open the panel's clock freezes**; the preview's own scoreboard is burned into its picture. No scoreboard clock is computed during an export: that is per-frame in the driver.
- **No scoreboard is drawn over the scan picture** (user decision, 2026-09-20).

### S5. Commands, undo and storage

- **Commands:** `TagMatchEvent { kind, source_index, source_seconds }` and `DeleteMatchEvent(Uuid)`, plus `SetScoreboard(ScoreboardConfig)` — which carries the back-anchor flag, so there is no separate toggle command.
- **The tag's anchor is the position the readout already computes** (`target_abs` when a seek is outstanding, else `abs_seconds(source_index, last_secs)`), mapped back through `locate()`. Reading `source_index` and `last_secs` separately pairs a new index with an old offset across a cross-source seek.
- **Validation lives at the command,** not in the render path: `SetScoreboard` rejects an empty team name with a message.
- **One `append_match_event(kind, …)` mutator,** not three that differ by a constant.
- **One cap rule.** `interpret` truncates the stored start/stops to the format's capacity — that must be total regardless. The UI **disables** the start/stop action at the cap and says why. The mutator does **not** silently no-op, as macOS's did: a command that quietly does nothing is worse than one that refuses out loud.
- **Undo:** `UndoAction::EditMatchEvents { before, after }` holding the whole list, as macOS did.
  - **A source move or remove purges `EditMatchEvents` from both stacks,** where trashed clips are already evicted. Those two permute `source_index`; a snapshot on the stack isn't remapped, so undo would restore events pointing at the wrong source.
  - **Add and relink don't need the purge:** events store `(source_index, source_seconds)`, and neither operation permutes indices. (The *derived absolute* events are a different matter — never cache those across any source edit, a relink included, since a duration change moves every later offset.)
- **Match events belong to the project,** as the format already has them: a goal must appear on every clip spanning it, and the clock runs across all sources. Source moves and deletions already remap them, and removing a source an event points at is already refused. **Phase 9 changes none of that.**

---

## Crate responsibilities

| Crate | Phase 9 contents |
|---|---|
| `video-coach-core` | `scoreboard.rs`: the on-disk types plus `interpret`, roles, the clock, `scoreboard_state`, `ScoreboardContext::state_at`, the derived back-anchor, and `MatchFormat`'s accessors. The event mutators. `SCOREBOARD_*` ratios in `layout.rs`. The `timeline.rs` doc amendment. |
| `video-coach-media` | The scoreboard drawn last in `overlay.rs`; a generalized `draw_text` (colour, alignment) and a second memo slot; the vendored bold face; `ScoreboardContext` on both jobs. |
| `video-coach-app` | Bus: the four commands, `EditMatchEvents` undo and its purge on source edits, building the context. UI: the three keys, the Match panel, and the setup sheet. |
| `video-coach-harness` | Tagging, deleting, undo across a source move, and the context reaching an export. |

## Testing

- **Core** (macOS has 651 lines to draw on): stoppage in both halves, HT and FT, quarters, overtime, the goal window, `interpret`'s tie-break and truncation, the roles map, `MatchFormat`'s names and labels, and **the derived back-anchor**, including that a back-anchored first period reaches stoppage correctly — which macOS's could not.
- **Media:**
  - properties, not golden images: drawn inside the bar ∪ tail rect, the area left of the bar untouched, the accent strip only over the team columns, the tail present only in stoppage;
  - **the pause test:** a clip with a mid-clip pause of N seconds shows the same clock at record time `p` and `p + N`. This is what pins BACKLOG #27 shut.
- **Harness:** tag, delete, undo; **undo after a source move doesn't restore a stale index**; an export whose context reaches the overlay.
- **Manual** (batched): tag a real match while scanning and check the clock against the footage.

## Risks

1. **The clock's correctness inside a clip** is the point of the phase, and it is one call. The pause test keeps it.
2. **Undo across source edits** is the subtle one; the purge is the fix, and the harness test is the guard.
3. **Two "draw nothing" cases** (not configured, nothing tagged yet) after validation moves to the command.

## Deferred

- A scoreboard over the scan picture (user decision).
- Manual clock offsets beyond the back-anchor.
- Per-event undo.
