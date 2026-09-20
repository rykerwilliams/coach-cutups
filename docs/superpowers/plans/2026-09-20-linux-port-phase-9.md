# Linux Port — Phase 9 Plan (Scoreboard)

**Date:** 2026-09-20
**Spec:** `docs/superpowers/specs/2026-09-20-linux-port-phase-9-design.md` (decisions S1–S5)
**Status:** Draft, pre-review.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the workspace and passes its tests on its own.

**Known facts. Don't re-derive these.**
- **Swift sources:** `apple/VideoCoachCore/Sources/VideoCoachCore/{ScoreboardState,MatchInterpret,MatchFormat,MatchEvent}.swift` and `Overlays/ScoreboardDraw.swift`; tests in `apple/VideoCoachCore/Tests/VideoCoachCoreTests/{ScoreboardTests,MatchFormatTests,ScoreboardRenderTests,CompilationPlannerScoreboardTests}.swift` (651 lines total). `apple/App/Views/Scoreboard/MatchInspectorPanel.swift` is the panel.
- **The layout ratios are fractions of `scoreBarH = barH − accentH`,** and the accent strip sits **above** the cells. The stoppage tail is **not** bold and is drawn **outside** the bar.
- **`overlay.rs`'s `fit` ellipsizes; it does not size-fit.** Only `width` measures. `Fitted` is a **single-slot** memo the text bar already uses. `draw_text` hardcodes white and left-alignment.
- **`layout.rs` already has `BAR_HEIGHT_RATIO = 0.08`** for the text bar. Name the scoreboard's constants `SCOREBOARD_*`.
- **`timeline.rs`'s module doc names itself the scoreboard's authority.** The code uses `FrameSpec::source_time` (from `playback_segments`), which differs by up to 50 ms at a freeze near a source's end, deliberately. Amend the doc, keep the 50 ms note.
- **`project.rs` already remaps `match_events` on source move and remove,** and `source_is_referenced` already refuses a remove. Don't touch it.
- **`app.slint`:** `handle-key` yields on `text-editing` (`name-edit.has-focus || inspector.editing`), the Esc cascade is recording → preview → selection, and `1/2/3` (zoom) deliberately omit `!event.repeat`. `z`, `x` and `v` are free.
- **Undo:** `UndoController::evict` already purges on source edits; add the new action to the same routine.

---

## Task 1 — Core: the scoreboard module

1. **Merge** `scoreboard_config.rs` into a new `scoreboard.rs`: the on-disk types plus the behaviour.
2. **Port** `interpret` (stable sort, input-order tie-break, truncation to `2 × total_periods`), returning `(Uuid, PeriodRole)`; `MatchFormat`'s derived accessors; `ClockDisplay`, `format_clock`; `scoreboard_state(now_abs, config, events) -> Option<ScoreboardState { home_score, away_score, clock }>`.
3. **The derived back-anchor:** `ScoreboardConfig.auto_back_anchor_p1: bool`; when set, `interpret` prepends a start at `p1_end_abs − period_seconds(0)`. **Remove `MatchEventRecord::is_auto_back_anchor`.**
4. `ScoreboardContext { config, events: Vec<AbsoluteMatchEvent>, source_offsets }` with `state_at(source_index, source_time)`; `Project::absolute_match_events()`.
5. The mutators on `Project`: `append_home_goal`, `append_away_goal`, `append_start_stop` (**no silent no-op**; the caller refuses at the cap), `set_auto_back_anchor_p1`.
6. `SCOREBOARD_*` ratios in `layout.rs`, per the spec's table, and the scoreboard added to that module's doc.
7. **Amend `timeline.rs`'s module doc** (the displayed frame's `source_time` is the clock's input).
8. **Tests,** drawing on macOS's 651 lines: stoppage in both halves, HT/FT, quarters, overtime, the goal window, the tie-break and truncation, the roles map, format names and labels, and **the derived back-anchor including that a back-anchored first period still reaches stoppage** (macOS's could not).

Commit: `feat(core): scoreboard clock, score and match interpretation`.

## Task 2 — Media: drawing it

1. **Vendor `DejaVuSans-Bold.ttf`** and its licence.
2. **Generalize `draw_text`** with colour and horizontal alignment; give the scoreboard its own memo slot (or key the memo by rect) so it doesn't thrash the bar's.
3. **Draw the scoreboard last** in `overlay.rs`, from `OverlayFrame.scoreboard: Option<(&ScoreboardConfig, &ScoreboardState)>`: the bar, the accent strip above the team columns, the four cells with their fills, the team names (fixed size, ellipsized), the score, the clock, and the stoppage tail outside the bar.
4. **`ScoreboardContext` on `ExportJob` and `PreviewJob`,** with both drivers calling `state_at(entry.source_index, frame.source_time)`.
5. **Tests:**
   - properties: drawn inside the bar ∪ tail rect, the area left of the bar untouched, the accent only over the team columns, the tail only in stoppage, each team's `font_color` used;
   - **the pause test:** a clip with a mid-clip pause of N seconds shows the same clock at record time `p` and `p + N` (this pins BACKLOG #27 shut);
   - a scoreboard-less job draws nothing.

Commit: `feat(media): draw the scoreboard`.

## Task 3 — Bus, UI and harness

1. **Bus:** `TagMatchEvent { kind, source_index, source_seconds }` (captured on the UI thread, with the `last_secs` fallback), `DeleteMatchEvent(Uuid)`, `SetScoreboard(Option<ScoreboardConfig>)` (**rejecting an empty team name**), `SetAutoBackAnchorP1(bool)`; `UndoAction::EditMatchEvents { before, after }`; **purge that action on every source add, move, remove and relink**, in `evict`; build the `ScoreboardContext` for both jobs.
2. **UI:**
   - `z`, `x`, `v` tag directly — with `!event.repeat`, yielding to text fields, gated to scanning or recording (never previewing, never during a recording's start-up);
   - a **Match panel** in the right-hand column: the live score and clock from the **scan anchor** (frozen while previewing), the three buttons with the same gate, the back-anchor toggle, and the event list with roles, seek and delete;
   - a **setup sheet** for team names, three colours each, and the format, with the shrink-below-tagged warning;
   - the new text fields folded into `text-editing`.
3. **Harness:** tag, delete, undo; **undo after a source move doesn't restore a stale index**; the context reaching an export.
4. **A screenshot pass** with a scratch project driven through callbacks (no camera, no input injection): the panel with events, and an exported frame's scoreboard.

Commit: `feat(app): match events and the scoreboard panel`.

## Task 4 — Closeout

1. Adversarial review of the Phase 9 diff; apply and backlog.
2. `CLAUDE.md`: the clock rule (the displayed frame's source time; no per-clip constant) in a line or two.
3. The hands-on checklist items, in the Task 4 notes.

## Deliberately not in this phase

- A scoreboard over the scan picture (user decision).
- Manual clock offsets; per-event undo.
