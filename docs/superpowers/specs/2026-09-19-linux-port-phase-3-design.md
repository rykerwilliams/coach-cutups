# Linux Port — Phase 3: Clips, Tags and Undo

**Date:** 2026-09-19
**Status:** Draft, pre-review
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phasing → Phase 3, and the execution-order note: it runs after Phase 4)
**Builds on:** Phase 2 (bus, sidebar, keyboard) and Phase 4 (recorded clips, the Clips list, the recording guard)
**Evidence:** the macOS inventory of `UndoController.swift`, `Workspace.swift` (delete, undo, reorder, sort, trash), `ClipSidebar.swift`, `ClipInspector.swift`, `TagField.swift`, `TagAggregation.swift` and `ClipCommands.swift`.

---

## Goal

A coach can manage their clips:
- select one;
- edit its name, tags, notes and "show webcam in export" flag;
- reorder the list by hand or sort it by position in the game;
- see every tag with its clip count and total length, and filter the list by one;
- jump the game video to a clip's start;
- delete a clip, and undo or redo any of these.

Selecting a clip does **not** open a preview: preview is Phase 7. Until then, selection drives the inspector only.

## Done when

1. **Selection.** Clicking a clip selects it, and the right-hand inspector shows its name, tags, notes and PiP flag. With nothing selected, the inspector shows the tag overview.
2. **Editing.** Edits save to `project.json`. Each committed field is one undo step: Ctrl+Z undoes it, and Ctrl+Shift+Z or Ctrl+Y redoes it.
3. **Delete.** Delete (with the list focused), or the context menu, removes the clip. Its recording moves to `recordings/.trash/`, and Ctrl+Z brings both back.
4. **Order.**
   - Dragging clips reorders them.
   - "Sort by position in video" orders them by source, then start time.
   - Both are undoable.
5. **Tags.**
   - Tags are normalized (lowercase, trimmed, deduped).
   - The tag field suggests existing tags.
   - Clicking a tag in the overview filters the list to it. Esc or the chip's ✕ clears the filter.
6. **Jump.** "Jump to clip start" pauses the game video at the clip's source position.
7. **Trash on open.** Opening a project empties `recordings/.trash/` and the undo history.
8. **Recording guard.** None of this is possible while recording.

---

## Decisions

### C1. `UndoController` is ported to core, with its eviction contract

`video-coach-core/src/undo.rs` ports `UndoController.swift`. It stays pure: no I/O.

```rust
pub enum UndoAction {
    EditClip { id: Uuid, before: ClipFields, after: ClipFields },
    DeleteClip(DeletedClip),
    ReorderClips { before: Vec<Uuid>, after: Vec<Uuid> },
}
pub struct DeletedClip { pub clip: Clip }   // the file is always recordings/.trash/<recording_filename>
```

**Pushing.**
- Both stacks are capped at 100 (`STACK_CAP`), dropping from the front.
- `push_edit` and `push_reorder` append and clear redo.
- `push_delete` works like macOS:
  1. It evicts the one prior `DeleteClip`, from either stack. **At most one delete exists across both stacks**, so `.trash` holds at most one file.
  2. It appends, and clears redo.
  3. It returns the evicted `DeletedClip`, so the caller can shred its file.

**Two fixes to the Swift behavior:**
- **A cap overflow that drops a `DeleteClip`** also returns it for shredding (`push_*` return `Option<DeletedClip>`). macOS left that file until the next open.
- **When a delete is evicted, every `EditClip` for that clip id is dropped from both stacks.** The clip can never return, so those entries could only no-op. macOS kept them, and Ctrl+Z silently consumed them.
  - `ReorderClips` entries stay: applying an order skips missing ids.

**Undo and redo.** `pop_undo` / `pop_redo` move an action between the stacks. `clear()` empties both.

**Tests:** port `UndoControllerTests`, plus the two fixes.

### C2. What an edit snapshot covers: `ClipFields`, not the whole clip

`ClipFields { name, tags, notes, show_pip }` are the fields a user edits in the inspector.

macOS snapshotted the whole `Clip`, so undoing an old edit also restored a stale `sortIndex`, which could quietly undo a later reorder. Snapshots of only the editable fields make edit, reorder and delete independent. `transcript` joins `ClipFields` in Phase 10.

### C3. Every mutation goes through the bus

Clip state lives in the `Project` the bus owns. Selection and the tag filter are **UI state** (the parent spec's "Workspace is project data only"), not bus state.

**New commands:**
- `EditClip { id, edit: ClipEdit }`, where `ClipEdit` is one of:
  - `Name(String)`;
  - `Tags(String)`: raw text, which the bus normalizes;
  - `Notes(String)`;
  - `ShowPip(bool)`.
- `DeleteClip(Uuid)`.
- `MoveClip { from: usize, to: usize }`: positions in the sorted list.
- `SortClipsBySource`.
- `JumpToClip(Uuid)`.
- `Undo`, `Redo`.

**Each mutation** runs one sequence:
1. It builds the new state.
2. It skips if nothing changed (the macOS `before == after` check).
3. It saves.
4. It pushes the undo action and emits `ProjectChanged`.

The Phase 4 recording guard already refuses all of these while recording, because they aren't on its allow-list.

**`Event::Select(Option<Uuid>)`** is emitted after an undo or redo that should move the selection:
- undoing an edit or a delete selects that clip;
- redoing an edit selects it.

The UI also drops a selection whose clip is no longer in the project, on every `ProjectChanged`. That covers redoing a delete without a special case.

### C4. Delete and restore are ordered for crash safety

**Delete:**
1. Remove the clip from the project.
2. **Save.** If the save fails, put the clip back and report the error. Nothing has moved.
3. Move `recordings/<file>` to `recordings/.trash/<file>`, replacing any file of that name there. A missing recording is tolerated.
4. `push_delete`. Shred any file it evicts.

**Restore (undo of a delete):**
1. Move the file back, if it's in `.trash`.
2. Reinsert the clip at its original `sort_index`.
3. Save.

**Redo of a delete** repeats the delete steps.

**Why this order.** macOS moved the file first and saved second, so a crash in between left `project.json` listing a clip whose recording the next open's shred-on-open deleted. That is a referenced clip with no recording. With save first, a crash leaves at worst an **unreferenced** recording in `recordings/`: an orphan, which is harmless and already tracked by BACKLOG #38.

**`sort_index` on restore.** If another clip now holds the restored clip's `sort_index` (after a reorder or a new recording), the clips at or above it shift up by one. That keeps the restored clip where it was relative to its old neighbours, and keeps indices unique. macOS could produce ties that sorted unpredictably.

### C5. Trash and history are cleared at project open

Opening a project removes `recordings/.trash/` entirely and clears the undo history. Undo is in-memory only, so a trashed file from an earlier session can never be restored (macOS parity).

Orphaned recordings in `recordings/` are **not** touched. Deleting user media nothing references is a separate decision; it stays in BACKLOG #38.

### C6. Reorder and sort

- **`MoveClip { from, to }`:**
  - reorders the clips by `sort_index`, moves one, and renumbers 0..n-1;
  - skips if the order is unchanged;
  - pushes `ReorderClips` with the id lists.
- **`SortClipsBySource`:** sorts by `(source_index, start_source_seconds)`, stable, and renumbers. It is also a `ReorderClips`, and is skipped if unchanged.
- **Applying an order** (undo or redo): ids in the snapshot order first, skipping ids that no longer exist, then any remaining clips in their current order. Then renumber (the macOS `applyClipOrder`).
- **UI:**
  - Drag-to-reorder in the Clips list, using Phase 2's source-list drag pattern.
  - Drag is disabled while a tag filter is active: positions in a filtered list don't map to the full order (macOS parity).
  - A "Sort by position" button in the Clips header, disabled with fewer than 2 clips.

### C7. Selection, jump, context menu, keys

**Selection:**
- Single selection, UI state only.
- Clicking a row selects it; clicking it again, or Esc, deselects it.
- Selection is cleared on project open.

**Jump to clip start:**
- `JumpToClip(id)` seeks the game video to the clip's `(source_index, start_source_seconds)` and pauses. It reuses the Phase 2 seek path with `Origin::System` and an accurate seek.
- Available from the context menu and by **double-clicking** a row. macOS had only the context menu; double-click is the common Linux idiom for "go to".
- Disabled when any source is missing, like every seek.

**Context menu** on a clip row: "Jump to clip start" and "Delete clip" (Slint `ContextMenuArea`).

**Keys**, through the root `FocusScope`:
- **Delete** deletes the selected clip. It is ignored while a text field has focus, since the field needs it.
- **Ctrl+Z** undoes. **Ctrl+Shift+Z** and **Ctrl+Y** redo.
  - While a text field has focus, these go to the field's own text undo. App undo is for committed actions.
  - Linux has no global Edit menu to arbitrate this, so "field focused → field wins" is the rule.
- **Esc** cascade: stop recording, then clear the selection, then clear the tag filter.
- There is no delete confirmation: undo is the recovery (macOS parity).

### C8. Inspector

A right-hand panel, about 280 px, next to the player.

**With a clip selected:**
- **Name:** a `LineEdit`. It commits on Enter or focus loss.
- **Tags:** C9.
- **"Show webcam in export":** a checkbox. It commits on toggle, and is the backlog #42 PiP checkbox.
- **Notes:** a `TextEdit`. It commits on focus loss.

**Commit rules:**
- Each commit sends one `EditClip` with the field's full value. The bus diffs against the clip, so there is one undo step per field session and none if nothing changed (macOS parity: no coalescing).
- A selection change first commits any focused field to the **old** clip, as macOS did by tearing down the editor.
- An empty name is allowed. The list shows "Untitled" for it.

**Transcript** is Phase 10. There is no `summary` field: the port dropped AI summaries.

### C9. Tags

**Normalization** is the existing core `normalize_tags`: split on commas, trim, lowercase, drop empties, and dedupe keeping first-seen order.

**The tag field** is a comma-separated `LineEdit` that commits on Enter or focus loss. The bus normalizes, and the field re-renders `", "`-joined.

**Suggestions:**
- **Matching:** existing project tags that are prefix matches of the fragment after the last comma. Exclude exact matches and tags already on the clip. Show up to 8, sorted.
- **Display:** a small list under the field while it is focused and the fragment is non-empty.
- **Keys:** ↑/↓ highlight; **Tab** takes the highlighted suggestion, or the top one; clicking takes one.
- **Taking a suggestion** replaces the fragment, appends `", "` and keeps focus.
- **Computed** in pure Rust (`tag_suggestions(all_tags, text) -> Vec<String>`), tested in core.

**The tag overview** is shown when nothing is selected:
- **Source:** `TagAggregation`, ported to core as `tag_summaries(&[Clip]) -> Vec<TagSummary { tag, clip_count, total_seconds }>`, sorted alphabetically.
- **Rows:** "tag", then "count · duration".
- **Sort toggle:** A–Z or Duration (descending, ties alphabetical). UI state.
- **Empty states:** "No clips yet" and "No tags yet — add tags to a clip in the inspector."

**The filter:**
- One tag at most, UI state.
- Clicking an overview row toggles it; a chip "Filtered: tag ✕" above the Clips list clears it.
- The list shows clips whose tags contain the tag, and an empty filtered list says "No clips tagged 'x'".
- The filter is cleared on project open.
- If the selected clip stops matching, the selection is kept but hidden. macOS behaved the same way; that is acceptable because the inspector still shows the clip.

### C10. The Clips list rows

Each row shows the name (or "Untitled") and the duration (`format_hms`), ordered by `sort_index`, with the selected row highlighted. Tags are not shown on the row (macOS parity), because the inspector and filter cover them.

---

## Crate responsibilities

| Crate | Phase 3 contents |
|---|---|
| `video-coach-core` | `undo.rs`: `UndoController`, `UndoAction`, `DeletedClip`, `ClipFields`. `tag.rs` additions: `tag_summaries`, `tag_suggestions`. `project.rs` additions: `clip_fields`/`set_clip_fields`, `move_clip`, `sort_clips_by_source`, `apply_clip_order`, `remove_clip`, `restore_clip` (with the `sort_index` shift). |
| `video-coach-media` | Nothing. |
| `video-coach-app` | Bus: the clip commands, the undo history, trash moves and shredding, clearing at open, `Event::Select`. UI: selection, inspector, tag field with suggestions, tag overview and filter, context menu, drag reorder, sort button, keys. |
| `video-coach-harness` | Edit/undo/redo; delete/undo/redo with files moving; eviction shredding; a crash-order check (save fails → the file isn't moved); reorder and sort undo; jump; clearing at open; the recording guard. |

## Testing

- **Core:**
  - the ported undo tests, plus the eviction purge and cap-evicted delete;
  - the order functions, including restore with a `sort_index` collision;
  - `tag_summaries` (counts, durations, order);
  - `tag_suggestions` (prefix, exclusions, cap, fragment after the last comma).
- **Harness** (test sources, temp projects):
  - an edit of each field → saved, one undo step, redo;
  - an unchanged edit → no undo step;
  - delete → the file is in `.trash`, not in the project → undo → the file is back and the clip is at its `sort_index` → redo → trashed again;
  - two deletes → the first trashed file is gone, and only the second is restorable;
  - delete when the save fails → the clip remains and the file hasn't moved. Make `project.json` read-only, or the folder read-only, and restore it after;
  - reorder and sort → undo restores the order;
  - edits to a clip whose delete was evicted are gone from undo;
  - `JumpToClip` → paused at the clip's source position;
  - open project → `.trash` removed and undo empty;
  - clip commands refused while recording.
- **Manual** (batched): click, double-click, context menu, drag, the keys (including Ctrl+Z inside a text field vs outside), suggestions with Tab/↑/↓, and the filter chip.

## Risks

1. **Slint at this UI complexity.** This is the parent spec's risk 7, first tested here: the inspector, suggestions popup and context menu. If Slint fights it, note where, and simplify the UX (e.g. suggestions without ↑/↓) rather than contort the code.
2. **Text-field undo vs app undo.** "Field focused → field wins" relies on Slint's `LineEdit`/`TextEdit` handling Ctrl+Z themselves. Verify this in Slint 1.18; if they don't, the app's Ctrl+Z must not fire while one is focused.
3. **`ContextMenuArea` in 1.18.** Verify it exists and works with the winit backend. Otherwise, a small popup on right-click.

## Deferred (→ BACKLOG)

- Orphaned-recording cleanup (#38, unchanged): not auto-deleting user media is deliberate.
- Undo coalescing (#11), and a menu bar with Edit → Undo (#32).
- Multi-select and bulk tag edits.
- Previewing a clip on selection: Phase 7.
