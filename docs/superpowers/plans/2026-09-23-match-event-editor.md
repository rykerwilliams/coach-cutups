# Match Event Editor Plan (one grammar, typed or pasted)

**Date:** 2026-09-23
**Spec:** `docs/superpowers/specs/2026-09-23-match-event-editor-design.md` (decisions P, T, B, C, V, N, F). It proceeds on the defaults for Q2, Q3, Q4, Q5 and Q6. The user's fixed decisions — time into a file, a list *and* a paste box, a sheet over the app, a time has a colon — are settled in the spec and are not re-litigated here.
**Status:** Not started.

**Scope.** Five tasks: core's grammar and mutator, the two bus commands, the app's headless text, the sheet, and the closeout. No format change (F1), no new crate dependency, no new `UndoAction`, nothing on the recording allow-list.

**Execution.** A fresh subagent per task (`superpowers:subagent-driven-development`), given this plan, the spec and `CLAUDE.md`.
- Tasks run one at a time, in the order written, in one tree. The cargo lock serialises every build anyway.
- The orchestrator runs the gate below and commits each task. It stages paths explicitly, never with `git add -A`.
- **At the end of every task the workspace builds and every test passes.** A task that changes a type fixes every user of it in the same task, in every crate.
- Each task writes its own `CLAUDE.md` addition, where it names one.

**The gate, for every task:** run the `verify` skill, with every cargo call under the machine-wide lock (`flock /tmp/claude-1000/cargo.lock nice -n 19 cargo …`), because other sessions build here too.

**Test-first.** Each task names the test that must fail first. Write it, run it and watch it fail for the stated reason, then build until it passes. A test that fails only because the code doesn't compile yet counts, but the plan names a behavioural failure wherever one exists — and for this feature almost every test can be behavioural, because the grammar is pure.

**What tests may touch.** No test reaches the network, the real camera or mic, or the user's footage. The exception is the `#[ignore]`d `COACH_FOOTAGE` tests, which this plan does not add to. Real footage and anything that identifies a team or player is never committed: the repository is public and the footage shows children. That includes file names, team names and shirt numbers, in code, tests, commit messages and docs. **The grammar's tests use invented team names** ("Rovers", "Green Rovers", "City").

---

## Known facts. Don't re-derive these.

Each was checked in the code while writing this plan.

**The pattern the sheet copies**
- **`HighlightsPanel` is the shape**: the component at `crates/video-coach-app/ui/app.slint:1006`, its `out property <bool> editing` at `:1030`, its read-only `for row in root.rows` at `:1055`, and the selected row's reveal `if row.id == root.selected: VerticalLayout` with its one `LineEdit` at `:1113-1135`. That `LineEdit` uses `text <=> root.label-text` (two-way, out to a window property) and `changed has-focus` for begin/end edit. Copy it; do not invent a table.
- **The one-way-binding warning** is written on `SetupField` (`app.slint:1488-1489`): *"a `text:` binding breaks the moment the user types into it"*. `SetupField`'s `✕` mark is `app.slint:1508-1513`.
- **The live-validator idiom**: a `pure callback` that takes the text as an argument so the binding re-evaluates on every keystroke — the tags field's suggestions (`app.slint:515-518`) and the setup sheet's validators (`app.slint:1552-1562`, fed from `main.rs:855-877`).
- **Sheets**: `ExportSheet` (`app.slint:1314`, `width: 480px`, `height: sheet.preferred-height` at `:1339`), `MatchSetupSheet` (`app.slint:1538`, `520px`, `:1587`). Their scrims are `app.slint:3512` and `:3540`, each `background: #000000a0;` with an empty `TouchArea`. The error dialog's is `:3580`.
- **A list with a height of its own**: `height: min(168px, root.lines * 28px)` at `app.slint:896`, with the comment above it at `:893-894`.
- **The seeding precedent**: `open_match_setup` at `main.rs:932-960` fills every window property from the project and then sets `match_sheet_open` (`main.rs:960`).

**Keys**
- **`text-editing`** is `name-edit.has-focus || inspector.editing || highlights.editing` at `app.slint:2071`. `Inspector.editing` is `app.slint:437-438` and carries the rule: *"**Every** text field belongs here: miss one and typing in it makes Space start playback and `r` start a recording."*
- **`handle-key`'s guard order**: the error dialog `app.slint:2240-2246`, the export sheet `:2248-2254`, the match setup sheet `:2255-2265` (`return reject` at `:2264`), then the `text-editing` yield `:2266-2270`.
- **The Ctrl branch** is `app.slint:2275-2303`, ending in `return reject` at `:2302`. **It is never reached from inside a sheet**, because the sheet's branch returns first — so the editor's own `return reject` is what delivers `Ctrl+V` to the paste box.
- **The Esc cascade** is `keys := FocusScope`'s `key-pressed` at `app.slint:2439-2445`: `if (root.text-editing && event.text == Key.Escape) { keys.focus(); return accept; }`, commented at `:2437-2438`.
- **`keys.focus()` from a sheet works** — the export sheet's close already does it (`app.slint:3533-3535`) — because the sheets are children of `keys`.
- **`handle-key` falls through to `reject` at `app.slint:2427`**, so `e` and `m` are free. The three tag keys are `app.slint:2312-2324`.

**The bus**
- **The funnel** is `Bus::edit_match_events` at `crates/video-coach-app/src/bus/scoreboard.rs:103-118`: clone before, run the closure, clone after, **drop a no-op at `:112-115`**, `save()`, `record(UndoAction::EditMatchEvents { before, after })`, `publish_project()` — once, whatever the closure did. Everything in this feature goes through it.
- **`tag_match_event`** (`bus/scoreboard.rs:34-57`) shows the two refusals to copy: an out-of-range source index at `:41-43` (silent `eprintln!`) and the cap at `:45-52` (a `UserError::Scoreboard` notice with the exact wording).
- **The recording allow-list** is the `matches!` in `Bus::command` at `bus/mod.rs:762-793`; it is **deny-by-default** and refuses silently with an `eprintln!` at `:792`. `TagMatchEvent` is on it at `:786`, with the comment at `:783-785`: *"Deleting and the setup sheet wait, as every other edit does."*
- **`UserError::Scoreboard(String)`** is `bus/mod.rs:437`, and `is_notice` returns true for it at `:445-454`.
- **The command enum**: `TagMatchEvent` at `bus/mod.rs:112-116`, `DeleteMatchEvent` at `:117`, `SetReelTrim` at `:118-126`, and the dispatch arms at `:810-817`.
- **Two writers publish `ProjectChanged` with no command behind them**, and both can land while the sheet is open:
  - a transcript arriving for a clip — `crates/video-coach-app/src/bus/transcribe.rs:451-462`, `project_changed()` at `:461`;
  - a source found missing after a player error — `crates/video-coach-app/src/bus/transport.rs:365-367`, `refresh_missing()` then `publish_project()`.

  This is why spec T3 is an invariant and not an observation.
- **`UndoAction::EditMatchEvents`** is `crates/video-coach-core/src/undo.rs:70-74`, and `purge_for_source_change` already drops it from both stacks (`undo.rs:154-160`). No new variant, no new purge rule.

**The app's rendering**
- **`MatchRowText`** is `crates/video-coach-app/src/match_panel.rs:28-42`; `match_rows` builds it from `labelled_events` at `:49-62`; `reel_span` is `:65-77`.
- **`over_cap`** is `match_panel.rs:152-155` and **subtracts a place for the back-anchor**; `over_cap_warning` is `:162-173`. `Project::start_stops_at_cap` (`crates/video-coach-core/src/scoreboard.rs:790-801`) counts **records** and does not. **They deliberately differ** — see spec V5, and the doc at `scoreboard.rs:310-316`.
- **The shared-parse discipline** is written at `match_panel.rs:241-246`: *"the sheet's 'this field is good' mark and the parse that builds the config call the same one and can't drift apart. Written on both sides they did."*
- **`show_match`** is `main.rs:1005-1039`: one `match_rows` call feeds the scrubber's marks, `match-list-lines` and the rows. **`show_project`** is `main.rs:2018-2075` and calls it at `:2066`; the `ScoreboardContext` rebuild with its "here and nowhere else" comment is `:2068-2073`.
- **A row's Go already exists**: `window.on_seek_match_event` at `main.rs:764-778` takes an event id, looks up `abs_seconds` and sends `Command::ScrubRelease`. `[` / `]` do the same at `main.rs:814-834`, with the optimistic `ui.target_abs` at `:827`.
- **The status bar's notice** is `app.slint:3461`, inside the window's layout — **behind** the scrims at `:3512` and `:3540`. That is spec C5's reason for the sheet's own message line.
- **`format::format_hms_tenths`** already exists and already renders `M:SS.t` / `H:MM:SS.t`, floored: `crates/video-coach-app/src/format.rs:21-30`, with its test at `:82-93`. **`format.rs` cannot move to core**: it imports `gstreamer::glib` at `format.rs:3` for `finish_at`, and core declares no media dependency.

**Core**
- **`MatchEventRecord`** is `scoreboard.rs:198-222`; the trims are `Option<f64>` with field-level `#[serde(default)]` at `:210-222`.
- **`interpret`** is `scoreboard.rs:317-352`. It filters to `StartStop`, **sorts by absolute time only** (`:324-326`), inserts the derived back-anchor start at `first − period_seconds(0)` (`:328-334`), truncates to `expected_start_stop_events()` (`:335`), then walks positionally. `labelled_with`'s sort is stable (`:409`), so two events at one instant keep stored order.
- **`labelled_events`** is `scoreboard.rs:422-435`; goals read `"Home goal"` / `"Away goal"` regardless of the scoreboard, and a role-less start/stop reads `"Start/stop (no period)"` with `role_less = true`.
- **`team_name`** is `scoreboard.rs:167-174`. **`absolute_match_events`** is `scoreboard.rs:716-724`, derived per call.
- **`SourceRef.display_name`** is `project.rs:123`; `duration_seconds` is *"**the** duration authority"* at `project.rs:111-117`. `Project::abs_seconds` is `project.rs:282-285`, `locate` is `:298-310`.
- **Core's dependency audit** lists exactly `serde`, `serde_json`, `thiserror`, `uuid`. `match_entry.rs` adds none.

**Tests**
- The harness's match-event tests are `crates/video-coach-harness/tests/match_events.rs`; its `no_project_changed` helper is at `:78`.

---

## Task 1: The grammar, in core

Everything the coach can type, parsed once. It is pure, so it runs on CI with no GStreamer, and it is where all but a handful of this feature's tests live.

**Files:**
- `crates/video-coach-core/src/match_entry.rs` (new), `crates/video-coach-core/src/lib.rs`
- `crates/video-coach-core/src/scoreboard.rs`
- `crates/video-coach-core/tests/match_entry.rs` (new), `crates/video-coach-core/tests/scoreboard.rs`

**What to build:**

1. **`parse_time(&str) -> Option<f64>` and `format_time(f64) -> String`** (spec T5).
   - Accepts `m:ss`, `mm:ss`, `h:mm:ss`, each optionally `.t`. The leading field is unbounded (`75:20` is 4520 s); every non-leading field is `< 60`. Trimmed.
   - **A colon is required.** No bare number, ever.
   - `format_time` renders `M:SS.t` / `H:MM:SS.t`, **floored in integer tenths**, exactly as `format::format_hms_tenths` does (`format.rs:21-30`). The duplication is deliberate and documented on the function: `format.rs` imports glib, core takes no media dependency, and core needs its own because it builds the echo text.
2. **The vocabulary** (spec B2), as one lookup over normalized tokens (lowercase; `-`, `_`, `,` → space; runs collapse):
   - `HomeGoal`: `home`, `hg`, `z`. `AwayGoal`: `away`, `ag`, `x`. `StartStop`: `v`, `start`, `stop`, `end`, `period`, `half`, `ht`, `ft`, `fulltime`, `halftime`.
   - Ignored: `goal`, `goals`, `at`, `the`, `scored`.
   - **Refused, with their own reason: `kickoff`, `kick`, `ko`, `restart`, `whistle`.** They must not reach `StartStop` by any path. The reason names the damage: `interpret` is positional (`scoreboard.rs:317-352`), so one spurious start/stop moves every later period boundary and the clock burned into every export.
   - **Team names match as a contiguous run of tokens** inside the remainder, against the normalized `ScoreboardConfig` names, so a two-word name works. Not used at all when one normalized name equals or contains the other.
   - Verdict: exactly one distinct kind → that kind; none → `no event word`; two or more → `ambiguous`; any unknown token → `ambiguous`.
3. **`parse_line(&Project, default_source, &str) -> LineVerdict`** (spec B1), where the line is `[<video>] <time> <words>`:
   - `#` to end of line is a comment; an empty remainder yields nothing at all.
   - **A leading bare integer is always a video number.** Out of `1..=source_videos.len()` it is refused as *"there is no video N"* — and where the integer is large enough to look like seconds, the reason adds *"a time needs a colon (15:00)"*.
   - Then a time (a colon required), then the kind.
   - Bounds: `0.0 <= source_seconds <= source_videos[i].duration_seconds`, refused naming the length, never clamped (spec V1).
4. **`format_line(&Project, &MatchEventRecord) -> String`**: the canonical rendering the row's field is seeded with — `<n> <format_time> home goal` / `away goal` / **`period`**. `period` is the neutral `StartStop` word: the stored kind does not know whether it starts or ends a half, and `interpret` decides. `parse_line(format_line(e))` must give `e` back.
5. **`edit_from_line(seed: &str, typed: &str, stored_seconds: f64, …) -> …`** (spec T5's last bullet): when the **time token** of `typed` is byte-identical to the seed's, the result carries `stored_seconds`, not `parse_time`'s value. Otherwise it carries the parsed value. Without this, changing only the kind re-rounds a stored `14.06` to `14.0`.
6. **`PendingMatchEvent { kind, source_index, source_seconds }`** and **`parse_batch(&Project, default_source, text) -> Batch { events, lines, leftover }`** (spec B3, B5, V3, V4):
   - `lines` is one echo record per non-blank line, in input order, with one of three verdicts: added, already there, refused-with-a-reason.
   - **The duplicate rule and the cap both count `existing + accepted-so-far`.** A block containing the same line twice adds it once; a batch that would pass `start_stops_at_cap` has its excess start/stops refused and the rest added.
   - `SAME_EVENT_SECONDS = 1.0`: same kind, same source, within a second.
   - `leftover` is exactly the lines that were not added, joined back into a block for the box.
7. **`Project::edit_match_event(id, kind, source_index, source_seconds) -> bool`** in `scoreboard.rs` (spec T6): mutates in place, keeping the `id`; clears both reel trims when the kind stops being a goal; keeps them across home ↔ away; `false` for an unknown id.

**Test that must fail first:** `a_kick_off_word_is_never_a_period_boundary` in `core/tests/match_entry.rs` — every one of `kickoff`, `kick-off`, `ko`, `restart`, `whistle` on a well-formed line yields a **refusal whose reason mentions the period shift**, and none of them yields `StartStop`. It fails on an empty module, then fails behaviourally if the word is ever added to the `StartStop` list.

Then the rest, all behavioural:
- `parse_time` on `0:05`, `14:05`, `75:20`, `1:02:03`, `14:05.5`; refusing `1405`, `14`, `14:60`, `-1:00`, `14:5:3`, `""`.
- `format_time` floors (`14.06` → `0:14.0`), has an hours form, and round-trips within a tenth.
- The vocabulary: each word, mixed case, hyphens and commas, comments, blanks, the ignore list, bare `goal` unresolved, `home away` ambiguous.
- Team names: one word; **two words as a contiguous run**; a trailing ignored word; **not matched** when one name contains the other.
- The video number: present, absent (the default), **out of range giving "there is no video N"**, `900 home goal` refused as a video and not read as 15:00, a leading integer with no time after it.
- `parse_line` ↔ `format_line` for all three kinds; `edit_from_line` carrying `14.06` when only the kind word changed, and the parsed value when the time text changed.
- `parse_batch`: the duration bound; the duplicate rule at exactly 1.0 s either side; **the same line twice adds once**; the cap counted incrementally; `leftover` holding exactly the un-added lines.
- `edit_match_event` in `core/tests/scoreboard.rs`: the id and untouched fields survive; goal → start/stop clears both trims; home ↔ away keeps them; unknown id is `false`; a re-timed event reorders `labelled_events` and can change its role; **re-timing the earliest start/stop under `auto_back_anchor_p1` moves every later clock reading** (spec V6).

**Verify:** the gate. `cargo test -p video-coach-core` alone must pass with no GStreamer, and the dependency audit must still list exactly the four crates.

**CLAUDE.md:** nothing yet — Task 4 writes the one paragraph this feature earns.

Commit: `feat(core): one grammar for typed and pasted match events`.

---

## Task 2: The two commands

**Files:**
- `crates/video-coach-app/src/bus/{mod.rs,scoreboard.rs}`
- `crates/video-coach-harness/tests/match_events.rs`

**What to build:**

1. **`Command::EditMatchEvent { id, kind, source_index, source_seconds }`** and **`Command::AddMatchEvents(Vec<PendingMatchEvent>)`** in `bus/mod.rs`, beside `TagMatchEvent` (`:112-126`), each with a doc saying the times are **typed, not captured**, and why that satisfies the caller-captured rule trivially (spec C2).
2. **Both go through `edit_match_events`** (`bus/scoreboard.rs:103-118`), so a twenty-event batch is one save, one undo step and one `ProjectChanged` (spec C3).
3. **The refusals**, aggregated into **one** `UserError::Scoreboard` notice per command — *"3 of 7 added: 2 are past the end of the second half, 2 are already tagged"* — using the existing cap wording verbatim where the cap is the reason (`bus/scoreboard.rs:45-52`). An out-of-range source index is refused as `tag_match_event` refuses one.
4. **Neither joins the recording allow-list** (`bus/mod.rs:762-793`). Deny-by-default means this is *not* a code change; it is a thing not done, and the task must not add an arm.
5. **The rules live in core.** The bus calls `parse_batch`'s output; it does not re-implement the bound, the duplicate rule or the cap count. The only thing it owns is the aggregate sentence.

**Test that must fail first:** `a_pasted_batch_is_one_undo_step` in `crates/video-coach-harness/tests/match_events.rs` — send `AddMatchEvents` with five events into a two-source project; exactly **one** `ProjectChanged` arrives, the saved project holds all five in match order, and one `Undo` restores the prior list exactly.

Then:
- `a_partly_refused_batch_adds_the_rest`: seven lines, two past the end of source 2 and two already tagged; three land, one `UserError::Scoreboard` notice names all four refusals, and the file on disk matches.
- `an_edit_moves_an_event_and_the_clock_follows`: `EditMatchEvent` re-times the first-half end; `ScoreboardContext::state_at` at a later instant reads differently afterwards; one `Undo` puts it back.

**Not tested here, on purpose:** that a batch adding nothing publishes nothing, and that a batch is dropped while recording. Both are properties of machinery these commands only pass through — the funnel's no-op guard (`bus/scoreboard.rs:112-115`) and the allow-list's deny-by-default `matches!` (`bus/mod.rs:762-793`) — each already pinned by its own tests. Re-testing them per command makes adding a command more expensive than it is.

**Verify:** the gate.

Commit: `feat(app): edit and bulk-add match events through the bus`.

---

## Task 3: The sheet's text, headless

Every string the sheet shows, in `match_panel.rs`, so it is tested without a window — the module's standing rule (`match_panel.rs:1-7`).

**Files:**
- `crates/video-coach-app/src/match_panel.rs`
- `crates/video-coach-app/src/format.rs` (test only)

**What to build:**

1. **`MatchRowText` gains `source_index: usize` and `source_seconds: f64`** (spec T1), filled in `match_rows` from the same record. **No second builder**: `editor_rows` would be a second `labelled_events` call and a second chance to order events differently from the panel, the scrubber's marks and `[` / `]`. The panel ignores the two new fields; `show_match` (`main.rs:1005-1039`) needs no change beyond compiling.
2. **`editor_row_line(&Project, &MatchRowText) -> String`**, a thin call to core's `format_line`, and the `<n> · <m:ss.t>` "where" text the read-only row shows.
3. **The echo lines and the summary** (spec B3): one function turning core's `Batch` into the three glyph-and-sentence lines and the *"7 events to add · 1 already tagged · 2 lines refused"* summary, plus the **"Add 7 events"** button label. Wording lives here, verdicts live in core.
4. **The over-cap warning is reused unchanged** (`over_cap_warning`, `match_panel.rs:162-173`). Do **not** reconcile `over_cap` with `Project::start_stops_at_cap`: spec V5 says why they differ, and the difference is load-bearing.

**Test that must fail first:** `the_echo_reads_back_every_verdict` in `match_panel.rs`'s test module — a block with one good line, one duplicate, one past the end and one kick-off word produces four echo lines with the right glyphs and reasons, and the summary reads *"1 event to add · 1 already tagged · 2 lines refused"* (singular and plural both pinned).

Then:
- `match_rows` carries the source index and the in-source time for an event on the second source.
- The row's line for each kind, and that it is exactly what core's `format_line` gives.
- **`format_time` and `format_hms_tenths` agree** over a table (`0.0`, `14.06`, `754.99`, `3723.45`), in a test in `format.rs` — the pin that keeps the two renderers from drifting (spec, Crate responsibilities).

**Verify:** the gate.

Commit: `feat(app): the match editor's rows, echo and summary`.

---

## Task 4: The sheet

**Files:**
- `crates/video-coach-app/ui/app.slint`
- `crates/video-coach-app/src/main.rs`
- `CLAUDE.md`

**What to build:**

1. **`MatchEditorSheet`**, a fourth copy of the sheet shape (`app.slint:3512`, `:3540`): a `#000000a0` scrim, an empty `TouchArea`, `width: 640px`, `height: sheet.preferred-height`. Top to bottom: the event list, the paste box with its default-video `ComboBox`, the echo list with its summary and **Add**, one **message line**, **Done**.
2. **The event list is `HighlightsPanel`'s pattern** (`app.slint:1006-1135`), copied not reinvented: a `for` of read-only rows (where · reads-as · `→` · `×`), and `if row.id == root.selected` revealing **one `LineEdit`**, two-way bound out to a window property, with `SetupField`'s `✕` (`app.slint:1508-1513`) when the line does not parse. Its height follows the house idiom (`app.slint:896`).
3. **The live echo** is a `pure callback check-paste(string) -> [PasteLine]` taking the text as an argument, so the binding re-evaluates on every keystroke (`app.slint:515-518`, `:1552-1562`).
4. **The key guard** (spec P3), placed with the other sheets (`app.slint:2255-2265`):
   ```slint
   if (root.editor-open) {
       if (pressed && event.text == Key.Escape && !root.text-editing) {
           root.close-match-editor();
           return accept;
       }
       return reject;
   }
   ```
   and **`text-editing` at `app.slint:2071` gains `|| match-editor.editing`**, without which the `!root.text-editing` test is always true, the first Esc closes the sheet and the cascade at `:2439-2445` is dead code. That `return reject` is also what delivers `Ctrl+V`; the Ctrl branch at `:2275-2303` is never reached.
5. **Every commit drops focus** (spec T4): the commit callback sends its command and then calls `keys.focus()`, unconditionally. Slint's `for` reuses items by index, so a commit that reorders the list would otherwise leave the focused field over a different event. The row stays *selected*; selection is not focus.
6. **The rebuild invariant** (spec T3). `main.rs` keeps one flag, set when the sheet sends `EditMatchEvent`, `AddMatchEvents` or `DeleteMatchEvent`; **`show_project` (`main.rs:2018-2075`) rebuilds the sheet's rows only when it is set**, and clears it. The open path seeds them once, as `open_match_setup` seeds the setup sheet (`main.rs:932-960`). Write the comment naming the two writers this protects against — a transcript landing (`bus/transcribe.rs:461`) and a source found missing (`bus/transport.rs:365-367`) — because the next reader will otherwise "simplify" the flag away.
7. **The message line** (spec C5): `in property <string> message`, one line above Done. While the sheet is open, `main.rs` routes a `UserError::Scoreboard` to it **as well as** to the status bar — the status bar's notice (`app.slint:3461`) renders behind the scrim, so it is invisible until Done. Cleared on close and on the next command.
8. **The paste box's text is a window property**, so it survives the sheet closing and a row's Go (spec F2, T10). Cleared when a project is opened or closed. The default-video choice resets on each open.
9. **Go reuses `on_seek_match_event`** (`main.rs:764-778`) and then closes the sheet. It does not touch the paste box.
10. **The button**: "Edit events…" in the Match panel's header row beside Setup… (`app.slint:825-839`), enabled on the panel's `can-edit` **and** the project having a source (spec P4), `keys.focus()` first.

**Test that must fail first:** there is no headless test for a Slint sheet, and the plan does not pretend otherwise — Task 3 already pins every string and Task 1 every verdict. **The failing-first artefact here is a screenshot pass**, written and run before the sheet exists:
- a scratch project (`XDG_CONFIG_HOME` pointed at a scratch dir, per CLAUDE.md) with four match events and two sources;
- the sheet open with a row selected and its field showing `2 14:05.0 home goal`;
- the paste box holding a block with one of each verdict, including a `kickoff` line, and the summary beneath it;
- a refusal on the message line.

Capture all four; the first run cannot produce them, which is the failure.

**Hands-on (batched, the user's eyes — Task 5 collects them):** the Esc order, `Ctrl+V`, the paste box surviving Go, and the greying during a recording.

**Verify:** the gate, plus the screenshot pass.

**CLAUDE.md:** one paragraph under the match-clock rules:
- match events are entered by `z` / `x` / `v` at the playhead, or typed in the editor sheet's **one grammar** (`core::match_entry`), which the row field and the paste box share;
- **a time has a colon, and a leading bare integer is a video number** — never a time;
- **kick-off words are refused, not read as period boundaries**: `interpret` is positional, so a spurious start/stop moves every later period and the clock in every export;
- the sheet's rows are rebuilt by its own committed edit and **never** from `show_project`, because a transcript landing or a source found missing publishes `ProjectChanged` too;
- a sheet with fields folds its `editing` into the window's `text-editing`, or Esc never leaves the field.

Commit: `feat(app): the match event editor sheet`.

---

## Task 5: Closeout

1. **Adversarial review** of the whole diff (the `adversarial-review` skill, CLAUDE.md's pattern). Apply, skip or defer.
2. **Backlog** what is deferred, in `BACKLOG.md`'s format, including the spec's Deferred list — with **"copying the list out in the same grammar"** marked *revisit first*: `format_line` already exists after Task 1, so it is a button and a clipboard call.
3. **`docs/hands-on-checklist.md`:** a section in the checklist's own voice:
   - **[must work] Paste a half's notes.** The rows read `1H start` / `1H end` / `2H start` / `2H end` in that order, and the scrubber's marks land where the times are (seek to two of them).
   - **[must work] Paste `kickoffs.txt` unedited.** Every line is refused, and the reason explains why a restart is not an event. **Nothing is added.**
   - **A wrong time.** Fix one row by retyping its line; Go jumps there and the picture confirms it.
   - **Only the kind.** Change `home goal` to `away goal` on a row and check the time did not move.
   - **Keys.** `Ctrl+V` pastes into the box; the first Esc leaves the field, the second closes the sheet; the box still holds its text after Go and a reopen.
   - **Gating.** The button is greyed during a recording and a preview.
   - **Undo.** Done, then `Ctrl+Z`: a whole pasted batch goes in one step.
   - **Your call:** whether the editor wants a keyboard shortcut (Q3), and whether "no `Ctrl+Z` while the sheet is open" grates (Q4).
4. **Check each task's CLAUDE.md addition is there** and still accurate.

Commit: `docs: close out the match event editor` (along with the review's fixes, in their own commits).

---

## The user's own steps

1. **Install the build that carries this** (there is no release point of its own in this plan; it rides whatever version ships next) and run the checklist section above, in a **copy** of a tagged project — the editor writes to `project.json` as it goes, and there is no Cancel (spec P5).
2. **Paste one real half's notes** and check the four period rows before trusting the clock in an export.
3. **Answer Q2–Q6** if any of the defaults grate. Q3 (`e` as a shortcut) and Q4 (undo while the sheet is open) are the two most likely to.

## Deliberately not in this plan

- Everything in the spec's Deferred list: numeric reel trims, the reel's effective span (BACKLOG #74), copying the list out, loading a paste from a file, multi-select, a session-wide undo step, frame nudging, and a stored "restart" kind.
- **A format change.** `CURRENT_FORMAT_VERSION` stays 11 (spec F1); no task touches `store.rs`.
- **Any change to `interpret`, the reel, chapters or the scoreboard's rules.** This feature is a second way to write the same three fields `z` / `x` / `v` already write, and nothing more.
