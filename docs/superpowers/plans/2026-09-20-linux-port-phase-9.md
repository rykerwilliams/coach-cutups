# Linux Port — Phase 9 Plan (Scoreboard)

**Date:** 2026-09-20
**Spec:** `docs/superpowers/specs/2026-09-20-linux-port-phase-9-design.md` (decisions S1–S5)
**Status:** Reviewed (simplify and correctness passes applied).

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task. Every task builds the workspace and passes its tests on its own.

**Known facts. Don't re-derive these.**
- **Swift sources:** `apple/VideoCoachCore/Sources/VideoCoachCore/{ScoreboardState,MatchInterpret,MatchFormat,MatchEvent}.swift` and `Overlays/ScoreboardDraw.swift`; tests in `apple/VideoCoachCore/Tests/VideoCoachCoreTests/{ScoreboardTests,MatchFormatTests,ScoreboardRenderTests,CompilationPlannerScoreboardTests}.swift` (651 lines total). `apple/App/Views/Scoreboard/MatchInspectorPanel.swift` is the panel.
- **The layout ratios are fractions of `scoreBarH = barH − accentH`,** and the accent strip sits **above** the cells. The stoppage tail is **not** bold and is drawn **outside** the bar.
- **`AbsoluteMatchEvent` does not exist in Rust.** Swift's is a different shape (it carries a `clipIndex`). Define it in `scoreboard.rs` as `{ id: Option<Uuid>, kind: MatchEventKind, abs_seconds: f64 }` — `id` is `None` for the derived back-anchor start.
- **`overlay.rs`'s `fit` ellipsizes; it does not size-fit.** Only `width` measures. `Fitted` is a **single-slot** memo the text bar already uses. `draw_text` hardcodes white and left-alignment.
- **`layout.rs` already has `BAR_HEIGHT_RATIO = 0.08`** for the text bar. Name the scoreboard's constants `SCOREBOARD_*`.
- **`timeline.rs`'s module doc names itself the scoreboard's authority,** and **`plan.rs:58`'s `PlanEntry::record_time` doc makes the same wrong claim.** The code uses `FrameSpec::source_time` (from `playback_segments`), which differs by up to 50 ms at a freeze near a source's end, deliberately. Amend both, keep the 50 ms note.
- **`project.rs` already remaps `match_events` on source move and remove,** and `source_is_referenced` already refuses a remove. Don't touch it.
- **`app.slint`:** `handle-key` yields on `text-editing` (`name-edit.has-focus || inspector.editing`), the Esc cascade is recording → preview → selection, and `1/2/3` (zoom) deliberately omit `!event.repeat`. `z`, `x` and `v` are free.
- **Undo purge:** there is no `UndoController::evict`. The hook is `UndoController::evict_deletes`, reached through `evict_trashed_clips` in `bus/clips.rs:192`, called from `bus/sources.rs:66` and `:85` — and today it partitions the **undo** stack only. Phase 9 needs both stacks, so widen it.

---

## Task 1 — Core: the scoreboard module

1. **Merge** `scoreboard_config.rs` into a new `scoreboard.rs`: the on-disk types plus the behaviour. Re-export from `lib.rs` so callers don't churn.
2. **Port** `interpret`, `MatchFormat`'s derived accessors, `ClockDisplay`, `format_clock`, and `scoreboard_state(now_abs, config, events) -> Option<ScoreboardState { home_score, away_score, clock }>`.
   - `interpret(events, config) -> Vec<(Option<Uuid>, PeriodRole)>`: start/stops stably sorted by absolute time with an input-order tie-break, **truncated to `2 × total_periods`**, then — if `auto_back_anchor_p1` — a derived start prepended, so the anchor never costs a slot. Even indices start a period, odd ones end it.
   - **The derived start's time** is `p1_end_abs − period_seconds(0)` once a first end has been tagged, and **absolute 0 before then**. Without that fallback there is no clock at all through the first half.
3. **Remove `MatchEventRecord::is_auto_back_anchor`** — it touches `scoreboard_config.rs:93,178,189`, `core/tests/project_format.rs:73`, `core/tests/sources.rs:58` and `harness/tests/project_and_sources.rs:70`. Nothing writes it, serde ignores unknown keys, so **the format stays at v7**.
4. `AbsoluteMatchEvent` and `ScoreboardContext { config, events, source_offsets }`, with **`for_project(&Project) -> Option<Self>`** and `state_at(source_index, source_time)`.
5. **One mutator:** `Project::append_match_event(kind, source_index, source_seconds) -> Uuid` and `delete_match_event(id)`. **No silent no-op** at the cap; the caller refuses. The back-anchor flag rides on `set_scoreboard`.
6. **`scoreboard_rects(out_w, out_h) -> ScoreboardRects { bar, accent, home, score, away, clock, tail }`** in `layout.rs`, per the spec's table, with the ratios private to that module and the scoreboard named in its doc.
7. **Amend the two module docs** (`timeline.rs`, `plan.rs:58`).
8. **Tests,** drawing on macOS's 651 lines: stoppage in both halves, HT/FT, quarters, overtime, the goal window, the tie-break and truncation, the roles map, format names and labels, `scoreboard_rects`' geometry (cells tile the bar, accent above, tail outside), and **the pause test** — a schedule whose clip pauses for N seconds yields the same `state_at` clock at record time `p` and `p + N`. That is `compilation_schedule` plus `state_at`, pure core, and it pins BACKLOG #27 shut.
9. **The back-anchor tests:** before any end is tagged the clock runs from 0; once half-time is tagged the first period **ends at exactly `period_seconds(0)`**; it **never enters stoppage** (that is what the anchor means — macOS's read 50:00 while still "running", and that test is *not* ported); and a full-capacity match plus the anchor keeps every tagged event.

Commit: `feat(core): scoreboard clock, score and match interpretation`.

## Task 2 — Media: drawing it

1. **Vendor `DejaVuSans-Bold.ttf`** and its licence. Both faces load under one family, so **set `Attrs::weight` explicitly** everywhere — bold for the four labels, normal for the tail.
2. **Generalize `draw_text`** with colour and horizontal alignment (all five labels are **centred**), and **turn the `Fitted` memo into `[Option<Fitted>; 3]` indexed by `TextSlot { Bar, HomeName, AwayName }`** — the two names share a size and width, so one slot would still thrash.
3. **Draw the scoreboard last** in `overlay.rs`, from `OverlayFrame.scoreboard: Option<(&ScoreboardConfig, &ScoreboardState)>`, using `scoreboard_rects`: the accent strip in `secondary_color` above the team columns, the home and away cells filled with `primary_color`, the score and clock cells in their dark fills, the names ellipsized in each team's `font_color`, and the tail outside the bar when in stoppage.
4. **`scoreboard: Option<ScoreboardContext>` on `ExportJob` and `PreviewJob`,** both drivers calling `state_at(entry.source_index, frame.source_time)`. **The bus passes `None` in this task** so it builds and ships alone; Task 3a fills it in.
5. **Tests:** drawn inside the bar ∪ tail rect with the area left of the bar untouched; the accent only over the team columns; the tail only in stoppage; each team's `font_color` used; a scoreboard-less job draws nothing.

Commit: `feat(media): draw the scoreboard`.

## Task 3a — Bus and harness

1. **Commands:** `TagMatchEvent { kind, source_index, source_seconds }` (caller-captured per the bus contract), `DeleteMatchEvent(Uuid)`, `SetScoreboard(ScoreboardConfig)` — which carries `auto_back_anchor_p1`, **rejects an empty team name**, and refuses a tag at the cap.
2. `UndoAction::EditMatchEvents { before, after }`.
3. **Purge that action from both stacks on a source move or remove,** by widening `evict_trashed_clips`/`evict_deletes` (`bus/clips.rs:192`, called from `bus/sources.rs:66` and `:85`) to cover the redo stack and this action. **Add and relink don't need it** — events key on `(source_index, source_seconds)` and neither permutes indices. Rename the routine for what it now does.
4. **Build the `ScoreboardContext` for both jobs** via `for_project`, replacing Task 2's `None`.
5. **Harness:** tag, delete, undo; **undo after a source move doesn't restore a stale index**; the context reaching an export.

Commit: `feat(app): match event commands and undo`.

## Task 3b — UI

1. **`z`, `x`, `v` tag directly,** gated exactly as `pressed && !event.repeat && !root.previewing && root.recording-phase != RecordingPhase.starting && root.can-play`, yielding to `text-editing`.
2. **A Match panel** in the right-hand column: the live score and clock, the three buttons under the same gate, and the event list with roles, seek and delete.
   - **Its anchor is the position the readout already computes** — `target_abs` while a seek is outstanding, else `abs_seconds(source_index, last_secs)` — mapped back through `locate()`. Reading `source_index` and `last_secs` separately pairs a new index with an old offset across a cross-source seek. Frozen while previewing.
3. **A setup sheet** for team names, the back-anchor toggle, the format, and the six colours as **hex text fields** (no picker; Slint 1.18 has none, and a field is what the Mac's inspector effectively was), with the shrink-below-tagged warning.
4. **Its own branch in `handle-key`** — the sheet is modal, so Esc closes it ahead of the existing cascade and the tag keys don't fire behind it. New fields fold into `text-editing`.
5. **A screenshot pass** with a scratch project driven through callbacks (no camera, no input injection): the panel with events, and an exported frame's scoreboard.

Commit: `feat(app): the Match panel and scoreboard setup`.

## Task 4 — Closeout

1. Adversarial review of the Phase 9 diff; apply and backlog.
2. `CLAUDE.md`: the clock rule (the displayed frame's source time; no per-clip constant) in a line or two.
3. The hands-on checklist items, in the Task 4 notes.

## Deliberately not in this phase

- A scoreboard over the scan picture (user decision).
- Manual clock offsets; per-event undo.
