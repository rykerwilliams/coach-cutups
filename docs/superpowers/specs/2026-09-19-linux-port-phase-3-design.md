# Linux Port — Phase 3: Clips, Tags and Undo

**Date:** 2026-09-19
**Status:** Reviewed (simplify and correctness passes applied; the Slint facts were tested against 1.18 with injected key and pointer events)
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
3. **Typing is safe.** While a text field has focus, no app shortcut fires. Typing "r" in a name doesn't start a recording.
4. **Delete.** The Delete key (with no text field focused), or the context menu, removes the clip. Its recording moves to `recordings/.trash/`, and Ctrl+Z brings both back to the same place in the list, for any number of deletes.
5. **Order.**
   - Dragging clips reorders them.
   - "Sort by position" orders them by source, then start time.
   - Both are undoable.
6. **Tags.**
   - Tags are normalized.
   - The tag field suggests existing tags.
   - Clicking a tag in the overview filters the list to it; clicking it again, or the chip's ✕, clears the filter.
7. **Jump.** Double-clicking a clip, or its context menu, pauses the game video at the clip's start.
8. **Trash on open.** Opening a project empties `recordings/.trash/` and the undo history.
9. **Recording guard.** None of this is possible while recording. The inspector is disabled then.

---

## Decisions

### C1. `UndoController` is ported to core — with multi-level delete undo

`video-coach-core/src/undo.rs` ports `UndoController.swift`. It is pure: no I/O.

```rust
pub enum ClipEdit { Name(String), Tags(Vec<String>), Notes(String), ShowPip(bool) }
pub enum UndoAction {
    EditClip { id: Uuid, before: ClipEdit, after: ClipEdit },  // one field; same variant
    DeleteClip(Clip),              // its file is recordings/.trash/<recording_filename>
    ReorderClips { before: Vec<Uuid>, after: Vec<Uuid> },
}
impl UndoController {
    pub fn push(&mut self, a: UndoAction) -> Vec<Clip>;       // clears redo; returns evicted deletes
    pub fn take_undo(&mut self) -> Option<UndoAction>;        // removes; caller applies, then…
    pub fn file_redo(&mut self, a: UndoAction);               // …files it on redo
    pub fn take_redo(&mut self) -> Option<UndoAction>;
    pub fn file_undo(&mut self, a: UndoAction);               // files it on undo (no redo clear)
    pub fn evict_deletes(&mut self) -> Vec<Clip>;             // for source changes (C4)
    pub fn clear(&mut self);
}
```

**Any number of deletes can be undone.** This is a **deliberate change from macOS**, which kept at most one delete in its history and shredded the previous file on every delete. Multi-level undo is what Ctrl+Z means everywhere else, and it removes macOS's eviction-on-delete machinery.

The cost: `.trash` holds one recording per undoable delete until the project is next opened, when it's emptied (C4). *Confirmed by the user, 2026-09-19.*

**`push`:**
1. Clear redo.
2. Append.
3. Enforce the cap of 100 by dropping from the front.

It returns any `DeleteClip` the cap drops. The caller shreds its file.

**Eviction** is one routine, used by the cap and by `evict_deletes`: remove the delete, and **purge every `EditClip` for that clip id** from both stacks. The clip can never return, so those entries could only no-op. macOS kept them, and Ctrl+Z silently consumed them.

**Take and file.**
- `take_undo`/`take_redo` hand the action to the bus, which applies it and then files it with `file_redo`/`file_undo`. A failed undo is put back with `file_undo`.
- The bus may file an **updated** action. A redo of a delete files the clip exactly as it was when trashed, with source indices remapped since and any edits that saved without pushing. Re-filing the old snapshot would restore stale data.
- An action whose target is gone (can't happen once eviction purges, but defensively) is dropped: not filed.

**Tests:**
- port `UndoControllerTests`, except the Swift one-delete eviction tests (`test_pushDelete_evicts_*`), which are deliberate divergences;
- multi-delete undo;
- the cap-dropped delete and its purge;
- `evict_deletes` and its purge;
- take and file with an updated action.

### C2. Edits snapshot one field

An `EditClip` holds the old and new value of **the one field** the user committed.

Undoing an edit therefore can't touch anything else:
- not the order: macOS snapshotted the whole `Clip`, including `sortIndex`, so an old edit's undo could quietly revert a later reorder;
- not a Phase 10 AI transcript write, which saves but never pushes undo.

`Transcript` becomes a `ClipEdit` variant in Phase 10.

### C3. Clip order: stored order is the order

`project.clips` is kept sorted, with `sort_index == position`:
- **On read:** `store::read` sorts by `sort_index` (stable) and renumbers 0..n-1. This is a decode-time normalization, consistent with "migration at decode time".
- **After every mutation:** one `renumber()`.

Every order operation is then a `Vec` operation plus `renumber()`:

| Operation | What it does |
|---|---|
| Move | remove, then insert |
| Sort | a stable `sort_by (source_index, start_source_seconds)` |
| Restore | `insert(min(clip.sort_index, len))` |
| Add a recorded clip | append (Phase 4's `max + 1` becomes `len`) |

There are no ties, gaps or collisions for export (Phase 8) to meet.

**`apply_clip_order(&[Uuid])`** is the one order mutation:
1. ids in the given order first, skipping any that no longer exist;
2. then any remaining clips, in their current order;
3. renumber.

`MoveClip` and `SortClipsBySource` compute a target id order with pure functions and apply it through `apply_clip_order`, the same path undo and redo use. Each is skipped if the order is unchanged.

### C4. Delete, restore, and the trash

**Two primitives on the bus** (both no-ops if the clip is already gone, or already present):
- **`trash_clip(id)`:**
  1. Remove the clip from the project.
  2. **Save.**
  3. Create `recordings/.trash/` if needed.
  4. Move `recordings/<file>` to `recordings/.trash/<file>`, replacing any file of that name there. A missing recording is tolerated.
- **`restore_clip(clip)`:**
  1. Move the file back, if it's in `.trash`.
  2. Insert the clip at its `sort_index` (C3).
  3. Save.

**Command paths:**

| Action | Steps |
|---|---|
| `DeleteClip` command | `trash_clip`, then `push(DeleteClip)`, then shred any returned clips' `.trash/<file>` |
| Undo of a delete | `restore_clip`, then `file_redo` (or `file_undo` if the file couldn't be moved back) |
| Redo of a delete | `trash_clip` (no push), then `file_undo` with the clip `trash_clip` removed |

**Why save first on delete.** macOS moved the file first, so a crash in between left `project.json` listing a clip whose recording the next open's shred-on-open deleted. With save first, a crash leaves at worst an **unreferenced** recording in `recordings/`: an orphan, which is harmless (BACKLOG #38). The restore order (move back, then save) is safe the same way: the reverse order would let the open-time shred delete a referenced file.

**Save failures** follow the existing `project_changed` convention everywhere: the in-memory change stands, the error is reported, and the action is pushed.
- The one delete-specific rule: **if the save failed, `trash_clip` doesn't move the file.**
- So `project.json` can never list a clip whose file is in `.trash`, where the next open would wipe it.
- The worst case is an orphan in `recordings/`.

**Shredding** only ever touches `recordings/.trash/<file>`, never `recordings/`. Moves and shreds ignore `NotFound`.

**Source changes invalidate trashed clips.** `MoveSource` and `RemoveSource` renumber live clips' `source_index`, but trashed clips aren't live. Restored later, one would point at the wrong video or past the end of the list; macOS had this bug.
- So after a **successful** source change (not a refused remove, not `from == to`), the bus calls `evict_deletes()` and shreds their files.
- Source changes are rare, and edits and reorders hold no source indices.
- A delete on the redo stack is a live clip, and is refreshed when redone.

**At project open**, on every path including the startup restore (`Bus::commit`): remove `recordings/.trash/` entirely, and the previous project's too, and clear the history. Undo is in-memory only (macOS parity). Orphans in `recordings/` are not touched, which stays BACKLOG #38; update its "When to revisit".

`write` doesn't fsync, so after a power loss the two renames could land out of order. That is noted, not handled.

### C5. Commands and events

**New commands:**
- `EditClip { id, edit: ClipEdit }`, where tags arrive normalized (the UI normalizes the field's text);
- `DeleteClip(Uuid)`;
- `MoveClip { from, to }`;
- `SortClipsBySource`;
- `JumpToClip(Uuid)`;
- `Undo`, `Redo`.

**Each mutation:**
1. Diff: skip it if unchanged, so no undo step and no save.
2. Apply.
3. Save via `project_changed`.
4. Push.

The Phase 4 recording guard refuses all of these while recording, because they aren't on its allow-list, except `EditClip`: a metadata edit can't disturb a recording, and a field's focus-loss commit arrives after the `ToggleRecording` that took its focus.

**`JumpToClip`:**
1. `reset_skip()`, so a pending skip debounce can't move the video afterwards.
2. `set_playing(false)`.
3. An accurate seek to the clip's `(source_index, start_source_seconds)` with `Origin::Scrub`: it's a user seek, like a scrub release.
4. `seek_abs` already refuses when a source is missing.

**`Event::Select(Uuid)`:** the UI can't tell from a snapshot what an undo touched, so after an undo that restores or edits a clip, or a redo of an edit, the bus sends `Select(id)`.
- It goes **after** that operation's `ProjectChanged`, because the UI drops a selection whose clip is missing on every `ProjectChanged`.
- That same rule clears the selection when a redo deletes the selected clip.

### C6. Keys and focus

**While any text field has focus, the root key handler takes nothing.**
- Slint's root `capture-key-pressed` sees every key **before** the focused field (tested). Today it yields only for the project-name field, so typing "a" in a new field would skip the video, and "r" would start recording.
- A single `text-editing` property ORs the `has-focus` of every text field: project name, clip name, tags, notes. `handle-key` rejects everything while it is true, and the error-dialog branch stays first.
- Fields then do their own editing and text undo. Ctrl+Z and Ctrl+Shift+Z work in a `LineEdit` (tested). Ctrl+Y is not a text redo on Linux (tested), so it does nothing in a field.

**App keys, with no field focused:**
- **Delete:** delete the selected clip.
- **Ctrl+Z:** undo. **Ctrl+Shift+Z** or **Ctrl+Y:** redo.
- **Esc cascade:** error dialog, then stop recording, then clear the selection.
  - Inside a field, Esc first closes the suggestions, then leaves the field, which commits it.
  - That is the only keyboard way out of the notes field.
- The filter is cleared with its chip or its overview row, not Esc.

**Moving focus off fields.** Clicks on list rows, the player and empty space call `keys.focus()` to take focus off a field. Slint doesn't move focus on a click by itself (tested).

### C7. Selection and the inspector

**Selection:**
- Single, UI state only.
- **A click always selects.** Clicking again doesn't deselect, because a double-click is two clicks and would select, deselect, then jump (tested). Esc deselects.
- Cleared on project open, and when its clip leaves the project.

**Double-click** a row to jump (C5). The context menu (Slint `ContextMenuArea`, which works on winit, tested) offers "Jump to clip start" and "Delete clip".

**The inspector** is a right-hand panel of about 280 px, disabled while recording. With a clip selected it shows:
- **Name:** a `LineEdit`. Commits on Enter or focus loss.
- **Tags:** see C8.
- **"Show webcam in export":** a checkbox. Commits on toggle. It is backlog #42's PiP checkbox.
- **Notes:** a `TextEdit`. Commits on focus loss.

An empty name is allowed; the list shows "Untitled". Transcript is Phase 10.

**Commit rules** (Slint-specific, tested):
- **Commit against the clip being edited.** Each field records its clip's id when it gains focus, and commits against **that** id. A row click doesn't take focus from a field, and the field's focus-loss handler runs after the selection has changed, so "the selected clip" at commit time can already be the new one.
- **Set field text imperatively.** It is set on selection change and on `ProjectChanged` when the field isn't focused, as the existing project-name field does. A `text:` binding breaks as soon as the user types.
- **A commit the bus skips as unchanged** produces no `ProjectChanged`, so the field re-renders from the clip on focus loss itself. For example, "A, a" normalizes to the existing "a".

### C8. Tags

**Normalization** is the existing `normalize_tags`: split on commas, trim, lowercase, drop empties, and dedupe keeping first-seen order.

**The tag field** is a comma-separated `LineEdit` that commits on Enter or focus loss, re-rendered `", "`-joined.

**Suggestions:**
- **What is suggested:** `tag_suggestions(all_tags, text)` in core. It takes existing project tags that are prefix matches of the fragment after the last comma, excludes tags already present in the field's **text** (macOS parity), and returns up to 8, sorted.
- **Where they appear:** an overlay `Rectangle` in the main tree, with a higher `z`, under the field while it has focus and the fragment is non-empty.
  - It is not a `PopupWindow`: showing one takes focus from the field (tested), which would commit it mid-typing.
  - It is also kept out of any `ScrollView`/`ListView`, which would clip it.
- **Taking a suggestion:** **Tab** takes the top one; **clicking** takes any.
  - It replaces the fragment, appends `", "` and keeps focus. It doesn't commit: the field commits on Enter or focus loss as usual.
  - There is no ↑/↓ highlight: it is the most Slint-hostile part, and typing another letter narrows the list. Tab is caught in the field's `key-pressed` (tested).

**The tag overview** is shown when nothing is selected:
- **Source:** `tag_summaries(&[Clip]) -> Vec<TagSummary { tag, clip_count, total_seconds }>` in core, sorted alphabetically (the `TagAggregation` port).
- **Rows:** "tag", then "count · duration".
- **Empty states:** "No clips yet" and "No tags yet — add tags to a clip in the inspector."
- The A–Z/Duration toggle is deferred.

**The filter:**
- One tag at most, UI state.
- Clicking an overview row toggles it. A chip, "Filtered: tag ✕", above the Clips list clears it.
- The list shows the clips carrying the tag, and an empty filtered list says "No clips tagged 'x'".
- Drag-reordering is disabled while filtering: positions in a filtered list don't map to the full order.
- Cleared on project open.
- A selected clip that stops matching stays selected but hidden, and the inspector still shows it (macOS parity).

### C9. The Clips list

- **Rows:** the name (or "Untitled") and the duration (`format_hms`), with the selected row highlighted.
- **Order:** drag to reorder, using Phase 2's source-list pattern.
- **Sort:** a "Sort by position" button in the header, disabled with fewer than 2 clips.
- Tags aren't shown on rows (macOS parity).

---

## Crate responsibilities

| Crate | Phase 3 contents |
|---|---|
| `video-coach-core` | `undo.rs` (`UndoController`, `UndoAction`, `ClipEdit`). `tag.rs`: `tag_summaries`, `tag_suggestions`. `project.rs`: `renumber`, `apply_clip_order`, `moved_order`, `source_sorted_order`, `remove_clip`, `insert_clip`, `Clip::set` (returns the old value) and `apply_edit`; `store::read` normalizes clip order; `add_recorded_clip` appends. |
| `video-coach-media` | Nothing. |
| `video-coach-app` | Bus: the clip commands, the history, `trash_clip`/`restore_clip`, shredding, eviction on source changes, clearing at open, `Event::Select`. UI: the `text-editing` yield, selection, inspector, tag field with suggestions, overview and filter, context menu, double-click jump, drag reorder, sort button, keys. |
| `video-coach-harness` | End-to-end clip management. |

## Testing

- **Core:**
  - the ported undo tests, plus the redo-stack delete case, the purge and the cap-dropped delete;
  - order: normalization on read, move, sort, restore position, `apply_clip_order` with stale ids;
  - `tag_summaries`;
  - `tag_suggestions`: prefix, text exclusions, the cap, the last fragment.
- **Harness** (test capture sources, temp projects):
  - each field edit → saved, one undo step, redo; an unchanged edit → nothing;
  - delete → the file is in `.trash` → undo → the file is back at the same list position → redo → trashed;
  - two deletes → only the second is restorable, and the first file is shredded;
  - a delete whose save fails (**read-only project folder**; a read-only `project.json` doesn't stop the temp-file rename) → the clip remains and the file hasn't moved;
  - a source removed after a delete → the trashed file is shredded and undo doesn't resurrect the clip;
  - reorder and sort → undo;
  - jump → paused at the clip's position, and a pending skip burst doesn't move it afterwards;
  - open → `.trash` gone and the history empty;
  - the recording guard.
- **Manual** (batched):
  - click, double-click, the context menu, drag;
  - typing letters in every field fires no shortcut;
  - Ctrl+Z inside and outside a field;
  - switching clips mid-edit commits to the right clip;
  - Tab and click suggestions;
  - the filter chip.

## Risks

1. **Slint at this UI complexity** (parent risk 7). The known traps are designed around above: focus, capture handlers, `PopupWindow` focus theft, and binding breakage. Anything new gets noted in the plan's task notes, and the UX simplified rather than the code contorted.

## Deferred (→ BACKLOG)

- Orphaned-recording cleanup (#38): now after Phase 3; not auto-deleting user media is deliberate.
- Clip-edit undo coalescing.
- A menu bar with Edit → Undo (#32).
- The tag-overview Duration sort.
- ↑/↓ keyboard selection of suggestions.
- Multi-select and bulk tag edits.
- Previewing on selection: Phase 7.
