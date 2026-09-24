# The match event editor: a table of the match, and a paste box for a list of times

**Date:** 2026-09-23
**Status:** Draft, awaiting adversarial review. The user answered on 2026-09-23: **a sheet over the app** (P1, accepting that the picture is hidden and Go closes it), and **a time must have a colon** (T4/Q1 — a bare `900` is refused, because a silent misreading puts an event minutes out).
**Builds on:** Phase 9 (match events, `interpret`, the Match panel, `EditMatchEvents` undo — `docs/superpowers/specs/2026-09-20-linux-port-phase-9-design.md`), match vision spec R (the reel and its per-goal trims) and spec C (chapters and the scrubber's marks), the lossless whole-match spec (the current format, v11).
**Evidence:** the code as it stands on `claude/intelligent-lamport-m2indd`. Every claim below carries a `file:line`. Nothing here is measured; there is nothing to measure.

Labels, as in the match vision spec: **[cited]** points at a file in this repository or at a decision recorded in another spec. There is no **[measured]** claim in this document.

---

## Goal

The coach asked for *"an additional event view editor? So an easy way to mass enter events in the game. Sometimes I already know the general timestamps."*

Today the only way to put a match event into a project is to be looking at the frame: `z`, `x` and `v` tag at the playhead (`crates/video-coach-app/ui/app.slint:2310-2323`), and the Match panel's three buttons do the same (`app.slint:874-889`). The panel then lists what was tagged, with a seek and a delete per row and the reel buttons on a goal (`app.slint:908-990`) — but **no row is editable**. A goal tagged two seconds late is deleted and re-tagged, and a half whose times the coach already has on paper has to be scrubbed through end to end.

This adds the two entry routes the coach asked for, both working on **time into a file** — the number the scrubber and the readout show, and the number `MatchEventRecord.source_seconds` already stores (`crates/video-coach-core/src/scoreboard.rs:205-221`):

1. **A table of the project's match events**, editable in place, with add and delete.
2. **A paste box** that takes a block of lines like `2 14:05 home goal`, shows what it understood line by line, and adds the lot in one go.

## Scope and product rules

These are the user's decisions (2026-09-23). This spec follows them and does not reopen them.

- **Times are time into a file, not match-clock time.** `"14:05 of the second half"` means `source_index = 1, source_seconds = 845.0`. The match clock is derived from the period start/stops by `interpret` (`scoreboard.rs:317-352`) and is never typed.
- **Both routes ship together.** The table is the correction tool; the paste box is the mass-entry tool. Neither replaces the other, and neither replaces `z` / `x` / `v`, which stay exactly as they are.

Out of scope, and unchanged by this work: the scoreboard's setup sheet, the reel, chapters, the export sheet, player highlights, and anything the match vision spec's P3–P7 will add.

---

## Decisions

### P. Where it lives

**P1. A sheet, not a panel and not an expansion of the Match panel.**

The Match panel is a column in the right-hand stack, under the clip inspector and the tag overview (`app.slint:3099-3134`, and the comment at `app.slint:3017-3019` on why a match lives below a clip). Its event list is already capped at `min(168px, lines * 28px)` "so a long match scrolls rather than pushing the inspector out of the column" (`app.slint:903-907`), and a goal's row is *two* 28 px lines because the reel buttons did not fit on one (`app.slint:949-990`). A four-column editable table plus a multi-line paste box plus a per-line echo does not go in that column without evicting the inspector from it.

So: **a modal sheet**, built the way the other two are. There is no generic sheet component — the export sheet (`app.slint:3511-3535`), the setup sheet (`app.slint:3538-3576`) and the error dialog (`app.slint:3578-3616`) are three copies of one shape: a full-window scrim `Rectangle { background: #000000a0; }` with an empty `TouchArea` to swallow clicks, and a fixed-width component that sizes itself by `sheet.preferred-height`. `MatchEditorSheet` is a fourth, at **720 px** (the setup sheet is 520, the export sheet 480), with its two lists given heights of their own in the house idiom (`app.slint:1356-1359`, `app.slint:903-907`).

**P2. Opened by a button in the Match panel's header row, beside "Setup…".** That row already holds the panel's title and the `Setup…` button (`app.slint:835-848`), and the new button reads **"Edit events…"**. It follows the house rule for every button — `keys.focus()` first, then the callback (`app.slint:3115-3130`, and the reasoning at `app.slint:3111-3114`).

No keyboard shortcut. `e` and `m` are both free (`handle-key` falls through to `reject` at `app.slint:2431`), and so is every `Ctrl+<letter>` other than `o`, `z`, `y` and `0` (`app.slint:2299`), so one can be added later at no cost. See **Q3**.

**P3. The sheet follows the setup sheet's key guard, with one addition.** `handle-key` guards the sheets in order (`app.slint:2240-2264`). The export sheet `accept`s everything, because it has no text fields; the setup sheet `return reject`s so that typing reaches whichever field has focus, taking only Esc. The editor has text fields, so it takes the setup sheet's branch — **and that is also what makes `Ctrl+V` work in the paste box**, since the Ctrl branch already rejects combinations it does not know (`app.slint:2299`) and there is no clipboard code anywhere in the crate to replace it.

The addition: **the setup sheet's Esc closes the sheet outright, even from inside a field, and the editor's must not.** The window's own rule everywhere else is that Esc leaves a field first (`app.slint:2437-2443`: "Esc a field didn't take leaves it, which commits it"). The setup sheet gets away with breaking it because its fields are short and re-seeded from the project every time it opens (`crates/video-coach-app/src/main.rs:931-961`). The paste box's text is neither. So the editor's guard reads: if a field of the editor has focus, the first Esc returns focus to the sheet (committing the field); the next closes the sheet. The sheet exposes `out property <bool> editing` for that, exactly as `Inspector` (`app.slint:437-438`) and `HighlightsPanel` (`app.slint:1030`) do for `text-editing` (`app.slint:2071`).

**P4. The editor is gated where the panel's own actions are.** The button is enabled on `has-project && can-play && !recording && !previewing` — the panel's existing `can-edit` (`app.slint:3106-3110`) — **and additionally on the project having at least one source**, since an event has to point at a video. Its commands are *not* added to the bus's recording allow-list; see **C4**.

**P5. Edits apply as they are committed; there is no Save.** The sheet's one button is **Done**. Each committed row edit is one bus command and one undo step; the paste's Add is one command and one undo step. Nothing is staged.

- *Why not Save/Cancel like the setup sheet:* the setup sheet edits a single value (`ScoreboardConfig`) that is deliberately **not** an undo step (`crates/video-coach-app/src/bus/scoreboard.rs:83-86`, `app.slint:1536-1537`), so Cancel is its only way back. Match events are the opposite: every mutation of them is already an undo step through one funnel (`bus/scoreboard.rs:105-118`), and the panel's own delete is immediate and undoable today (`app.slint:940-947`, tooltip "Delete this event (undoable)"). Staging a whole table would mean a second copy of the list and a merge, to buy a Cancel that `Ctrl+Z` already provides.
- *The cost, stated:* `Ctrl+Z` does **not** work while the sheet is open — the guard rejects it to the focused field (`app.slint:2263`), which is already true of the setup sheet. The coach presses Done, then undoes. Q4 asks whether that is enough.
- *The benefit, which is not incidental:* while the sheet is open, **the editor is the only writer of `match_events`**. Recording is blocked (P4), the tag keys are swallowed by the guard, and no other command can reach the list. So the rows model can be rebuilt after each committed edit without ever yanking a field the coach is typing in — see **T3**.

### T. The table

**T1. One row per match event, in match order, built from the same list the panel uses.** `match_panel::match_rows` already returns id, kind, absolute time, the formatted time, the derived label, `role_less` and the reel span, from core's `labelled_events` (`crates/video-coach-app/src/match_panel.rs:50-65`). The editor adds a second builder in the same module, `editor_rows`, which returns the same records with two more fields the panel has no use for: the **source index** and the **time into that source**. Both builders call `labelled_events`, so the table, the panel's list, the scrubber's marks and `[` / `]` remain one ordering (`match_panel.rs:81-89`, `main.rs:1007-1022`).

**T2. The columns.**

| Column | Editable | What it is |
|---|---|---|
| **Video** | yes, a `ComboBox` | The source, listed 1-based as `1 · <display name>`, matching the paste grammar's numbering (**B1**) and the coach's own notes convention. Disabled, showing the only entry, when the project has one source. |
| **Time** | yes, a `LineEdit` | Time **into that video**, `m:ss`, `h:mm:ss`, with tenths where the stored value has a fraction (**T4**). |
| **Event** | yes, a `ComboBox` | The *kind*, three entries: `<home> goal`, `<away> goal`, `Period start/stop` — the team names from the scoreboard where there is one, "Home"/"Away" otherwise, via `scoreboard::team_name` (`crates/video-coach-core/src/scoreboard.rs:167-174`). |
| **Reads as** | no | The derived label, `labelled_events`' wording: `1H start`, `2H end`, `Home goal`, or `Start/stop (no period)` greyed, the panel's `role_less` styling (`app.slint:919-926`). **This column is the feedback channel for `interpret`** — see **V2**. |
| **Reel** | no | A goal's span, `−30 s / +6 s`, from the existing `match_panel::reel_span` (`match_panel.rs:69-82`). See **T8**. |
| — | — | A **Go** button (`→`) and a **Delete** button (`×`), the panel's two glyphs and tooltips (`app.slint:927-947`). |

**T3. Order is match order, recomputed on each committed edit.** A row whose time moves past another jumps to its new place, and the edited row stays selected and highlighted so the coach sees where it went (`editor-selected-id`, one `in-out property <string>`). The alternative — stored order — would hide the one thing the table exists to show, which is whether the start/stops come out in a sane sequence.

The rebuild is safe only because of **P5**: nothing but the coach's own commit changes the list while the sheet is open. This matters more than it looks. Slint's own warning is on the file: *"The text is two-way bound all the way out to the window's property: a `text:` binding breaks the moment the user types into it"* (`app.slint:1488-1489`, repeated for `ComboBox.current-index` at `app.slint:1420-1422`). A per-row `LineEdit` inside a `for` over a model can only be bound one-way, so a model rebuilt underneath a focused field would overwrite what is being typed. Making the editor the only writer is what removes that hazard, rather than a rule about when it is safe to rebuild.

**T4. How a time is typed, and what is refused.**

- **Accepted:** `m:ss`, `mm:ss`, `h:mm:ss`, each optionally with tenths (`14:05.5`). Minutes may exceed 59 (`75:20` is 4520 s in an 80-minute file); seconds and minutes in the non-leading positions must be under 60. Leading and trailing spaces are ignored.
- **Refused: a bare number.** `14` is 14 seconds to a computer and 14 minutes to a coach, and this field's whole job is to be exactly the number the coach means. One rule, in the table and in the paste box alike: **a time has a colon.** See **Q1**.
- **An unparseable field never reaches the bus.** The field marks itself — the `✕` the setup sheet's `SetupField` already shows (`app.slint:1490-1529`) — and the row's stored time stands. Esc restores the field's text to the stored value. The parse that marks it and the parse that builds the command are the same function, which is this module's existing discipline: *"the sheet's 'this field is good' mark and the parse that builds the config call the same one and can't drift apart. Written on both sides they did"* (`match_panel.rs:243-247`).
- **Committing:** Enter commits and keeps focus; focus loss commits; Esc reverts. That is the Inspector's three-callback protocol verbatim (`app.slint:448-453`, `app.slint:473-480`, `app.slint:496-502`).
- **A field whose text is unchanged commits nothing.** This is not a micro-optimisation: the display shows tenths, and a stored 14.06 s reads back as `0:14.0`. If an untouched field committed, clicking into a row and pressing Enter would move the event by up to a tenth of a second. (`edit_match_events` also drops a no-op edit — `bus/scoreboard.rs:112` — but only when the value is byte-identical, which 14.06 → 14.0 is not.) The core formatter is therefore its own function, `match_entry::format_time`, **not** `format::format_hms`, which floors to whole seconds (`crates/video-coach-app/src/format.rs:7-19`) and would lose a tenth on every round trip.

**T5. An edit moves the event; it never replaces it.** Committing a row mutates that record's `kind`, `source_index` and `source_seconds` in place, keeping its `id`. Three reasons, each load-bearing:

- **The reel trims hang off the record** (`scoreboard.rs:212-221`) and are stored *relative to the goal* precisely so they follow it (`scoreboard.rs:727-735`). Re-timing a goal by deleting and re-adding would silently reset its trims to the defaults.
- **The id is the key everything else uses**: the panel's rows, the scrubber's marks, `Go`, and — once the match vision spec's P4 ships — a suggestion's resolution.
- **`interpret`'s tie-break is stored order** (`scoreboard.rs:326` sorts by time only, and `labelled_with` sorts stably at `scoreboard.rs:409`), so two events sharing an instant keep the order they were tagged in. Replacing a record would push it to the end of the list and flip that pair.

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

**T6. Add is a row at the foot of the table**, with the same three controls and an **Add** button, enabled once the time parses. Its video defaults to the last row's, or to the first source; its time starts empty; its kind defaults to Period start/stop. It does **not** default to the playhead — `z` / `x` / `v` and the panel's buttons are the playhead route, and a typed-time route that silently prefilled a position from a picture the modal is covering would be a trap.

It sends `AddMatchEvents` with one element, not `TagMatchEvent`; see **C1**.

**T7. Delete is immediate and undoable**, reusing `Command::DeleteMatchEvent(Uuid)` (`crates/video-coach-app/src/bus/mod.rs:117`) — the panel's behaviour today, with no confirmation, because undo is the confirmation.

**T8. Reel trims are shown, not edited.** They are read-only text in the table. Setting one means *"the reel starts at the frame I am looking at"*, captured from the scan position at the click (match vision spec R3; `bus/mod.rs:118-121`), and there is no frame to look at behind a modal. The panel keeps the four `ReelButton`s (`app.slint:963-986`), which is where that job belongs.

Two notes the table inherits rather than creates: the span shown is the **stored** trim, not what the reel will actually play after its clamps and merges (BACKLOG #74); and because trims are relative, re-timing a goal in the table carries them along, which is the behaviour R3 was designed for.

**T9. Go seeks and closes.** The row's `→` sends `Command::ScrubRelease { abs }` at the event's absolute time and closes the sheet, reusing the path `[` / `]` already take (`main.rs:812-835`), including its optimistic `ui.target_abs`. The picture is behind the modal, so seeking without closing would show the coach nothing.

### B. The paste box

**B1. The grammar: one event per line, `[<video>] <time> <words>`.**

- **Comments and blanks.** `#` starts a comment to the end of the line; a line that is empty after stripping it is ignored silently, contributing no echo row. This is `kickoffs.txt`'s convention (`docs/superpowers/plans/2026-09-22-match-vision.md:218`).
- **Fields are separated by whitespace,** in this order:
  - an optional **video number**, 1-based, when it is the first token and is a bare integer in `1..=source_videos.len()`. `2` is the second half.
  - a **time**, in exactly the shapes **T4** accepts — a colon required, tenths optional.
  - the **rest of the line**, which names the kind (**B2**).
- **A line with no video number uses the sheet's default**, a `ComboBox` above the box reading "Lines with no number are: `1 · <name>`", defaulting to the first source. Every echo row names the video it resolved to, so a wrong default is visible before Add.
- **The kind is required.** A line with a time and nothing else is echoed as *"no event word"* and is not added. See **B6** for why this is the right answer for `kickoffs.txt` rather than an annoyance.

**B2. The vocabulary, matched on the whole remainder.** The remainder is lowercased, `-`, `_` and `,` become spaces, runs of space collapse. Then each token is looked up:

| Kind | Words |
|---|---|
| `HomeGoal` | `home`, `hg`, `z` |
| `AwayGoal` | `away`, `ag`, `x` |
| `StartStop` | `v`, `start`, `stop`, `end`, `kickoff`, `kick`, `ko`, `whistle`, `period`, `half`, `ht`, `ft`, `fulltime`, `halftime` |
| — (ignored) | `goal`, `goals`, `at`, `the`, `scored` |

`z`, `x` and `v` are the app's own three keys (`app.slint:2310-2323`), so the vocabulary starts from what the coach's fingers already know.

**Team names are also accepted**, as a whole normalized name — `rovers 14:05` — taken from `ScoreboardConfig` when there is one. They are **not** used when one team's normalized name equals or contains the other's, because then the match is ambiguous and a wrong side is a wrong scoreboard.

The verdict for a line is then: **exactly one distinct kind among its tokens → that kind; none → "no event word"; two or more → "ambiguous"**. So `home goal` is a home goal (`goal` is ignored), `goal` alone is *not* understood (which side?), and `home away` is ambiguous. Anything unrecognised that is not in the ignore list makes the line ambiguous rather than being skipped, because a stray word is more likely a misspelled side than noise.

**B3. Every line is echoed, live, in input order.** A second list under the box, rebuilt on every keystroke through a `pure callback check-paste(string) -> [PasteLine]` — the same shape as the tags field's live suggestions (`app.slint:515-518`) and the setup sheet's per-field validators, which take the text as an argument precisely so the binding re-evaluates as it is typed (`app.slint:1560-1567`, `main.rs:855-877`). The work is pure string parsing over a few dozen lines.

Four verdicts, one glyph each:

| | Example |
|---|---|
| **will add** | `✓  2 · 14:05 · Away goal` |
| **already there** | `•  1 · 3:20 · Home goal — already tagged, skipped` |
| **refused** | `✗  2 · 40:00 — the second half is 27:13 long` |
| **not understood** | `✗  line 4: "2 1405 home" — no time (use m:ss)` |

and a summary line above the button: *"7 events to add · 1 already tagged · 2 lines not understood"*. The button reads **"Add 7 events"** and is enabled while at least one line will add.

**B4. A paste merges; it never replaces.** Nothing is deleted by adding. A replacing paste would throw away the ids, and with them the reel trims the coach had set on the goals it overwrote (**T5**). Deleting is the table's job.

**B5. After Add, the box keeps only the lines that were not added.** The understood lines go; the duplicates, the refusals and the unclear lines stay, with their echo, so the coach fixes them in place and presses Add again. That makes the box self-clearing and the whole operation idempotent — pasting the same block twice adds nothing the second time (**V3**).

**B6. The relationship to `kickoffs.txt`: aligned, deliberately not subsumed.** The ground-truth convention is one line per post-goal restart, `<1-based source> <mm:ss>`, with `#` for comments (`docs/superpowers/plans/2026-09-22-match-vision.md:216-219`). This grammar is that line **plus a kind word**, and it keeps the `#`, the 1-based number and the `mm:ss` exactly.

It does not swallow the file, and that is on purpose: a post-goal restart **is not a stored event kind**. The match vision spec is explicit — *"No event kind means 'kick-off', and `V` would shift every period boundary after it"* (`docs/superpowers/specs/2026-09-22-match-vision-design.md:556`), because `interpret` is positional and the third start/stop in a half *is* the second period's start (`scoreboard.rs:317-352`). A paste box that guessed `StartStop` for a bare `2 14:05` would corrupt the clock of every match whose notes the coach pasted. So a bare time is echoed *"no event word"*, and `kickoffs.txt` keeps its own job.

### C. Commands, undo and the bus contract

**C1. Two new commands, in `bus/scoreboard.rs`, both going through the existing funnel.**

```rust
/// Move or retype one event. The times are typed, not captured (C2).
EditMatchEvent { id: Uuid, kind: MatchEventKind, source_index: usize, source_seconds: f64 },
/// A batch of typed events, added as one undo step. Partly refused lines
/// are named in one notice; the rest are added (V4).
AddMatchEvents(Vec<PendingMatchEvent>),
```

`PendingMatchEvent { kind, source_index, source_seconds }` lives in core beside the parser. The table's Add row sends `AddMatchEvents` with one element.

**Why not reuse `TagMatchEvent` for the single add.** They are the same mutation under different rules, and the rules are the interesting part. `TagMatchEvent` must never refuse a duplicate (two goals in quick succession from the keyboard are real football) and must not be bounds-checked against `duration_seconds` (the playhead is in range by construction). `AddMatchEvents` does both (**V1**, **V3**). Folding them together would mean a flag that selects which rules apply, which is a worse thing than two commands. `TagMatchEvent` stays exactly what its doc says it is: *"The position is the readout's at the keypress, captured by the caller"* (`bus/mod.rs:109-111`).

**C2. Typed times and the caller-captured rule.** CLAUDE.md's bus contract exists to stop **queue delay** moving an event: the bus must never ask the pipeline where it is, because by the time the handler runs the answer has changed. A typed time satisfies that rule trivially and absolutely — **the editor never reads the playhead at all**, so there is no position for a queue delay to stale. The contract is unweakened; it simply has nothing to bite on here. (The one place the editor *does* read a position is `Go`, which is an ordinary seek and carries the absolute time as a field, as `[` / `]` already do — `main.rs:826-833`.)

**C3. A batch is one undo step, because the funnel snapshots the whole list.** `Bus::edit_match_events` clones `match_events` before and after an arbitrary closure, drops a no-op, then saves, records `UndoAction::EditMatchEvents { before, after }` and publishes — once (`bus/scoreboard.rs:105-118`). Twenty appends inside one closure are therefore **one** save, **one** undo step and **one** `ProjectChanged`. No new `UndoAction` variant, no new purge rule, and no per-event inverse (`crates/video-coach-core/src/undo.rs:70-78`).

That one `ProjectChanged` also rebuilds the panel's rows, the scrubber's marks and `match-list-lines` together, since `show_match` builds all three from one `match_rows` call (`main.rs:1005-1039`, called from `show_project` at `main.rs:2066`). A twenty-event paste costs one rebuild, not twenty.

**C4. Neither command is on the recording allow-list, and the button is disabled during a take.** The allow-list is a `matches!` at the top of `Bus::command` (`bus/mod.rs:762-794`) with a comment that settles this already: *"The coach tags the match while scanning or recording (spec S4): the three keys are live throughout. **Deleting and the setup sheet wait**, as every other edit does."* A bulk rewrite of the event list mid-take is exactly the class of thing that waits.

One gap worth naming: the guard is **silent** — it `eprintln!`s and returns with no `Event::Error` (`bus/mod.rs:793`). That is fine for a control the UI greys out, which is why P4 greys this one out, and it is why reaching the guard is a UI bug rather than a user-facing path.

**C5. Refusals are notices, not dialogs.** Every match-event refusal already is: `UserError::Scoreboard(String)` is in `is_notice` (`bus/mod.rs:428-454`), for the reason given there — a modal could land over a live commentary take and swallow the transport keys. The editor's refusals join it, aggregated into one message per command (`"3 of 7 added: 2 are past the end of the second half, 2 are already tagged"`), because `Scoreboard` carries a free-form `String` and per-row structure would be a new variant for one screen. The table's own per-field errors never get that far: they are marked in the field and never sent (**T4**).

### V. Validation and feedback

**V1. The bounds: `0.0 <= source_seconds <= source_videos[i].duration_seconds`, and `i` in range.** `duration_seconds` is *"the duration authority"* (`crates/video-coach-core/src/project.rs:113-117`), so the check is exact and needs no probe. Out of range is **refused, naming the length** — not clamped. A clamped goal is a wrong timestamp that looks right, and an event past the end can never be seeked to or seen. An out-of-range source index is refused as `tag_match_event` already refuses one (`bus/scoreboard.rs:41-44`).

**V2. The editor never refuses on match logic. It shows what `interpret` made of the list.** Two period starts in a row, a goal before any kick-off, a half that ends before it begins — `interpret` is positional and has an answer for all of them (`scoreboard.rs:317-352`): sorted by absolute time, the first start/stop starts period 0, the second ends it, and so on. There is no "invalid" arrangement for it to reject, and inventing one here would mean a second set of match rules that could disagree with the scoreboard's.

The **"Reads as"** column is the whole feedback mechanism, and it is live: type `14:05` into the second start/stop of the first half and the column says `1H end`. Beyond it, the editor shows two things the app already computes:

- the panel's **over-cap warning** verbatim (`match_panel.rs:162-173`), for start/stops the format has no period for;
- a **goal that the scoreboard does not count** — one outside `[first start, final whistle]` (`scoreboard.rs:596-615`) — marked in the "Reads as" column the way `role_less` is marked, in `Palette.alternate-foreground` (`app.slint:919-926`). That is the "goal before any kick-off" case, and it is a tagging slip for the coach to see, not something to hide. The reel and the chapter list already take this position (match vision spec R2).

**V3. Duplicates: allowed in the table, skipped by the paste.** Two events at the same instant are legal and keep their stored order (`scoreboard.rs:326`, `:409`), so the table never refuses one. The paste box is different: its input is a list the coach may well paste twice, and B5 leaves failed lines in the box for a second Add. So a pasted line is **skipped when an event of the same kind is already on the same source within `SAME_EVENT_SECONDS = 1.0`** — echoed `• already tagged`, never silently. Two genuine goals inside one second do not happen; a doubled paste does.

**V4. The start/stop cap is counted across the batch.** `Project::start_stops_at_cap` is *"the one rule"* the panel and the command share (`scoreboard.rs:796-800`, `bus/scoreboard.rs:47-53`). A paste is checked line by line against `existing + accepted-so-far`, so a batch that would push past the format's last period has its **excess lines refused and the rest added**, with the existing message (*"every period of this match format is already tagged; change the format to tag more"*) in the aggregate notice. Best-effort, not all-or-nothing: a list of twelve goals and one stray `V` should add the twelve.

The same rule applies to the table: retyping a goal as a start/stop at the cap is refused; moving a start/stop cannot break the cap, since the count is unchanged.

### N. What it must not break

| Thing | Why it is safe |
|---|---|
| **`interpret`'s period rules** | Untouched. The editor adds no role rule and no new kind; it edits the same three fields `z` / `x` / `v` write. The cap refusal is the existing one (**V4**). |
| **The reel and its trims** | An edit moves a record, so relative trims follow it (**T5**). Goal → start/stop clears them, which is what `set_reel_trim`'s `NotAGoal` already implies (`scoreboard.rs:743-746`). Moving a goal next to another changes the reel's own clamps and merges (match vision spec R2) — the reel's rule, evaluated at plan time, not a break. |
| **Chapters** | `labelled_events` and `chapter_events` are both derived from `project.match_events` on every call (`scoreboard.rs:378-411`). Nothing is cached, so nothing needs invalidating. |
| **The scrubber's marks** | Built in `show_match` from the same `match_rows` vector as the list (`main.rs:1007-1022`), on the one `ProjectChanged` the batch publishes (**C3**). |
| **The source-change purge** | The editor records only `EditMatchEvents`, which `purge_for_source_change` already drops from both stacks on a source move or remove (`undo.rs:154-167`, called from `bus/sources.rs:65` and `:82`). No new undo action means no new purge rule. |
| **`ScoreboardContext` / `AbsoluteMatchEvent` caching** | The editor builds neither. Its rows carry `(source_index, source_seconds)` and are rebuilt from the `ProjectChanged` snapshot, per the standing rule at `scoreboard.rs:249-253`. |
| **`source_is_referenced`** | Unchanged: it already counts match events, so a source an editor-added event points at cannot be removed (`project.rs:314-330`). |
| **Recording** | Blocked both ways (**P4**, **C4**). `TagMatchEvent` stays on the allow-list; nothing else here joins it. |

### F. Format

**F1. Nothing new is stored, and the format version does not move.** The editor writes only fields `MatchEventRecord` has had since v8: `id`, `kind`, `source_index`, `source_seconds`, `reel_lead_in`, `reel_tail` (`scoreboard.rs:205-221`). It adds no `Project` field, no `Preferences` field (`project.rs:76-99`) and no `state.json` key. `CURRENT_FORMAT_VERSION` stays **11** (`crates/video-coach-core/src/store.rs:21`).

**F2. The paste box's text is not persisted.** It lives for the life of the sheet and is gone when the sheet closes. Nor is the default-video choice remembered: it resets to the first source each time the sheet opens, as the setup sheet re-seeds all its fields from the project (`main.rs:931-961`).

---

## Crate responsibilities

| Crate | Contents |
|---|---|
| `video-coach-core` | A new module `match_entry.rs`: `parse_time` / `format_time`, the kind vocabulary, `PendingMatchEvent`, `Batch { events, lines, leftover }` and `parse_batch(&Project, default_source, text)` — including the duration bound, the duplicate rule and the incremental cap count, so the echo and the command reach the same verdict from the same code. In `scoreboard.rs`: `Project::edit_match_event`. `SAME_EVENT_SECONDS`. No new dependency: the audit still lists exactly `serde`, `serde_json`, `thiserror`, `uuid`. |
| `video-coach-media` | Nothing. |
| `video-coach-app` | Bus: `EditMatchEvent` and `AddMatchEvents` in `bus/scoreboard.rs`, both through `edit_match_events`, with the aggregate `UserError::Scoreboard` notice. `match_panel.rs`: `editor_rows`, the echo-line wording and the summary line, tested headless as the rest of that module is. UI: `MatchEditorSheet`, the `match-editor-open` guard in `handle-key`, the "Edit events…" button, and `show_match_editor` in `main.rs`. |
| `video-coach-harness` | Batch add as one undo step, a partly-refused batch, an edit that moves an event, and a batch refused while recording. |

The parser is pure and lives in core, so every line of the grammar is tested on CI with no GStreamer, and the app is left with rendering.

## Testing

- **Core (`match_entry.rs`):**
  - `parse_time` on every accepted shape (`0:05`, `14:05`, `75:20`, `1:02:03`, `14:05.5`) and every refused one (`1405`, `14`, `14:60`, `-1:00`, `14:5:3`, `""`).
  - `format_time(parse_time(s)) == s` for the shapes the editor displays, and `format_time` keeping a tenth that `format_hms` would floor away.
  - The vocabulary: each word, mixed case, hyphens and commas, `#` comments, blank lines, the ignore list, a bare `goal` unresolved, `home away` ambiguous, team names matched, and team names **not** matched when one contains the other.
  - The video number: present, absent (the default), out of range, and a leading integer that is not a video number.
  - `parse_batch`: the duration bound, the duplicate rule at exactly `SAME_EVENT_SECONDS`, the cap counted incrementally across the batch, and `leftover` holding exactly the lines that were not added.
- **Core (`scoreboard.rs`):** `edit_match_event` keeps the id and the stored position; clears both trims on goal → start/stop and keeps them on home ↔ away; returns false for an unknown id; a re-timed event reorders `labelled_events` and can change its role; a moved goal's trims still describe the same span around it.
- **App (`match_panel.rs`, headless):** `editor_rows` carries the source index and the in-source time; the echo lines and the summary line's wording for all four verdicts; the over-cap warning reused unchanged.
- **Harness (`tests/match_events.rs`, or a sibling):** following the five shapes already there —
  - a batch of N publishes **one** `ProjectChanged` and one `Undo` restores the whole prior list;
  - a batch with some lines refused adds the rest, emits one `UserError::Scoreboard` notice, and the saved project matches;
  - a batch that adds nothing produces **no** `ProjectChanged` (asserted through the `shutdown()` barrier, as `no_project_changed` does today);
  - `EditMatchEvent` moves an event, the scoreboard's state at a later instant changes accordingly, and one `Undo` restores it;
  - a batch sent while recording is dropped and the project is unchanged.
- **Manual (batched, needs the user's eyes):**
  - paste a real half's notes and check the "Reads as" column gives `1H start` / `1H end` / `2H start` / `2H end` in that order;
  - re-time a goal in the table and confirm its reel entry moves with it and its span is unchanged;
  - confirm the scrubber's marks land where the pasted times are, by seeking to two of them;
  - confirm `Ctrl+V` works in the paste box, and that Esc leaves the box before it closes the sheet (**P3**);
  - confirm the button is greyed during a recording and a preview.

## Risks

1. **Per-row editable controls inside a Slint `ListView`.** The one-way binding warning (`app.slint:1488-1489`) is a real trap, and the mitigation is structural rather than careful: the modal makes the editor the only writer (**P5**, **T3**). If that turns out not to hold in practice, the fallback is the Inspector's shape — a read-only list with a one-row edit strip beneath it — which is the same information in the same sheet.
2. **A wrong time is silent.** Nothing in the app can tell `14:05` from `14:50`; only the picture can. `Go` is the check, and the manual pass above is where it gets exercised.
3. **Esc losing a half-typed paste.** Handled by P3, which is a deviation from the setup sheet's guard and therefore a thing to get right rather than copy.
4. **The vocabulary is a judgement call.** It will meet words it does not know. B2's rule — unknown word ⇒ ambiguous, never silently skipped — is what keeps a misunderstanding visible instead of wrong.

## Deferred

- **Setting reel trims numerically in the table** (**T8**). They are set from the frame, and the modal covers the frame. Revisit if the coach asks for `−12 / +6` as typed numbers.
- **Showing the reel's *effective* span** rather than the stored one (BACKLOG #74). It needs the plan's clamps and merges, which only the export computes.
- **Copying the table out** in the same grammar, which would make the editor a round trip and give the coach a backup of a match's tags. Cheap, and genuinely useful; it is deferred only because nothing asked for it. Revisit first.
- **Loading the paste from a file** ("Open…"). Paste covers it; a file picker for a text file is a picker to maintain.
- **Multi-select and bulk delete.** Row-at-a-time delete plus undo covers the case; a selection model does not earn its place yet.
- **A whole editor session as one undo step.** Per-edit steps are what the funnel gives for free (**C3**); a session step would need the sheet to hold a snapshot and reconcile it.
- **Nudging a time by a frame from the keyboard** in the table. `,` and `.` do that against the picture, which is the right place for it.
- **A stored "restart" event kind** to subsume `kickoffs.txt` (**B6**). It would change `interpret`'s input, which is the last thing this feature should do.

## Open questions for the user

Each has a default, and **the plan proceeds on it unless the user says otherwise.**

- **Q1. Should a bare number be accepted as a time?** `900` for 15:00 would make pasting from a tool that emits seconds work.
  **Default:** no — a time has a colon, in the table and the paste box alike (**T4**), because `14` is ambiguous between 14 seconds and 14 minutes and this field's one job is to be exact.
- **Q2. Is the default-video dropdown above the paste box worth it,** or should a line with no number always mean the first video?
  **Default:** the dropdown, defaulting to the first video. It costs one control and it is what makes pasting a second-half list without editing forty lines possible.
- **Q3. Should the editor have a keyboard shortcut?** `e` and `m` are free, as is any `Ctrl+<letter>` outside `o`/`z`/`y`/`0`.
  **Default:** no key — the button in the Match panel only. Add `e` on request.
- **Q4. Is "no Ctrl+Z while the sheet is open" acceptable?** It is what the setup sheet does today, and each edit is undoable the moment the sheet closes.
  **Default:** yes. The alternative is letting the editor's own undo through the guard, which means the rows model can change under a focused field (**T3**) — a real hazard traded for a small convenience.
- **Q5. Should a time past the end of a video be refused or clamped?**
  **Default:** refused, naming the video's length (**V1**). A clamped event is a wrong timestamp that looks right.
- **Q6. Should the table be able to edit a start/stop's *role* directly** — "make this one the second-half kick-off"?
  **Default:** no. Roles are positional and derived (`scoreboard.rs:317-352`); a stored role would be a second source of truth for the match clock, and it is exactly the mistake the port fixed on the macOS side (BACKLOG #27).
