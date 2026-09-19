# Linux Port — Phase 3 Plan (Clips, Tags, Undo)

**Date:** 2026-09-19
**Spec:** `docs/superpowers/specs/2026-09-19-linux-port-phase-3-design.md` (decisions cited as C1–C9)
**Status:** Reviewed. Simplification and correctness passes are applied; the correctness pass probed Slint 1.18 with injected events.

**Goal:** everything on the spec's "Done when" list works on the reference laptop.

**Execution.**
- Each task runs in a fresh subagent that is given this plan, the spec and `CLAUDE.md`.
- The orchestrator runs `verify` and commits per task.
- Every task must build the whole workspace, including the binary, and keep CI green.

**Known facts. Don't re-derive these.**
- **Slint 1.18** (tested with injected events, winit):
  - **Keys:**
    - The root `FocusScope`'s `capture-key-pressed` runs **before** the focused field. If it accepts a key, the field never sees it.
    - When the capture handler rejects **Esc**, Esc bubbles from a `LineEdit`/`TextEdit` to the root's non-capture `key-pressed`. Letters don't bubble.
    - `LineEdit` handles Ctrl+Z and Ctrl+Shift+Z itself. Ctrl+Y is redo only on Windows.
    - A `LineEdit`'s `key-pressed` can take Tab.
  - **Clicks:**
    - A `TouchArea` in a `ListView` row gets `double-clicked`. A double-click also fires `clicked` twice first.
    - A right-click does **not** fire `clicked`.
    - A row of `DropArea > DragArea > TouchArea + ContextMenuArea` works: clicks and double-clicks fire, and a drag drops on the right row without firing `clicked`.
  - **Focus:**
    - `PopupWindow.show()` takes focus from a `LineEdit`.
    - A click doesn't move focus off a field; call `keys.focus()`.
    - **After** `keys.focus()` inside a click handler, `has-focus` is already false there, but the field's `changed has-focus` handler runs **later** and reads whatever text the click handler set.
  - **Bindings:**
    - `text:` on a `LineEdit` **and** `checked:` on a `CheckBox` break once the user interacts. Set them imperatively.
    - A child component's `out property <bool> editing: a.has-focus || b.has-focus` binds correctly into the root.
    - `TextEdit` has `has-focus` and `key-pressed`.
- **Store.**
  - `store::write` writes `.project.json.tmp` and renames it over `project.json`, so a read-only `project.json` doesn't fail a save. A read-only project **folder** (`chmod 555`) does, and `recordings/` stays writable.
  - `write` creates `recordings/` but not `.trash`.
  - `.trash` is inside `recordings/`, so a rename is always on the same filesystem.
- **Bus.**
  - The recording guard's allow-list is at the top of `Bus::command`.
  - `project_changed()` (`bus/project.rs`) saves, emits `Error` on failure, and emits `ProjectChanged`. It returns `()`; Task 2 makes it return `bool`.
  - **Startup** goes through `RestoreLastProject` → `Bus::commit`, not `open_project`. Both end in `commit()`.
  - `seek_abs` is **private** in `transport.rs`; `load`, `reset_skip`, `set_playing` and `seekable` are there too.
  - `remove_source` returns early when the source is referenced.
- **Existing tests and helpers.**
  - `crates/video-coach-core/tests/project_format.rs::sort_index_is_one_past_the_largest_even_after_a_gap` asserts Phase 4's `max + 1` rule, which this phase replaces.
  - The harness `clip()` helper and the `ReadOnly` drop guard (with its running-as-root skip) are private to `crates/video-coach-harness/tests/project_and_sources.rs`.
  - `write_project` writes sources only.
  - `main.rs` sorts clip rows by `sort_index` (≈line 555), and `ClipRow` has no id.

---

## Task 1 — Core: undo, clip order, tags

No GStreamer. Use `port-swift-module`. Read `UndoController.swift`, `TagAggregation.swift` and their tests first.

1. **`undo.rs`**, exactly as spec C1: multi-level deletes; `push`, `take_undo`/`undone`, `take_redo`/`redone`, `evict_deletes`, `clear`; one eviction routine that purges the evicted clip's `EditClip`s from both stacks; `STACK_CAP = 100`.
   - `push` returns `Vec<Clip>` (the deletes the cap dropped); mark it `#[must_use]`.
   - **Tests:**
     - port `UndoControllerTests`, except `test_pushDelete_evicts_*`: a deliberate divergence, commented as such;
     - three deletes undone in order;
     - a cap-dropped delete is returned and its edits purged;
     - `evict_deletes` purges;
     - `redone` with an updated action files the new one;
     - a push clears redo.
2. **Order** (C3) in `project.rs`:
   - private `renumber`;
   - `apply_clip_order(&[Uuid])`;
   - pure `moved_order(from, to)` and `source_sorted_order()`, which is stable by `(source_index, start_source_seconds)`;
   - `remove_clip(id) -> Option<Clip>`;
   - `insert_clip(clip)`: at `min(sort_index, len)`, then renumber; a no-op if the id is present;
   - `apply_edit(id, ClipEdit) -> Option<ClipEdit>`, returning the previous value.
   - **`add_recorded_clip`** appends with `sort_index = len`. Update its doc.
   - **`store::read`** sorts by `sort_index` (stable) and renumbers, with a comment.
   - **Replace** `sort_index_is_one_past_the_largest_even_after_a_gap` with a test that a gapped or unordered file is normalized on read, and that a recorded clip is appended.
3. **Tags** (`tag.rs`):
   - `tag_summaries(&[Clip]) -> Vec<TagSummary>`, alphabetical;
   - `tag_suggestions(summaries: &[TagSummary], text: &str) -> Vec<String>`: a prefix match on the last comma fragment, excluding tags already in `text` (normalized) and exact matches; up to 8, sorted.
4. **Tests** in `crates/video-coach-core/tests/`. Cover every `ClipEdit` variant through `apply_edit` here, not in the harness.

Commit: `feat(core): undo history, clip order, tag summaries and suggestions`.

## Task 2 — Bus: clip commands, trash, history

In `bus/clips.rs` (new), plus variants in `bus/mod.rs`. `main.rs` gets a placeholder arm for the new event.

1. **Commands:**
   - `EditClip { id, edit: ClipEdit }`. For `Tags`, the UI sends the raw text as one element, and the bus normalizes with `normalize_tags(&v.join(","))`.
   - `DeleteClip(Uuid)`, `MoveClip { from, to }`, `SortClipsBySource`, `JumpToClip(Uuid)`, `Undo`, `Redo`.
2. **Event:** `Event::Select(Uuid)`.
3. **`project_changed()` returns `bool`** (whether the save succeeded). Existing callers ignore it.
4. **`history: UndoController`** on `Bus`, plus one helper `record(action)`: `push` it and shred every returned clip's trash file. **Every** push goes through `record`.
5. **Edits:** `apply_edit`. Skip if the value is unchanged; otherwise `project_changed()`, then `record(EditClip)`.
6. **Order:** `MoveClip`/`SortClipsBySource` compute the target. Skip if it's unchanged; otherwise `apply_clip_order`, `project_changed()`, then `record(ReorderClips)`.
7. **Trash** (C4):
   - `trash_path(clip)` = `recordings/.trash/<recording_filename>`. Shredding only uses it.
   - **`trash_clip(id) -> Option<Clip>`:**
     1. `remove_clip`.
     2. `project_changed()`.
     3. **Only if it saved:** `create_dir_all(.trash)`, then rename the file in. `NotFound` is ignored.
   - **`restore_clip(clip)`:**
     1. Rename back from `.trash`, ignoring `NotFound`.
     2. `insert_clip`.
     3. `project_changed()`.
   - **Delete command:** `trash_clip`, then `record(DeleteClip(clip))`.
8. **Undo and redo:**
   - `take_undo` → apply the inverse → `undone(action)`. `take_redo` → apply forward → `redone(action)`.
   - **Redo of a delete** files `DeleteClip(the clip trash_clip returned)`.
   - An `EditClip` whose clip is missing is logged and dropped, not filed. It is unreachable once eviction purges.
   - **Selection:** after undoing an edit or a delete, or redoing an edit, emit `Select(id)` **after** the `ProjectChanged`.
9. **Source changes:** after a **successful** `MoveSource`/`RemoveSource` (not refused, not `from == to`), call `history.evict_deletes()` and shred each file.
10. **Open:** in `Bus::commit()`, which covers startup restore and explicit opens, `remove_dir_all(recordings/.trash)` (ignoring `NotFound`) and `history.clear()`.
11. **`JumpToClip`:**
    1. `reset_skip()`.
    2. `set_playing(false)`.
    3. If `seekable()`, `load(clip.source_index, clip.start_source_seconds, true, Origin::Scrub)`. Alternatively make `seek_abs` `pub(super)`; pick one.
12. **Harness:**
    - Move `clip()` and `ReadOnly` from `tests/project_and_sources.rs` into `crates/video-coach-harness/src/lib.rs`.
    - Add a helper that writes a project with N clips and a small dummy file in `recordings/` per clip.
    - **Tests** (`tests/clips.rs`):
      - one field edit → saved, undo, redo; an unchanged edit → nothing;
      - delete two clips → both files in `.trash` → undo twice → both back at their positions → redo → trashed;
      - a delete with a read-only folder → the error arrives, the file is still in `recordings/`;
      - delete, then a source removal or move → the trashed file is shredded, and undo doesn't resurrect the clip;
      - delete → undo → move a source → redo → undo → the clip has the **remapped** `source_index`;
      - reorder and sort → undo;
      - jump → paused at the clip's position, and a skip burst in progress doesn't move it afterwards;
      - startup restore (`RestoreLastProject`) and open both empty `.trash` and the history;
      - one clip command refused while recording.

Commit: `feat(app): clip editing, delete with trash, reorder and undo on the bus`.

## Task 3 — UI: the Clips list, selection, keys

In `app.slint` and `main.rs`. Keep the existing name-field yield for now; Task 4 generalizes it.

1. **`ClipRow`** gains `id`. `main.rs` drops its `sort_by_key`, since stored order is the order.
2. **Rows** (C9):
   - selected highlight;
   - **`clicked` selects** (never toggles) and calls `keys.focus()`;
   - `double-clicked` → `JumpToClip`;
   - `ContextMenuArea` with "Jump to clip start" and "Delete clip", both acting on **the row's own id**, since a right-click doesn't select;
   - drag-reorder via the source list's DropArea/DragArea pattern → `MoveClip`;
   - "Untitled" for an empty name;
   - a "Sort by position" header button, disabled with fewer than 2 clips.
3. **Selection** lives **only** in the Slint property `selected-clip` (id string, empty for none):
   - `main.rs` reads it in the `ProjectChanged` handler and clears it if the clip is gone;
   - it is set on `Select`;
   - it is cleared on `ProjectOpened`.
4. **Keys** (C6):
   - **Delete** → `DeleteClip(selected)`;
   - **Ctrl+Z** → `Undo`;
   - **Ctrl+Shift+Z** or **Ctrl+Y** → `Redo`;
   - **Esc** cascade: error dialog, stop recording, clear selection.
5. The list is disabled while `recording`.

Commit: `feat(app): clip selection, context menu, reorder and undo keys`.

## Task 4 — UI: inspector, focus, tags, overview, filter

1. **The `text-editing` yield** (C6):
   - one root property ORing `has-focus` of every text field: project name and the inspector fields, via the inspector component's `out property editing`;
   - `handle-key` rejects everything while it is true, after the error-dialog branch;
   - replace the name-only yield and the `name-editing` out-property; `main.rs:467` checks the specific field instead.
2. **Esc in fields:** a root non-capture `key-pressed` handles a bubbled Esc while `text-editing` by calling `keys.focus()`, which commits. It rejects everything else.
3. **Focus-off clicks:** clicks on the player area and empty sidebar space call `keys.focus()`.
4. **Inspector** (C7): a right-hand column of about 280 px, disabled while recording.
   - With a selection:
     - Name `LineEdit`;
     - Tags `LineEdit` and suggestions;
     - "Show webcam in export" `CheckBox`;
     - Notes `TextEdit`.
   - Otherwise: the overview.
5. **Commit-against-id, and no clobbering:**
   - **One property, `editing-clip-id`.** Any inspector field sets it when it **gains** focus.
   - **On focus loss**, a field:
     1. commits `EditClip` against `editing-clip-id`;
     2. clears the id;
     3. re-renders from the current selection.
   - **Enter** in the name and tags fields commits the same way, keeping focus.
   - **Re-rendering** field text and the checkbox's `checked` is **imperative** (on selection change and on `ProjectChanged`), and **skips while `editing-clip-id` is non-empty**. Checking `has-focus` isn't enough: it's already false during the click.
   - **The checkbox** commits on toggle against the selected id.
6. **Suggestions** (C8):
   - An overlay `Rectangle` (higher `z`, not a `PopupWindow`, not inside a `ScrollView`) under the tags field.
   - **Shown** while the field has focus, the list is non-empty, and `suggestions-dismissed` is false. `suggestions-dismissed` is reset on `edited`.
   - **Computed** by `tag_suggestions` on `edited`.
   - **Tab** (in the field's `key-pressed`) takes the top suggestion. **Clicking** a row takes that one.
   - **Taking** replaces the last fragment, appends `", "`, keeps focus and puts the cursor at the end.
   - **Esc** in the tags field with suggestions shown sets `suggestions-dismissed` and accepts. Otherwise Esc bubbles, as in step 2.
7. **Overview:**
   - `tag_summaries` rows, plus the empty states;
   - clicking toggles `tag-filter`, which is a Slint property only.
8. **Filter:**
   - a chip "Filtered: tag ✕";
   - matching rows only;
   - "No clips tagged 'x'";
   - drag disabled;
   - cleared on `ProjectOpened`.
9. **Screenshot pass:**
   - Use a scratch project written as `project.json` with 3 clips and dummy `.mkv` files. No camera.
   - Set `XDG_CONFIG_HOME` to the scratchpad. Find the window by `_NET_WM_PID`.
   - If the screen is locked, **don't inject input**: use a temporary env-var driver that invokes callbacks, and remove it afterwards.
   - Screenshot the inspector, suggestions, overview and filter.
   - Kill only your own PID.
   - Write "### Task 4 notes".

Commit: `feat(app): clip inspector, tag suggestions, overview and filter`.

### Task 4 notes

Screenshot pass, 2026-09-19, reference laptop: a release build with `vblank_mode=0`, a scratch project (one `videotestsrc` WebM source, 3 tagged clips with empty `.mkv` files) and `XDG_CONFIG_HOME` in the scratchpad. No real input was injected. A temporary env-var driver called the same Slint functions and callbacks the input handlers do, and it has been removed along with the scratch data.

- **Verified:**
  - **Commit against the edited clip.** Focus A's name field, type "Renamed A", then do what a click on row B does (`selected-clip = B; keys.focus()`). `project.json` got A's name, B was untouched, and the inspector showed B's fields. When the row click ran, `editing-clip-id` was still A: the focus-loss handler runs later, as expected.
  - **Tags.** Typing "set piece, defence, p" in B's tags field shows the overlay with "pass", over the checkbox. Taking it gives "set piece, defence, pass, ". Leaving the field (the bubbled-Esc path, `keys.focus()`) commits `["set piece", "defence", "pass"]`, and the field re-renders `", "`-joined.
  - **No-op edit.** A tags commit that normalizes to the stored value sends nothing, and the field re-renders itself.
  - **PiP.** The PiP toggle committed `showPip: true` against B, even with the tags field focused.
  - **Overview and filter.** Deselecting shows the overview: "pass · 3 clips · 1:47", alphabetical. The "pass" filter highlights its row, shows the chip and lists only the matching clips. A tag with no clips shows "No clips tagged 'zzz'".
- **Deviations:**
  - A focus-loss commit re-renders the fields only when the edit changes nothing, since the bus then sends nothing back. A real change waits for its `ProjectChanged`, because re-rendering straight away would flash the old value. `changes()` in `main.rs` mirrors the bus's diff to decide which case applies.
  - `main.rs` no longer checks the name field. The window copies `saved-project-name` into the field whenever it changes, unless the field has focus.
  - Clearing the selection calls `keys.focus()`, so a field can't keep focus while the inspector hides it.
  - `take_suggestion` is in core `tag.rs`, with a test.
  - The overview stays clickable while recording, since the filter is UI state only.
- **For the user's checklist:**
  - Real clicks and keys: Tab and Esc in the tags field (the driver couldn't send keys).
  - Enter in the name and tags fields keeps focus.
  - Esc leaves the notes field.
  - Clicks on the player and on empty sidebar or inspector space leave a field.
  - The cursor lands at the end after taking a suggestion.
  - Letters typed in the inspector fields fire no shortcut.

## Task 5 — Closeout

1. Adversarial review of the Phase 3 code diff; apply the fixes and backlog any deferrals.
2. **BACKLOG:**
   - mark #42's PiP checkbox as delivered;
   - check that #38's revisit line reads "after Phase 3".
3. The user's hands-on checklist for Phase 3, in the Task 5 notes:
   - click, double-click, the context menu, drag;
   - typing letters in every field fires no shortcut;
   - Ctrl+Z in and out of fields;
   - switching clips mid-edit;
   - Tab and click suggestions;
   - the filter;
   - several deletes, then undoing all of them.

## Deliberately not in this phase

- Clip preview on selection: Phase 7.
- Transcript: Phase 10.
- Orphan cleanup: #38.
- Coalescing, the Duration sort, ↑/↓ suggestions: #44.
- Multi-select: #45.
