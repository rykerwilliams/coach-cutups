# Linux Port — Phase 3 Plan (Clips, Tags, Undo)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-3-design.md` (decisions cited as C1–C9)
**Status:** Draft, pre-review.

**Goal:** everything on the spec's "Done when" list works on the reference laptop.

**Execution.**
- Each task runs in a fresh subagent that is given this plan, the spec and `CLAUDE.md`.
- The orchestrator runs `verify` and commits per task.
- Every task must build the whole workspace, including the binary, and keep CI green.

**Known facts. Don't re-derive these.**
- **Slint 1.18** (tested with injected events, winit):
  - The root `FocusScope`'s `capture-key-pressed` runs **before** the focused `LineEdit`. If it accepts a key, the field never sees it.
  - `LineEdit` handles Ctrl+Z and Ctrl+Shift+Z itself. Ctrl+Y is redo only on Windows.
  - A `LineEdit`'s `key-pressed` can take Tab.
  - `TouchArea` in a `ListView` row gets `double-clicked`. A double-click also fires `clicked` twice first.
  - `ContextMenuArea` works on winit.
  - `PopupWindow.show()` takes focus from a `LineEdit`.
  - A click doesn't move focus off a `LineEdit`. Call `some-focus-scope.focus()` explicitly.
  - A field's `changed has-focus` handler runs **after** a click handler that changed selection.
  - A `text:` binding on a `LineEdit` breaks once the user types. Set the text imperatively, like the existing `project-name`/`saved-project-name` pattern in `app.slint` and `main.rs`.
- **Store.** `store::write` writes `.project.json.tmp` and renames it over `project.json`, so a read-only `project.json` does **not** fail a save. A read-only project folder does. `write` creates `recordings/` but not `.trash`.
- **Bus.**
  - The recording guard's allow-list is at the top of `Bus::command` (`bus/mod.rs`). New commands are refused while recording by default.
  - `project_changed()` saves and emits `ProjectChanged`, and reports a save failure without rolling back.
  - `reset_skip()` / `set_playing()` / `seek_abs()` are in `transport.rs`.
  - `Origin::Scrub` is the user-seek origin.
- **Existing keys.** `app.slint`'s `handle-key` (around lines 367–445) yields only for `name-edit.has-focus`.
- **Core.**
  - `tag.rs` has `normalize_tags`.
  - `Project::add_recorded_clip` computes `sort_index = max + 1`.
  - `Project::move_source`/`remove_source` remap live clips' `source_index`.

---

## Task 1 — Core: undo, clip order, tags

No GStreamer. Use the `port-swift-module` skill for the undo port. Read `apple/VideoCoachCore/Sources/VideoCoachCore/UndoController.swift`, `TagAggregation.swift` and their tests first.

1. **`undo.rs`** (C1): `ClipEdit`, `UndoAction`, `UndoController { undo, redo }` with `push`, `pop_undo`, `pop_redo`, `evict_delete` and `clear`, as the spec defines.
   - `ClipEdit::Tags` holds `Vec<String>`.
   - `STACK_CAP = 100`.
   - **Tests:** port `UndoControllerTests`, plus:
     - pushing a delete while a delete sits on redo: redo is cleared and that clip's edits on undo survive;
     - a second delete evicts the first and purges its edits;
     - a cap-dropped delete is returned and its edits purged;
     - `evict_delete`.
2. **Order** (C3) in `project.rs`:
   - `renumber(&mut self)`.
   - `apply_clip_order(&mut self, ids: &[Uuid])`: listed ids first, skipping missing ones; the rest in current order; renumber.
   - Pure `moved_order(&self, from, to) -> Vec<Uuid>` and `source_sorted_order(&self) -> Vec<Uuid>` (stable).
   - `remove_clip(&mut self, id) -> Option<Clip>`.
   - `insert_clip(&mut self, clip)`: at `min(sort_index, len)`, then renumber. A no-op if the id is present.
   - `apply_edit(&mut self, id, ClipEdit) -> Option<ClipEdit>`: returns the previous value of that field, or `None` if the clip is missing.
   - `add_recorded_clip` appends, and `sort_index = len`.
   - **`store::read`** sorts clips by `sort_index` (stable) and renumbers, with a comment on why (C3). Check that a round-trip test still holds; files written by this app are already normalized.
3. **Tags** in `tag.rs` (C8):
   - `tag_summaries(&[Clip]) -> Vec<TagSummary>`, alphabetical;
   - `tag_suggestions(all_tags: &[String], text: &str) -> Vec<String>`: the prefix of the last fragment, excluding every tag in `text` (normalized), exact matches and empties; up to 8, sorted;
   - `Project::all_tags() -> Vec<String>`: sorted and unique.
4. **Tests** for all of the above in `crates/video-coach-core/tests/`.

Commit: `feat(core): undo history, clip order, tag summaries and suggestions`.

## Task 2 — Bus: clip commands, trash, history

In `bus/clips.rs` (new), plus variants in `bus/mod.rs`. `main.rs` gets placeholder arms, so everything builds.

1. **Commands** (C5): `EditClip { id, edit: ClipEditInput }`, `DeleteClip(Uuid)`, `MoveClip { from, to }`, `SortClipsBySource`, `JumpToClip(Uuid)`, `Undo`, `Redo`.
   - `ClipEditInput` mirrors `ClipEdit`, but tags are raw `String`. The bus normalizes them.
2. **Event:** `Event::Select(Uuid)`.
3. **The `Bus` gains `history: UndoController`.**
4. **Edits:** `apply_edit`. If the old value equals the new one, skip. Otherwise `project_changed()`, then `push(EditClip)`.
5. **Order:** `MoveClip`/`SortClipsBySource` compute the target order. If it equals the current order, skip. Otherwise `apply_clip_order`, `project_changed()`, `push(ReorderClips)`.
6. **Trash** (C4):
   - `trash_clip(id) -> Result<Clip, UserError>` and `restore_clip(clip)`, as the spec defines.
   - **Delete command:**
     1. `trash_clip`, where a save failure rolls back: re-insert the clip, don't move the file, report the error, push nothing.
     2. `push(DeleteClip)`.
     3. Shred the returned clip's `.trash/<file>`.
   - **The trash path** is a helper: `recordings/.trash/<recording_filename>`. Shredding only ever uses it.
7. **Undo/redo:**
   - Pop, then apply the inverse (undo) or forward (redo) through the same primitives, with no push.
   - A redo-delete whose save fails doesn't move the file.
   - After an undo of an edit or delete, or a redo of an edit, emit `Select(id)` **after** the `ProjectChanged`.
8. **Source changes:** `MoveSource`/`RemoveSource` call `history.evict_delete()` and shred its file.
9. **Open:** after a successful open, `remove_dir_all(recordings/.trash)` (ignoring NotFound) and `history.clear()`.
10. **`JumpToClip`:**
    1. `reset_skip()`.
    2. `set_playing(false)`.
    3. `seek_abs(project.abs_seconds(clip.source_index, clip.start_source_seconds), true, Origin::Scrub)`.
11. **Harness tests** (`crates/video-coach-harness/tests/clips.rs`), the spec's Harness list.
    - Clips come from real recordings with `CaptureKind::Test`, or from `write_project` with clip entries plus small dummy files in `recordings/`, whichever is simpler. The trash tests only need files to exist.
    - **The save-failure test:** make the project **folder** read-only (`chmod 555`), and restore it in a drop guard.

Commit: `feat(app): clip editing, delete with trash, reorder and undo on the bus`.

## Task 3 — UI: list, keys, selection

In `app.slint` and `main.rs`.

1. **`text-editing` yield** (C6): one property ORing `has-focus` of every text field. `handle-key` returns `reject` for everything while it is true, after the error-dialog branch. Replace the existing name-field-only yield with it.
2. **Clips list rows** (C9):
   - selected highlight;
   - **`clicked` selects**, and never toggles;
   - `double-clicked` sends `JumpToClip`;
   - `ContextMenuArea` with "Jump to clip start" and "Delete clip";
   - drag-reorder, reusing the source list's DragArea/DropArea pattern, disabled while filtering → `MoveClip`;
   - a "Sort by position" header button, disabled with fewer than 2 clips.
   - Rows show "Untitled" for an empty name.
3. **Selection:**
   - UI state (`selected-clip: string` id, empty for none) in `main.rs`'s `UiState` and a Slint property.
   - Cleared on `ProjectOpened` and when missing after `ProjectChanged`.
   - `Event::Select(id)` sets it.
4. **Keys** (C6):
   - **Delete** → `DeleteClip(selected)`;
   - **Ctrl+Z** → `Undo`;
   - **Ctrl+Shift+Z** or **Ctrl+Y** → `Redo`;
   - **Esc** cascade: error dialog, stop recording, clear selection.
5. **Focus-off clicks:** clicks on list rows, the player area and empty sidebar space call the root `keys.focus()`.
6. **Guard:** the list's interactions and keys are disabled while `recording`, as in Phase 4.

Commit: `feat(app): clip selection, context menu, reorder and undo keys`.

## Task 4 — UI: inspector, tags, overview, filter

1. **Inspector panel** (C7): a right-hand column of about 280 px, disabled while recording.
   - With a selection:
     - Name `LineEdit`;
     - Tags `LineEdit` and suggestions;
     - "Show webcam in export" `CheckBox`;
     - Notes `TextEdit`.
   - Otherwise: the tag overview.
2. **Commit-against-id:**
   - Each field stores the clip id it was focused on (a `string` property set in `changed has-focus` when it gains focus).
   - It commits against that id on Enter (name, tags) or focus loss (all), sending `EditClip`.
   - The checkbox commits on toggle against the selected id.
   - Field text is set imperatively on selection change, and on `ProjectChanged` when the field isn't focused. On focus loss, a field re-renders from the current clip, since a skipped unchanged commit sends no `ProjectChanged`.
3. **Suggestions** (C8):
   - An overlay `Rectangle` (higher `z`, not a `PopupWindow`, not inside a `ScrollView`) under the tags field. It is shown while the field has focus and `tag_suggestions` is non-empty, and recomputed on `edited`.
   - **Tab** in the field's `key-pressed` takes the top suggestion. **Clicking** a row takes that one.
   - **Taking a suggestion** replaces the last fragment, appends `", "`, keeps focus and moves the cursor to the end.
   - **Esc** in the field closes the suggestions if shown; otherwise it focuses the root, which commits.
4. **Overview:**
   - `tag_summaries` rows ("tag", "count · duration"), plus the empty states;
   - clicking a row toggles `tag-filter`.
5. **Filter:**
   - a chip "Filtered: tag ✕" above the Clips list;
   - the list shows only matching clips;
   - "No clips tagged 'x'";
   - drag disabled;
   - cleared on `ProjectOpened`.
6. **Manual run** on the laptop, with `XDG_CONFIG_HOME` and a scratch project in the scratchpad.
   - Create 2–3 clips with a short recording each. This **opens the real camera and mic**; delete the media afterwards.
   - Alternatively, write a `project.json` with clips and dummy `.mkv` files, which is preferred since it needs no camera.
   - Screenshot the inspector, suggestions, overview and filter. Find the window by `_NET_WM_PID`.
   - Verify edits, undo, delete and restore in `project.json` and `recordings/.trash`.
   - Synthetic input only works if the screen isn't locked. If it is locked, **do not type into anything**: use a temporary env-var driver that invokes callbacks, and remove it afterwards.
   - Kill only your own PID.
   - Write "### Task 4 notes" with what was verified, plus checklist items for the user.

Commit: `feat(app): clip inspector, tag suggestions, overview and filter`.

## Task 5 — Closeout

1. Adversarial review of `git diff <plan commit>..HEAD -- crates`; apply the fixes and backlog any deferrals.
2. Add Phase 3 items to the user's batched hands-on checklist in the Task 5 notes:
   - click, double-click, the context menu, drag;
   - typing letters in every field fires no shortcut;
   - Ctrl+Z in and out of fields;
   - switching clips mid-edit;
   - Tab and click suggestions;
   - the filter;
   - Delete then undo restores the file.

## Deliberately not in this phase

- Clip preview on selection: Phase 7.
- Transcript: Phase 10.
- Orphan cleanup: BACKLOG #38.
- Coalescing, the Duration sort, ↑/↓ suggestions: #44.
- Multi-select: #45.
