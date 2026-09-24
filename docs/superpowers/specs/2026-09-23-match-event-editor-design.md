# The match event editor: one line per event, typed or pasted

**Date:** 2026-09-23
**Status:** Reviewed (simplify + correctness applied). The user's decisions of 2026-09-23 stand and are not reopened: times are **time into a file**; there is **a list and a paste box**, both shipping together; it is **a sheet over the app** (P1, accepting that the picture is hidden and Go closes it); and **a time must contain a colon** (T5/B1 — a bare `900` is refused, because a silent misreading puts an event minutes out).
**Builds on:** Phase 9 (match events, `interpret`, the Match panel, `EditMatchEvents` undo — `docs/superpowers/specs/2026-09-20-linux-port-phase-9-design.md`), match vision spec R (the reel and its per-goal trims) and spec C (chapters and the scrubber's marks), the lossless whole-match spec (the current format, v11).
**Evidence:** the code as it stands on `claude/intelligent-lamport-m2indd`. Every claim below carries a `file:line`. Nothing here is measured; there is nothing to measure.

Labels, as in the match vision spec: **[cited]** points at a file in this repository or at a decision recorded in another spec. There is no **[measured]** claim in this document.

---

## Goal

The coach asked for *"an additional event view editor? So an easy way to mass enter events in the game. Sometimes I already know the general timestamps."*

Today the only way to put a match event into a project is to be looking at the frame: `z`, `x` and `v` tag at the playhead (`crates/video-coach-app/ui/app.slint:2312-2324`), and the Match panel's three buttons do the same (`app.slint:866-880`). The panel then lists what was tagged, with a seek and a delete per row and the reel buttons on a goal (`app.slint:895-990`) — but **no row is editable**. A goal tagged two seconds late is deleted and re-tagged, and a half whose times the coach already has on paper has to be scrubbed through end to end.

This adds the two entry routes the coach asked for, both working on **time into a file** — the number the scrubber and the readout show, and the number `MatchEventRecord.source_seconds` already stores (`crates/video-coach-core/src/scoreboard.rs:198-222`):

1. **A list of the project's match events**, where the selected row can be retyped.
2. **A paste box** that takes a block of lines like `2 14:05 home goal`, shows what it understood line by line, and adds the lot in one go.

**One grammar, one parser, one editable field.** The row's field and the paste box hold the *same* text in the *same* grammar, parsed by the *same* function. There is no second way to say a thing.

## Scope and product rules

These are the user's decisions (2026-09-23). This spec follows them and does not reopen them.

- **Times are time into a file, not match-clock time.** `"14:05 of the second half"` means `source_index = 1, source_seconds = 845.0`. The match clock is derived from the period start/stops by `interpret` (`scoreboard.rs:317-352`) and is never typed.
- **Both routes ship together.** The list is the correction tool; the paste box is the entry tool. Neither replaces the other, and neither replaces `z` / `x` / `v`, which stay exactly as they are.

Out of scope, and unchanged by this work: the scoreboard's setup sheet, the reel, chapters, the export sheet, player highlights, and anything the match vision spec's P3–P7 will add.

---

## Decisions

### P. Where it lives

**P1. A sheet, not a panel and not an expansion of the Match panel.**

The Match panel is a column in the right-hand stack, under the clip inspector and the tag overview. Its event list is already capped at `min(168px, lines * 28px)` "so a long match scrolls rather than pushing the inspector out of the column" (`app.slint:893-896`), and a goal's row is *two* 28 px lines because the reel buttons did not fit on one. A paste box plus a per-line echo plus an editable row does not go in that column without evicting the inspector from it.

So: **a modal sheet**, built the way the other two are — and **there is now one component for that shape**, because this was the fourth copy of it. `Scrim` is the full-window dim plus the empty `TouchArea` that swallows clicks meant for the window behind; `Sheet` is the card: `card-width`, a `title` in the one size and weight, and a `VerticalLayout` body its caller fills as children. The export sheet, the setup sheet, the editor and the error dialog are all `Scrim { … }` around a `Sheet`, and nothing but `Sheet` says `#000000a0`, `border-radius: 8px` or `padding: 20px` any more.

`MatchEditorSheet` is a `Sheet` at **640 px** (the setup sheet is 520, the export sheet 480, the error dialog 440): the widest thing in it is an echo line's sentence, not a table. Its two lists get heights of their own in the house idiom (`app.slint:896`).

*One sheet is not a plain `inherits Sheet`:* the setup sheet's colour picker hangs over the card as an absolutely-positioned sibling, and `@children` would put it in the body's layout. That one wraps a `Sheet` in a `Rectangle` instead, taking its size from the card, and keeps the popup beside it.

Top to bottom: the **event list** (the correction tool), the **paste box** with its default-video picker, the **echo list** and its summary, one **message line** (**C5**), and **Done**.

**P2. Opened by a button in the Match panel's header row, beside "Setup…".** That row already holds the panel's title and the `Setup…` button (`app.slint:825-839`), and the new button reads **"Edit events…"**. It follows the house rule for every button — `keys.focus()` first, then the callback.

No keyboard shortcut. `e` and `m` are both free (`handle-key` falls through to `reject` at `app.slint:2427`), and so is every `Ctrl+<letter>` other than `o`, `z`, `y` and `0` (`app.slint:2275-2303`), so one can be added later at no cost. See **Q3**.

**P3. The sheet follows the setup sheet's key guard, with one addition.** `handle-key` guards the sheets in order (`app.slint:2240-2270`). The export sheet `accept`s everything, because it has no text fields; the setup sheet `return reject`s so that typing reaches whichever field has focus, taking only Esc (`app.slint:2255-2265`). The editor has text fields, so it takes the setup sheet's branch.

**That same `return reject` is what makes `Ctrl+V` work in the paste box.** The Ctrl branch at `app.slint:2275-2303` is never reached from inside the sheet — the editor's own branch returns first, and a rejected key goes to the focused `LineEdit`, whose widget handles the paste. (There is no clipboard code anywhere in the crate; the widget's own is all there is.)

The addition: **the setup sheet's Esc closes the sheet outright, even from inside a field, and the editor's must not.** The window's own rule everywhere else is that Esc leaves a field first (`app.slint:2437-2445`: "Esc a field didn't take leaves it, which commits it"). The setup sheet gets away with breaking it because its fields are short and re-seeded from the project every time it opens (`crates/video-coach-app/src/main.rs:931-961`). The paste box's text is neither. So the editor's guard reads:

```slint
if (root.editor-open) {
    if (pressed && event.text == Key.Escape) {
        // Esc in a row's field cancels: the seeded line back, then out.
        if (root.match-editor-line-focused) {
            root.reseed-match-line();
            keys.focus();
            return accept;
        }
        if (!root.text-editing) {
            root.close-match-editor();
            return accept;
        }
    }
    return reject;
}
```

with one more branch before the `!root.text-editing` test: **Esc in the row's field cancels rather than commits.** Leaving a field commits it — that is the window's cascade, and the only way out of the notes field by keyboard — so Esc there would mean "commit", which Esc means nowhere else in this app. The guard therefore puts the seeded line back (`reseed-match-line()`, `main.rs`) and *then* drops focus, so the commit that follows edits nothing. Esc in the **paste box** falls through to the cascade unchanged: its text is the session's, and leaving the field neither commits nor discards it. The next Esc, with nothing focused, closes the sheet.

That the row's field has focus is a property of its own (`line-focused`, out of the sheet and onto the window), because **T3** needs it too.

**That `!root.text-editing` only works if the editor's own `editing` is folded into it.** `text-editing` is `name-edit.has-focus || inspector.editing || highlights.editing` (`app.slint:2071`); the sheet exposes `out property <bool> editing` exactly as `Inspector` (`app.slint:437-438`) and `HighlightsPanel` (`app.slint:1030`) do, and it joins that `||`. Without the fold, `text-editing` is false inside the sheet, the guard closes on the first Esc, and the "Esc leaves the field first" path is dead code.

**P4. The editor is gated where the panel's own actions are.** The button is enabled on the panel's existing `can-edit` — `can-play && !recording && !previewing` — and that is the whole gate: `can-play` is false until the project has a source, so "the project has a video to point at" is already in it. (`open_match_editor` still returns early on an empty source list; that is a backstop, not a second rule.) Its commands are *not* added to the bus's recording allow-list; see **C4**.

**P5. Edits apply as they are committed; there is no Save.** The sheet's one button is **Done**. Each committed row edit is one bus command and one undo step; the paste's Add is one command and one undo step. Nothing is staged.

- *Why not Save/Cancel like the setup sheet:* the setup sheet edits a single value (`ScoreboardConfig`) that is deliberately **not** an undo step (`crates/video-coach-app/src/bus/scoreboard.rs:82-86`), so Cancel is its only way back. Match events are the opposite: every mutation of them is already an undo step through one funnel (`bus/scoreboard.rs:103-118`), and the panel's own delete is immediate and undoable today ("Delete this event (undoable)"). Staging a whole list would mean a second copy of it and a merge, to buy a Cancel that `Ctrl+Z` already provides.
- *The cost, stated:* `Ctrl+Z` does **not** work while the sheet is open — the guard rejects it to the focused field, which is already true of the setup sheet. The coach presses Done, then undoes. Q4 asks whether that is enough.

### T. The event list

**T1. One row per match event, in match order, from the list the panel already builds.** `match_panel::match_rows` returns id, kind, absolute time, the formatted time, the derived label, `role_less` and the reel span, from core's `labelled_events` (`crates/video-coach-app/src/match_panel.rs:49-62`).

**`MatchRowText` gains two fields** — `source_index: usize` and `source_seconds: f64` — filled in `match_rows` from the same record. It does **not** gain a second builder: a parallel `editor_rows` would be a second call to `labelled_events` and a second chance to order events differently from the panel, the scrubber's marks and `[` / `]` (`main.rs:1005-1039`). The panel ignores the two new fields.

**T2. The row is read-only; the selected row reveals one field.** This is `HighlightsPanel`'s shape verbatim (`app.slint:1006-1135`): a `for` of read-only rows (`app.slint:1055`), and `if row.id == root.selected` reveals the controls that edit it (`app.slint:1113`). It is already shipped, already lives with the one-way binding hazard, and the editor copies it rather than inventing a table.

| Part of the row | What it is |
|---|---|
| **Where** | `<n> · <m:ss.t>`, monospace: the 1-based video number and the time into that video, the paste grammar's own numbering (**B1**). |
| **Reads as** | The derived label, `labelled_events`' wording: `1H start`, `2H end`, `Home goal`, or `Start/stop (no period)` greyed, the panel's `role_less` styling. **This is the feedback channel for `interpret`** — see **V2**. |
| **Go / Delete** | `→` and `×`, the panel's two glyphs and tooltips. |

**The selected row adds one `LineEdit`**, holding that event as a line in the paste box's grammar:

```
2 14:05.0 home goal
```

seeded by `match_entry::format_line(kind, source_index, source_seconds)`, and read back by **`bus::editor_line`** — the grammar's `edit_from_line` plus the one rule the grammar does not own, the start/stop cap (**V4**). A `✕` marks a line that will not land, the way `SetupField` marks a bad field (`app.slint:1508-1513`), and nothing is sent until it does.

**The mark and the command call the same function**, cap and all, so a line cannot read good and then be refused — which is what happened when the cap lived in the command alone: retyping a goal as `period` in a format whose start/stops are full showed a tick and got a notice.

There is no combo box for the video, none for the kind, and no fourth column. A kind is a word; a video is a number; both are in the line.

**The reel span is not shown here.** It is on the panel's goal rows, beside the buttons that set it, and repeating it in the sheet would be a second rendering of a number the sheet cannot change (**T9**).

**T3. The rows follow the project, except while the row's field has focus.** The hazard is exact and narrow: a `LineEdit` inside a `for` can only be bound one way — the warning Slint's own code carries here (`app.slint:1488-1489`: "a `text:` binding breaks the moment the user types into it") — so re-seeding that field while the coach is typing in it would overwrite what he typed. Nothing else in the sheet has that problem: the paste box is a window property the rebuild never touches, and the read-only rows want to be current.

So `show_project` rebuilds the editor's rows on **every** `ProjectChanged` while the sheet is open, and skips only while `match-editor-line-focused` is set. That state lasts exactly as long as the coach's hands are in the field, because every commit drops focus (**T4**) — and so do the sheet's two list-changing buttons, a row's `×` and Add, which take focus off the field first as every other button in this app does (a click does not take it by itself).

*Why not "only after a command of the sheet's own":* because the editor is not the only writer, and the sheet's own writes are not the only thing that reaches the rows.

- The sheet can have **two commands in flight** — commit a row, then click Add — and a one-shot flag is consumed by the first publish, so the second's events never reach the list.
- A command the bus **refuses** publishes nothing at all, so a flag set when it was sent stays set, and the next unrelated publish — a transcript arriving (`bus/transcribe.rs`), a source found missing (`bus/transport.rs`) — rebuilds the rows under a field being typed in. That is the very hazard the rule exists to prevent, reached by the mechanism meant to prevent it.

The invariant is the focus, so the code tests the focus.

**T4. Order is match order, and a commit that changes it drops focus.** A row whose time moves past another jumps to its new place, and the edited row stays *selected*, so the coach sees where it went. Stored order would hide the one thing the list exists to show, which is whether the start/stops come out in a sane sequence.

**Selection is not focus.** Slint's `for` reuses items by index, so an event that moves from index 3 to index 0 leaves the focused `LineEdit` sitting over whatever is at index 3 now — and the next keystroke edits a different event. So **every commit drops focus back to the sheet** (`keys.focus()`), unconditionally rather than only when the order changed: one rule, no ordering comparison, and nothing to get wrong. Enter commits and leaves the field; focus loss commits; Esc puts the seeded line back and leaves the field (**P3**).

**Enter therefore commits twice**, and that is fine rather than guarded: the focus drop commits the same line again, the bus reads it against the record the first command already moved, gets the same event, and `edit_match_events` drops it — no save, no undo step, no publish (`bus/scoreboard.rs`). There is no UI-side "did it move?" pre-check; one existed, and it was a second copy of the bus's own reasoning serving a flag that no longer exists (**T3**).

**T5. How a line's time is typed, and what is refused.**

- **Accepted:** `m:ss`, `mm:ss`, `h:mm:ss`, each optionally with tenths (`14:05.5`). Minutes may exceed 59 (`75:20` is 4520 s in an 80-minute file); seconds, and minutes in the non-leading position, must be under 60. Leading and trailing spaces are ignored.
- **Refused: a bare number.** `14` is 14 seconds to a computer and 14 minutes to a coach, and this field's whole job is to be exactly the number the coach means. One rule, in the row's field and in the paste box alike: **a time has a colon.** (The user decided this on 2026-09-23; a leading bare integer means a video number instead — **B1**.)
- **An unparseable line never reaches the bus.** The field marks itself and the row's stored values stand. The parse that marks it and the parse that builds the command are the same function, which is this module's existing discipline: *"the sheet's 'this field is good' mark and the parse that builds the config call the same one and can't drift apart. Written on both sides they did"* (`match_panel.rs:241-246`).
- **An edit carries the stored seconds unless the time text changed.** The sheet keeps the line it seeded the field with; `match_entry::edit_from_line(seed, typed)` compares the two lines' *time tokens*, and when they are byte-identical the command carries the row's **stored** `source_seconds` rather than what `parse_time` makes of the text. Without this, a coach who selects a row to change `home goal` to `away goal` silently re-rounds a stored `14.06` to `14.0`, because the display floors to tenths. (`edit_match_events` drops a no-op — `bus/scoreboard.rs:112-115` — but only when the record is byte-identical, which 14.06 → 14.0 is not.)

**T6. An edit moves the event; it never replaces it.** Committing a line mutates that record's `kind`, `source_index` and `source_seconds` in place, keeping its `id`. Three reasons, each load-bearing:

- **The reel trims hang off the record** (`scoreboard.rs:210-221`) and are stored *relative to the goal* precisely so they follow it. Re-timing a goal by deleting and re-adding would silently reset its trims to the defaults.
- **The id is the key everything else uses**: the panel's rows, the scrubber's marks, `Go`, and — once the match vision spec's P4 ships — a suggestion's resolution.
- **`interpret`'s tie-break is stored order** (`scoreboard.rs:324-326` sorts by time only, and `labelled_with` sorts stably at `scoreboard.rs:409`), so two events sharing an instant keep the order they were tagged in. Replacing a record would push it to the end of the list and flip that pair.

Core gains one mutator beside the existing three:

```rust
/// Move or retype the event with `id`. Returns false if there is none.
/// Clears the reel trims when the kind stops being a goal: they are
/// meaningless on a start/stop, and `set_reel_trim` refuses one.
pub fn edit_match_event(
    &mut self, id: Uuid, kind: MatchEventKind, source_index: usize, source_seconds: f64,
) -> bool
```

Changing home goal ↔ away goal keeps the trims; goal → start/stop clears both to `None`; start/stop → goal leaves them `None`, which is the reel's default.

**T7. There is no "add" control.** Adding an event is one line typed into the paste box and Add pressed — the same grammar, the same parser, the same echo, the same refusals. A separate add row would be a second entry path with its own defaults, its own validation and its own bugs, for a job the box already does.

**T8. Delete is immediate and undoable**, reusing `Command::DeleteMatchEvent(Uuid)` (`crates/video-coach-app/src/bus/mod.rs:117`) — the panel's behaviour today, with no confirmation, because undo is the confirmation.

**T9. Reel trims stay on the panel.** Setting one means *"the reel starts at the frame I am looking at"*, captured from the scan position at the click (match vision spec R3; `bus/mod.rs:118-125`), and there is no frame to look at behind a modal. The panel keeps its four `ReelButton`s, which is where that job belongs. Because trims are relative, re-timing a goal in the sheet carries them along — the behaviour R3 was designed for.

**T10. Go seeks and closes.** The row's `→` is the panel's own `on_seek_match_event` (`main.rs:764-778`) — an event id in, one frame-accurate `Command::ScrubRelease` out — and then the sheet closes. The picture is behind the modal, so seeking without closing would show the coach nothing.

**It does not clear the paste box.** A coach who pastes a half's notes and checks one row against the picture must get the block back when the sheet reopens — see **F2**.

### B. The paste box

**B1. The grammar: one event per line, `[<video>] <time> <words>`.**

- **Comments and blanks.** `#` starts a comment to the end of the line; a line that is empty after stripping it is ignored silently, contributing no echo row. This is `kickoffs.txt`'s convention (`docs/superpowers/plans/2026-09-22-match-vision.md:216-219`).
- **Fields are separated by whitespace,** in this order:
  - an optional **video number**, 1-based. **A leading bare integer is always a video number** — never a time, never a word: **T5**'s colon rule is what makes that unambiguous. A number outside `1..=source_videos.len()` gets its own refusal rather than being reinterpreted as anything else.
  - a **time**, in exactly the shapes **T5** accepts — a colon required, tenths optional.
  - the **rest of the line**, which names the kind (**B2**).
- **A line with no video number uses the sheet's default**, a `ComboBox` above the box reading "Lines with no number are: `1 · <name>`", defaulting to the first source (`SourceRef.display_name`, `crates/video-coach-core/src/project.rs:123`). Every echo row names the video it resolved to, so a wrong default is visible before Add.
- **The kind is required.** A line with a time and nothing else is refused with *"no event word"*. See **B6** for why that is the right answer for `kickoffs.txt` rather than an annoyance.

`900 home goal` is therefore refused as *"there is no video 900 — a time needs a colon (15:00)"*, which is the whole point of the colon rule: the misreading is named, not performed. **The hint rides every video refusal**, not only a number big enough to look like seconds — a coach who types `14` meaning fourteen minutes into a two-video project is exactly the reader it is for, and "there is no video 14" alone tells him nothing about what to do.

**B2. The vocabulary, matched over the whole remainder.** The remainder is lowercased, `-`, `_` and `,` become spaces, and runs of space collapse. Then the tokens are looked up:

| Kind | Words |
|---|---|
| `HomeGoal` | `home`, `hg`, `z` |
| `AwayGoal` | `away`, `ag`, `x` |
| `StartStop` | `v`, `start`, `stop`, `end`, `period`, `half`, `ht`, `ft`, `fulltime`, `halftime` |
| — (ignored) | `goal`, `goals`, `at`, `the`, `scored` |
| **refused, with its own reason** | `kickoff`, `kick`, `ko`, `restart`, `whistle` |

`z`, `x` and `v` are the app's own three keys (`app.slint:2312-2324`), so the vocabulary starts from what the coach's fingers already know.

**The kick-off words are refused on purpose, and the refusal says why.** They are the words most likely to appear in the coach's own notes, and `kickoffs.txt` is *by definition* a list of **post-goal restarts** (`docs/superpowers/plans/2026-09-22-match-vision.md:216-219`). A restart is **not a stored event kind**, and `interpret` is positional (`scoreboard.rs:317-352`): sorted by absolute time, the first start/stop starts period 0, the second ends it, and so on. So one spurious start/stop does not add a stray row — it **shifts every period boundary after it**, and with it the match clock burned into every export. A file of six restarts pasted into a tagged match would move half-time by six events.

The echo therefore reads:

```
✗  2 · 14:05 · "kick-off" — a restart after a goal isn't a stored event, and
   reading it as a period boundary would move every later period. Only a
   period's own start or end is tagged: use `start`, `end`, `ht` or `ft`.
```

`whistle` joins them because a whistle is as often a foul as a period boundary, and the same damage follows from guessing.

**Team names are also accepted.** A team's name is matched as a **contiguous run of tokens inside the remainder**, against the normalized `ScoreboardConfig` name, so a two-word name works: with `home.name = "Green Rovers"`, the line `2 14:05 green rovers` is a home goal, and `2 14:05 green rovers scored` is too (`scored` is ignored). They are **not** used when one team's normalized name equals or contains the other's, because then the match is ambiguous and a wrong side is a wrong scoreboard.

The verdict for a line is then: **exactly one distinct kind among its tokens → that kind; none → "no event word"; two or more → "ambiguous"**. A refused word (the kick-off list) short-circuits with its own reason. So `home goal` is a home goal (`goal` is ignored), `goal` alone is not understood (which side?), and `home away` is ambiguous. Anything unrecognised that is not in the ignore list makes the line ambiguous rather than being skipped, because a stray word is more likely a misspelled side than noise.

**B3. Every line is echoed, live, in input order.** A list under the box, rebuilt on every keystroke through a `pure callback check-paste(string) -> [PasteLine]` — the same shape as the tags field's live suggestions (`app.slint:515-518`) and the setup sheet's per-field validators, which take the text as an argument precisely so the binding re-evaluates as it is typed (`app.slint:1552-1562`, `main.rs:855-877`). The work is pure string parsing over a few dozen lines.

**Three verdicts, one glyph each:**

| | Example |
|---|---|
| **will add** | `✓  2 · 14:05 · Away goal` |
| **already there** | `•  1 · 3:20 · Home goal — already tagged, skipped` |
| **refused** | `✗  line 4: "2 1405 home" — no time (use m:ss)` |

A refusal's reason is a sentence: *no time (use m:ss)*, *no event word*, *ambiguous*, *there is no video 3*, *the second half is 27:13 long*, *every period of this match format is already tagged*, or the kick-off paragraph above. There is no fourth "not understood" bucket — it was the same glyph, the same handling and the same place in the summary as a refusal, and a coach reading `✗` wants the reason, not the taxonomy.

The summary line above the button reads *"7 events to add · 1 already tagged · 2 lines refused"*. The button reads **"Add 7 events"** and is enabled while at least one line will add.

**B4. A paste merges; it never replaces.** Nothing is deleted by adding. A replacing paste would throw away the ids, and with them the reel trims the coach had set on the goals it overwrote (**T6**). Deleting is the list's job.

**B5. After Add, the box keeps the refused lines and nothing else.**

- The **added** lines go: they are in the project.
- The **duplicates** go too. A line skipped as `already tagged` is not something editing can fix — the event it names is already there — so keeping it would ask the coach to clear a line whose only fault is being right. The notice (**C5**) says how many were skipped; the box is for work left to do.
- The **refusals** stay, with their echo, so the coach fixes them in place and presses Add again.

**The bus's own parse decides what is left**, and hands it back as `Event::MatchPasteLeftover`. The UI must not strip the box itself: it re-reads the block against a snapshot that may be a command behind, and a line the bus read differently would then be in neither the box nor the project — typed text lost with nothing to show for it.

Pasting the same block twice therefore adds nothing the second time and leaves the box empty (**V3**).

**B6. The relationship to `kickoffs.txt`: aligned, deliberately not subsumed.** The ground-truth convention is one line per post-goal restart, `<1-based source> <mm:ss>`, with `#` for comments. This grammar is that line **plus a kind word**, and it keeps the `#`, the 1-based number and the `mm:ss` exactly — so a coach can paste the file in, see every line refused for the same stated reason, and add the kind words to the ones that really are period boundaries.

It does not swallow the file, for the reason **B2** gives: a paste box that guessed `StartStop` for a bare `2 14:05` would corrupt the clock of every match whose notes the coach pasted.

### C. Commands, undo and the bus contract

**C1. Two new commands, in `bus/scoreboard.rs`, both going through the existing funnel.**

```rust
/// Move or retype one event. The times are typed, not captured (C2).
EditMatchEvent { id: Uuid, kind: MatchEventKind, source_index: usize, source_seconds: f64 },
/// A batch of typed events, added as one undo step. Partly refused lines
/// are named in one notice; the rest are added (V4).
AddMatchEvents(Vec<PendingMatchEvent>),
```

`PendingMatchEvent { kind, source_index, source_seconds }` lives in core beside the parser.

**Why not reuse `TagMatchEvent`.** They are the same mutation under different rules, and the rules are the interesting part. `TagMatchEvent` must never refuse a duplicate (two goals in quick succession from the keyboard are real football) and must not be bounds-checked against `duration_seconds` (the playhead is in range by construction). `AddMatchEvents` does both (**V1**, **V3**). Folding them together would mean a flag that selects which rules apply, which is a worse thing than two commands. `TagMatchEvent` stays exactly what its doc says it is: *"The position is the readout's at the keypress, captured by the caller"* (`bus/mod.rs:109-112`).

**C2. Typed times and the caller-captured rule.** CLAUDE.md's bus contract exists to stop **queue delay** moving an event: the bus must never ask the pipeline where it is, because by the time the handler runs the answer has changed. A typed time satisfies that rule trivially and absolutely — **the editor never reads the playhead at all**, so there is no position for a queue delay to stale. The contract is unweakened; it simply has nothing to bite on here. (The one place the editor *does* use a position is `Go`, which is an ordinary seek carrying the absolute time as a field, as `[` / `]` already do.)

**C3. A batch is one undo step, because the funnel snapshots the whole list.** `Bus::edit_match_events` clones `match_events` before and after an arbitrary closure, drops a no-op, then saves, records `UndoAction::EditMatchEvents { before, after }` and publishes — once (`bus/scoreboard.rs:103-118`). Twenty appends inside one closure are therefore **one** save, **one** undo step and **one** `ProjectChanged`. No new `UndoAction` variant, no new purge rule, and no per-event inverse (`crates/video-coach-core/src/undo.rs:70-78`).

That one `ProjectChanged` also rebuilds the panel's rows, the scrubber's marks and `match-list-lines` together, since `show_match` builds all three from one `match_rows` call (`main.rs:1005-1039`, called from `show_project` at `main.rs:2066`). A twenty-event paste costs one rebuild, not twenty.

**C4. Neither command is on the recording allow-list, and the button is disabled during a take.** The allow-list is a `matches!` at the top of `Bus::command` (`bus/mod.rs:762-793`) with a comment that settles this already: *"The coach tags the match while scanning **or** recording (spec S4): the three keys are live throughout. Deleting and the setup sheet wait, as every other edit does."* A bulk rewrite of the event list mid-take is exactly the class of thing that waits.

One gap worth naming: the guard is **silent** — it `eprintln!`s and returns with no `Event::Error` (`bus/mod.rs:792`). That is fine for a control the UI greys out, which is why P4 greys this one out, and it is why reaching the guard is a UI bug rather than a user-facing path.

**C5. Refusals are notices — and the sheet needs its own line for them.** Every match-event refusal already is a notice: `UserError::Scoreboard(String)` is in `is_notice` (`bus/mod.rs:437-454`), for the reason given there — a modal could land over a live commentary take and swallow the transport keys.

**But the status bar's notice line (`app.slint:3461`) renders *behind* the sheet's scrim.** The scrim is a sibling drawn after the window's layout (`app.slint:3512`, `:3540`), so a refusal raised while the sheet is open is invisible. The sheet therefore carries `in property <string> message`, one line above Done, and `main.rs` routes a `UserError::Scoreboard` to it as well as to the status bar while the sheet is open — the status bar keeps its copy so the message is still there after Done. The line clears when the sheet closes and when the next command goes out.

Refusals are aggregated into one message per command (*"3 of 7 added: 2 are past the end of the second half, 2 are already tagged"*), because `Scoreboard` carries a free-form `String` and per-row structure would be a new variant for one screen. Per-line detail is the echo's job, before Add is ever pressed.

### V. Validation and feedback

**V1. The bounds: `0.0 <= source_seconds <= source_videos[i].duration_seconds`, and `i` in range.** `duration_seconds` is *"**the** duration authority"* (`crates/video-coach-core/src/project.rs:111-117`), so the check is exact and needs no probe. Out of range is **refused, naming the length** — not clamped. A clamped goal is a wrong timestamp that looks right, and an event past the end can never be seeked to or seen. An out-of-range source index is refused as `tag_match_event` already refuses one (`bus/scoreboard.rs:41-43`).

**V2. The editor never refuses on match logic. It shows what `interpret` made of the list.** Two period starts in a row, a goal before any kick-off, a half that ends before it begins — `interpret` is positional and has an answer for all of them (`scoreboard.rs:317-352`). There is no "invalid" arrangement for it to reject, and inventing one here would mean a second set of match rules that could disagree with the scoreboard's.

The **"Reads as"** part of each row is the whole feedback mechanism, and it is live: retype the second start/stop of the first half and the row says `1H end`. Beyond it, the editor shows two things the app already computes:

- the panel's **over-cap warning** verbatim (`match_panel::over_cap_warning`, `match_panel.rs:158-172`), for start/stops the format has no period for;
- a **goal that the scoreboard does not count** — one outside `[first start, final whistle]` (`scoreboard.rs:596-615`) — greyed the way `role_less` is. That is the "goal before any kick-off" case, and it is a tagging slip for the coach to see, not something to hide. The reel and the chapter list already take this position (match vision spec R2).

**V3. Duplicates: allowed in the list, skipped by the paste.** Two events at the same instant are legal and keep their stored order (`scoreboard.rs:324-326`, `:409`), so a retyped row never refuses one. The paste box is different: its input is a list the coach may well paste twice, and B5 leaves failed lines in the box for a second Add. So a pasted line is **skipped when an event of the same kind is already on the same source within `SAME_EVENT_SECONDS = 1.0`** — echoed `• already tagged`, never silently.

**The comparison is against `existing + accepted-so-far`, not against the project alone.** A block that contains the same line twice must not add it twice: the second copy is a duplicate of the first copy, which is not in the project yet. Two genuine goals inside one second do not happen; a doubled paste, and a doubled line inside one paste, both do.

**V4. The start/stop cap is counted the same way, across the batch.** `Project::start_stops_at_cap` is *"the one rule"* the panel and the command share (`scoreboard.rs:790-801`, `bus/scoreboard.rs:45-52`). A pasted start/stop is checked against **`existing + accepted-so-far`**, so a batch that would push past the format's last period has its excess lines refused and the rest added, with the existing message (*"every period of this match format is already tagged; change the format to tag more"*). Best-effort, not all-or-nothing: a list of twelve goals and one stray period line should add the twelve.

The same rule applies to a row edit: retyping a goal as a start/stop at the cap is refused; moving a start/stop cannot break the cap, since the count is unchanged. **The row's field shows it before the line is sent**, because the mark and the command are one call (**T2**).

**V5. The two cap numbers deliberately differ, and neither is wrong.**

- `Project::start_stops_at_cap` (`scoreboard.rs:790-801`) counts **stored records** against `format.expected_start_stop_events()`. It is what refuses a tag.
- `match_panel::over_cap` (`match_panel.rs:150-155`) subtracts **one place when the back-anchor is on**, because `interpret` inserts a derived period-1 start before the first stored one and then truncates (`scoreboard.rs:328-334`). It is what warns that a stored event has no role.

So with `auto_back_anchor_p1` set, the last stored start/stop is **under the cap and still role-less**, and both numbers are telling the truth about different things. The doc on `interpret` says why: *"The coach never loses a stored event to the anchor — the cap the UI enforces is on the records, which the anchor is not part of — so turning the anchor back off restores every role"* (`scoreboard.rs:310-316`). **Do not reconcile them.** The editor uses the first to refuse and the second to warn, exactly as the key and the setup sheet do.

**V6. Re-timing the earliest start/stop moves the whole match clock, with the back-anchor on.** `interpret` derives period 1's start as `first_stored_start_stop − period_seconds(0)` (`scoreboard.rs:328-333`), so moving the first stored start/stop by ten seconds moves the derived kick-off, and with it every clock reading in every export. That is correct — it is the anchor's whole design — and it is the one edit in this sheet whose effect is not local. It is also why the "Reads as" column exists: the rows either side change wording as soon as the commit lands.

### N. What it must not break

| Thing | Why it is safe |
|---|---|
| **`interpret`'s period rules** | Untouched. The editor adds no role rule and no new kind; it edits the same three fields `z` / `x` / `v` write. The cap refusal is the existing one (**V4**), and the kick-off words are refused rather than invented into start/stops (**B2**). |
| **The reel and its trims** | An edit moves a record, so relative trims follow it (**T6**). Goal → start/stop clears them, which is what `set_reel_trim`'s `NotAGoal` already implies. Moving a goal next to another changes the reel's own clamps and merges (match vision spec R2) — the reel's rule, evaluated at plan time, not a break. |
| **Chapters** | `labelled_events` and `chapter_events` are both derived from `project.match_events` on every call (`scoreboard.rs:378-411`). Nothing is cached, so nothing needs invalidating. |
| **The scrubber's marks** | Built in `show_match` from the same `match_rows` vector as the list (`main.rs:1005-1039`), on the one `ProjectChanged` the batch publishes (**C3**). |
| **The source-change purge** | The editor records only `EditMatchEvents`, which `purge_for_source_change` already drops from both stacks on a source move or remove (`undo.rs:154-167`). No new undo action means no new purge rule. |
| **`ScoreboardContext` / `AbsoluteMatchEvent` caching** | The editor builds neither. `absolute_match_events` is derived per job (`scoreboard.rs:716-724`) and the UI's context is rebuilt in `show_project` and nowhere else (`main.rs:2068-2073`), which is the standing rule. |
| **`source_is_referenced`** | Unchanged: it already counts match events, so a source an editor-added event points at cannot be removed. |
| **Recording** | Blocked both ways (**P4**, **C4**). `TagMatchEvent` stays on the allow-list; nothing else here joins it. |
| **The sheet's rows under an async write** | The invariant in **T3**, with the two writers named. |

### F. Format

**F1. Nothing new is stored, and the format version does not move.** The editor writes only fields `MatchEventRecord` has had since v8: `id`, `kind`, `source_index`, `source_seconds`, `reel_lead_in`, `reel_tail` (`scoreboard.rs:198-222`). It adds no `Project` field, no `Preferences` field and no `state.json` key. `CURRENT_FORMAT_VERSION` stays **11**.

**F2. The paste box's text lives for the session, not for the project.** It is a window property (as the setup sheet's fields are), so it survives the sheet closing and a row's Go — a coach who pastes a half's notes, checks one row against the picture and comes back must find the block still there. It is not written to disk, and it is cleared when a project is opened or closed. The default-video choice resets to the first source each time the sheet opens.

---

## Crate responsibilities

| Crate | Contents |
|---|---|
| `video-coach-core` | A new module `match_entry.rs`: `parse_time` / `format_time`, `parse_line` / `format_line(kind, source_index, source_seconds)` / `edit_from_line`, the kind vocabulary (with the refused kick-off words), `PendingMatchEvent`, `Batch { events, lines, leftover }` and `parse_batch(&Project, default_source, text)` — including the duration bound, the duplicate rule and the incremental cap count, so the echo and the command reach the same verdict from the same code. In `scoreboard.rs`: `Project::edit_match_event`, `#[must_use]`. `SAME_EVENT_SECONDS`. No new dependency: the audit still lists exactly `serde`, `serde_json`, `thiserror`, `uuid`. |
| `video-coach-media` | Nothing. |
| `video-coach-app` | Bus: `EditMatchEvent` and `AddMatchEvents` in `bus/scoreboard.rs`, both through `edit_match_events`, with the aggregate `UserError::Scoreboard` notice and `Event::MatchPasteLeftover`. `bus::editor_line` reads one row line — the grammar plus the cap — for the command *and* for the field's mark (**T2**). `match_panel.rs`: the two new `MatchRowText` fields, the echo-line wording and the summary line, tested headless as the rest of that module is. UI: the shared `Scrim` / `Sheet` (**P1**), `MatchEditorSheet`, the `editor-open` guard in `handle-key`, the `editing` and `line-focused` folds, and the "Edit events…" button. |
| `video-coach-harness` | Batch add as one undo step, a partly-refused batch, and an edit that moves an event and moves the clock with it. |

**`format_time` is core's, and `format::format_hms_tenths` stays where it is.** The app already has the renderer this sheet wants — `format_hms_tenths` produces `M:SS.t` and `H:MM:SS.t`, floored (`crates/video-coach-app/src/format.rs:21-30`) — but `format.rs` imports `gstreamer::glib` for `finish_at` (`format.rs:3`), and core declares no media dependency, so the module cannot move. Core needs its own because `parse_batch` builds echo text. So there are two, both flooring, with one test in the app crate asserting they agree over a table of values. Two five-line functions that a test pins together is a smaller thing than a crate split, and a smaller thing than sending formatted strings from the app into core's parser.

The parser is pure and lives in core, so every line of the grammar is tested on CI with no GStreamer, and the app is left with rendering.

## Testing

- **Core (`match_entry.rs`):**
  - `parse_time` on every accepted shape (`0:05`, `14:05`, `75:20`, `1:02:03`, `14:05.5`) and every refused one (`1405`, `14`, `14:60`, `-1:00`, `14:5:3`, `""`).
  - `format_time` floors (`14.06` → `0:14.0`), has an hours form, and `parse_time(format_time(x))` is within a tenth of `x`.
  - The vocabulary: each word, mixed case, hyphens and commas, `#` comments, blank lines, the ignore list, a bare `goal` unresolved, `home away` ambiguous.
  - **The kick-off words** — `kickoff`, `kick`, `ko`, `restart`, `whistle` — are refused, and the reason names the period shift. A test that one of them **never** yields `StartStop`.
  - Team names: a one-word name, a **two-word name matched as a contiguous run**, a name with a trailing ignored word, and a name **not** matched when one team's name contains the other's.
  - The video number: present, absent (the default), **out of range giving "there is no video N" rather than a time**, and a leading integer with no time after it.
  - `parse_line` / `format_line` round trip for each kind; `edit_from_line` carrying the stored seconds when the time token is unchanged and the parsed seconds when it is not, **including when the video number changed under an unchanged time** (the stored sub-tenth seconds move to the other source); and **the kick-off words refused through `edit_from_line`**, not only through `parse_line` — the row's field is the same grammar.
  - `parse_batch`: the duration bound, the duplicate rule at exactly `SAME_EVENT_SECONDS`, **a block containing the same line twice adding it once**, the cap counted incrementally across the batch, and `leftover` holding exactly the **refused** lines — not the duplicates (**B5**).
- **Core (`scoreboard.rs`):** `edit_match_event` keeps the id and the stored position; clears both trims on goal → start/stop and keeps them on home ↔ away; returns false for an unknown id; a re-timed event reorders `labelled_events` and can change its role; **re-timing the earliest start/stop under `auto_back_anchor_p1` moves every later clock reading**.
- **App (`match_panel.rs`, headless):** `match_rows` carries the source index and the in-source time; the echo lines and the summary wording for all three verdicts; the over-cap warning reused unchanged; `format_time` and `format_hms_tenths` agree.
- **Harness (`tests/match_events.rs`):**
  - a batch of N publishes **one** `ProjectChanged`, and one `Undo` restores the whole prior list;
  - a batch with some lines refused adds the rest, emits one `UserError::Scoreboard` notice and one `MatchPasteLeftover` holding the refused lines alone, and the saved project matches;
  - `EditMatchEvent` moves an event, the scoreboard's state at a later instant changes accordingly, and one `Undo` restores it;
  - **a row retyped as a start/stop at the cap is refused and the record stands**, while moving a start/stop at the cap is not.

  *Not tested here:* that a batch adding nothing publishes nothing, and that a batch is dropped while recording. Both are properties of machinery these commands only pass through — `edit_match_events`' no-op guard (`bus/scoreboard.rs:112-115`) and the allow-list's deny-by-default `matches!` (`bus/mod.rs:762-793`) — each already pinned by its own tests. Re-testing them per command buys nothing and makes adding a command more expensive than it is.
- **Manual (batched, needs the user's eyes):**
  - paste a real half's notes and check the rows read `1H start` / `1H end` / `2H start` / `2H end` in that order;
  - paste `kickoffs.txt` unedited and read the refusal;
  - re-time a goal and confirm its reel entry moves with it and its span is unchanged;
  - confirm the scrubber's marks land where the pasted times are, by seeking to two of them;
  - confirm `Ctrl+V` works in the paste box, that Esc leaves the box before it closes the sheet (**P3**), and that the box still holds its text after Go and a reopen;
  - confirm the button is greyed during a recording and a preview.

## Risks

1. **A wrong time is silent.** Nothing in the app can tell `14:05` from `14:50`; only the picture can. `Go` is the check, and the manual pass above is where it gets exercised.
2. **Esc losing a half-typed paste.** Handled by P3, which is a deviation from the setup sheet's guard and therefore a thing to get right rather than copy — and it only works with P3's `text-editing` fold.
3. **The vocabulary is a judgement call.** It will meet words it does not know. B2's rule — unknown word ⇒ ambiguous, never silently skipped — is what keeps a misunderstanding visible instead of wrong, and the kick-off words are the one case where the right answer is a refusal with an explanation rather than either.
4. **One editable field per row is a narrower target than a table, but it is still a `LineEdit` inside a `for`.** The mitigation is T3's invariant plus T4's unconditional focus drop, both mechanical. The pattern itself is `HighlightsPanel`'s and already ships.

## Deferred

- **Setting reel trims numerically** (**T9**). They are set from the frame, and the modal covers the frame. Revisit if the coach asks for `−12 / +6` as typed numbers.
- **Showing the reel's *effective* span** rather than the stored one (BACKLOG #74). It needs the plan's clamps and merges, which only the export computes.
- **Copying the list out** in the same grammar, which would make the editor a round trip and give the coach a backup of a match's tags. `format_line` already exists for the row field, so this is a button and a clipboard call; it is deferred only because nothing asked for it. **Revisit first.**
- **Loading the paste from a file** ("Open…"). Paste covers it; a file picker for a text file is a picker to maintain.
- **Multi-select and bulk delete.** Row-at-a-time delete plus undo covers the case; a selection model does not earn its place yet.
- **A whole editor session as one undo step.** Per-edit steps are what the funnel gives for free (**C3**); a session step would need the sheet to hold a snapshot and reconcile it.
- **Nudging a time by a frame from the keyboard.** `,` and `.` do that against the picture, which is the right place for it.
- **A stored "restart" event kind** to subsume `kickoffs.txt` (**B2**, **B6**). It would change `interpret`'s input, which is the last thing this feature should do.

## Open questions for the user

Each has a default, and **the plan proceeds on it unless the user says otherwise.** (The colon rule, the sheet, the two routes and time-into-a-file are decided, not open; they are in Scope, P1, T5 and B1.)

- **Q2. Is the default-video dropdown above the paste box worth it,** or should a line with no number always mean the first video?
  **Default:** the dropdown, defaulting to the first video. It costs one control and it is what makes pasting a second-half list without editing forty lines possible.
- **Q3. Should the editor have a keyboard shortcut?** `e` and `m` are free, as is any `Ctrl+<letter>` outside `o`/`z`/`y`/`0`.
  **Default:** no key — the button in the Match panel only. Add `e` on request.
- **Q4. Is "no Ctrl+Z while the sheet is open" acceptable?** It is what the setup sheet does today, and each edit is undoable the moment the sheet closes.
  **Default:** yes. The alternative is letting undo through the guard, which would rewrite the rows under a focused field — the hazard T3 exists to remove — for a small convenience.
- **Q5. Should a time past the end of a video be refused or clamped?**
  **Default:** refused, naming the video's length (**V1**). A clamped event is a wrong timestamp that looks right.
- **Q6. Should a start/stop's *role* be editable directly** — "make this one the second-half kick-off"?
  **Default:** no. Roles are positional and derived (`scoreboard.rs:317-352`); a stored role would be a second source of truth for the match clock, and it is exactly the mistake the port fixed on the macOS side (BACKLOG #27).
