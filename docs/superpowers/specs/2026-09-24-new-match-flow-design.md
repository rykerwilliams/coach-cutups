# New match: one sheet that makes the folder, names the project and sets the teams

**Date:** 2026-09-24
**Status:** Draft, for adversarial review.
**Builds on:** the project lifecycle (`bus/project.rs`, spec D6), the source list (`bus/sources.rs`, spec D7), the match setup sheet and `ScoreboardConfig` (Phase 9 spec S5), the shared `Sheet` / `Scrim` extracted for the match event editor (`docs/superpowers/specs/2026-09-23-match-event-editor-design.md` P1), and the export file-name rules (Phase 8 spec E6).
**Evidence:** the code as it stands on `claude/intelligent-lamport-m2indd`, and a read-only inspection of the coach's two real footage trees on 2026-09-24. Every claim carries a `file:line` or an observation.

Labels: **[cited]** points at a file in this repository or a decision recorded in another spec. **[observed]** is something read off the coach's own disks on 2026-09-24 — described structurally (`<club>`, `<season>`, `<match>`, `<opponent>`), never by name. There is no **[measured]** claim in this document.

---

## Goal

The coach asked for this in these words:

> *"the project creation flow is a bit wonky. in general i will have a folder with the game files. so like, i'd want the app to help create the folder? note the pattern of having a 'coach-cuts' folder and then projects inside it? so if the app could help with a uniform naming scheme."*

Today, starting a match takes four separate acts, in a fixed order, none of which knows about the others:

1. **`Open Project…`** — a folder picker (`Pick::ProjectFolder`, `pickers.rs:17`) whose result becomes `Command::OpenProject` (`main.rs:363-370`). The folder must **already exist**: `open_project` refuses one that doesn't, because "saving never creates one" (`bus/project.rs:34-44`). So the coach creates the folder in the picker's own *New Folder* button, or in a file manager, before the app is involved at all. The project's name is then whatever that folder is called — `Project::new(folder.file_name())` (`bus/project.rs:47-52`).
2. **`Add Source Video…`** — `Pick::Videos`, one `Command::AddSource` per file (`main.rs:371-385`), each one a separate probe, gate, save and publish (`bus/sources.rs:26-43`).
3. **`Set up teams…`** — the match setup sheet, `Command::SetScoreboard` (`app.slint:899`, `main.rs:848-859`).
4. **Renaming the project** — the title field, `Command::RenameProject` (`main.rs:428-442`), which a coach who got the folder name right never does.

**What that produces on the coach's own disk** [observed]:

| | Tree A | Tree B |
|---|---|---|
| footage | `<club>/<team>/game-videos/<match>/P1.mp4, P2.mp4` | `<club>/<season>/<match>/<two files from one camera app>` |
| projects | `<club>/<team>/coach-cuts/<match>/project.json` | `<club>/coach-cuts/<match>/project.json` |
| folder names | `2026-09-19-<opponent>` | `<8-digit date>-<opponent>` |
| project names | `"<club> <team> - <opponent>"` and `"<team> vs <opponent> <date>"` — **two different schemes in two sibling projects** | `"<8-digit date>-<opponent>"` — the raw folder name, i.e. nobody ever renamed it |

And one of Tree B's two project folders **contains nothing at all** — no `project.json`, no `recordings/`. It is a folder made in a picker or a file manager that the app was never pointed at. That is the wonkiness, on disk: the folder is created by hand, outside the app, and nothing guarantees the app ever finishes the job.

This spec collapses steps 1–4 into **one sheet**, prefilled from the footage the coach picked, and one all-or-nothing command. The four existing routes stay exactly as they are (**E3**).

---

## Scope

**The user's decisions of 2026-09-24. This spec follows them and does not reopen them:**

- **D-1. The project folder goes in a `coach-cuts/` folder beside the videos.** The app finds the sensible parent and creates `coach-cuts/<match>/`. **The footage is left where it is** — never moved, never copied.
- **D-2. The name carries the date and both teams.** Folder `2026-09-17-<home>-<away>`; project name `<Home> v <Away>`. The date comes from the video files. The coach can edit everything before anything is created.

**In scope:** the entry points, the inference, the sheet, the one command, the naming rules, and the failure behaviour.

**Out of scope, unchanged:** the match setup sheet itself (it remains the way to edit teams afterwards, and the only way to reach overtime, the font colours and the back-anchor); `Open Project…`; `Add Source Video…`; relink; export; anything to do with clips, recording or the match clock.

---

## Decisions

### E. How the flow is entered

**E1. A "New match…" button in the transport row, immediately left of `Open Project…`, and the primary action of the no-project empty card.** Both, not one.

The transport row already holds `Open Project…` and `Add Source Video…` side by side (`app.slint:4031-4040`), and the empty card `"No project open"` offers `Open Project…` as its single action (`app.slint:3701-3706`). A coach who has just launched the app sees the card, not the toolbar; a coach who has a project open and wants the next match sees the toolbar, not the card. Putting it in one place only would miss one of those two.

`EmptyCard` takes one `action` and one `clicked()` (`app.slint:274-317`). It gains an optional `secondary` string and `secondary-clicked()`, rendered as a plain (non-`primary`) button beside the first. The no-project card becomes **primary "New match…"**, **secondary "Open Project…"** — new is the common case, opening an old one is the exception. The other two cards pass no `secondary` and are unchanged.

**Gating is exactly `Open Project…`'s**: `enabled: !root.recording` (`app.slint:4033`). No new rule. The command is not on the recording allow-list, which is deny-by-default (`bus/mod.rs:807-835`), so a UI bug reaches a silent refusal rather than a half-built project mid-take.

**E2. It opens the existing multi-select video picker first, and the videos decide the folder.** `Pick::Videos` already exists, already filters on `VIDEO_EXTENSIONS` in both cases (`pickers.rs:36-39`), already multi-selects, and already parents itself to the window on the portal without blocking the event loop (`pickers.rs:1-4`). Nothing new is needed.

Picking the **files** rather than the folder is deliberate, and it is what lets this spec answer *"which files are game videos at all"* honestly: **the coach's selection is the answer, and the app never guesses.** A folder-first flow would have to scan and then decide which of a folder's files are halves and which are a stray phone clip, a `.txt` of kick-off times or a thumbnail — a guess with no good failure mode. The desktop dialog is already the right place to look at names and sizes.

The match folder is then derived, not asked for: **the deepest common ancestor directory of the chosen files** (**I1**). For both of the coach's trees that is the `<match>` folder [observed].

**E3. Nothing is removed.** `Open Project…`, `Add Source Video…`, `Set up teams…` and the rename field all stay, unchanged and un-deprecated. They are how an existing project is opened, how a third camera angle is added a week later, and how the teams are corrected. The new flow is a fifth route, not a replacement.

**E4. `Pickers::open` hands back the whole selection once, instead of calling back per file.** Today it sorts the portal's answer by file name and then calls `then` once per path (`pickers.rs:97-101`), with the doc comment *"camera files are named by when they were shot, so sorting them is what puts a game's halves in sequence."* **That is false for one of the coach's two camera systems** (**I6**), and the new flow needs the whole set at once anyway, to find the common ancestor and to order it.

So the signature becomes `then: impl FnMut(Vec<PathBuf>)`, the sort moves out of `pickers.rs`, and `on_add_source` (`main.rs:371-385`) orders the selection with the same core function the new flow uses before sending its `AddSource`es. That is a fix to the existing path, not only new code, and it is the sort of adjacent change this project's values call for.

### I. What the app infers, and how sure it is

**I0. Every inference is a prefilled, editable field. Nothing is decided silently, and nothing is created until Create is pressed.** Where an inference is weak, the field carries one short provenance line under it; where it is strong, it carries nothing, because a hint on every field is a hint on none. Where there is nothing to infer, the field is empty and Create is disabled until it is filled.

**I1. The match folder is the deepest common ancestor of the chosen files.** With every file in one directory — both of the coach's trees, every time [observed] — that is that directory. With files from two directories it is their shared parent, and the opponent inference (**I4**) will usually find nothing there, which is the correct outcome: an empty field rather than a wrong one.

**I2. The date, in this order. A bare year is never a date.**

1. **A full date in a file's name.** The shapes accepted are `YYYYMMDD`, `YYYY-MM-DD`, `YYYY_MM_DD` and `YYYY.MM.DD`, each bounded by a non-digit or an end of string, with month `1..=12`, day `1..=31` and year `2000..=<this year + 1>`. Leftmost wins. If every chosen file yields the same date, that is the answer — **strong, no hint**.
2. **A full date in the match folder's name**, same shapes, same bounds — **strong, no hint**.
3. **The earliest chosen file's mtime**, resolved in the local zone — **a guess, with the hint** *"guessed from the file's date — check it."*
4. Nothing (a future-dated or unreadable mtime): the field is empty, Create disabled.

**Requiring a full date is the whole rule**, and it is what stops two real traps in the coach's own trees [observed]: a team folder whose name is a **four-digit birth year**, and a match folder whose name begins with a **six-digit age-group code**. A `YYYY` matcher would date every match in Tree A to a decade ago; a `YYMMDD` matcher would read that six-digit code as a month of 15. Both are rejected by requiring eight digits with a valid month and day. Tree A's file names carry a leading `YYYYMMDD` **and** a bare year later in the same name; leftmost-first plus the eight-digit rule takes the right one.

**Why mtime is third and is labelled a guess**, rather than second: in Tree A the footage mtime is **two days** after the date the coach's own folder name gives; in Tree B it is **one day** after [observed]. These are downloads, and they are downloaded when the coach gets to it. `bus/export.rs:585-593`'s doc says mtime *"is within a day of the match"*; Tree A shows two. It is close enough to be a useful prefill and not close enough to be trusted (**Deferred 3**).

**I3. The container's creation time is unusable, and the probe is not extended to read it.** Both of the coach's camera systems write `creation_time = 1904-01-01T00:00:02Z` into every file — the QuickTime epoch with a two-second offset, i.e. a field that was never set [observed]. Taking it would date every match in both trees to 1904. `video_coach_media::probe` returns duration and display aspect and nothing else (`probe.rs:18-25`); adding a creation-time field to it would be a media change made to serve a value that is provably garbage on 100% of the coach's footage. **Not done.** If a coach ever shoots on a phone, whose containers do carry a real one, revisit (**Deferred 2**).

**I4. The opponent comes from the match folder's name, and only from there.**

Take the match folder's file name; strip a leading run of digits and separators (that is either a date or a code, and in neither case a team); strip a trailing run of digits and separators; replace `_`, `-` and `.` with spaces; collapse spaces; trim. If what is left is non-empty, title-case each word and use it — **strong, no hint**. Otherwise the field is left empty.

Both of the coach's trees yield the opponent's name cleanly under this rule [observed]: one after stripping a `YYYY-MM-DD-` prefix, the other after stripping a six-digit code and its hyphen.

**File names are deliberately not mined for the opponent.** Tree A's file names do contain it, wrapped in a date, an age group, a club abbreviation, a separator that is itself a hyphen-underscore sandwich, and a half marker [observed]. Anything that pulled a team out of that would be a pattern fitted to one camera system, and it would quietly produce nonsense on the other, whose file names carry no team at all. The folder name is what the coach already curates.

**I5. The club, the colours and the match format are seeded from the projects already in the chosen `coach-cuts/` folder.**

A coach's own team, kit colours and match format are the same for every match of a season; the opponent is the only thing that changes. So, having resolved where `coach-cuts/` is (**W1**):

- Read every `project.json` directly under it with `store::read` (`store.rs:97`). Unreadable and legacy files are skipped silently — this is a prefill, not an operation.
- **The club is the team name that appears in the most of them**, counted case-insensitively over both `home.name` and `away.name`, needing at least **two** appearances to count. Its `TeamConfig` comes from the most recent project it appears in, colours and all, and it is seeded into **the slot it occupied there**.
- **The other slot gets the opponent from I4**, with the colours that slot carried in the most recent project — the previous opponent's kit, which is wrong, but is a colour the coach is about to change rather than a blank he must invent.
- **The match format** comes from the most recent project with a scoreboard.
- **With fewer than two prior projects** there is no recurring name. Seed from the most recent project if there is one (both slots verbatim), put the opponent in the **home** slot, and leave the rest to the coach. With no prior project at all, seed `match_panel::blank_config()` (`match_panel.rs:319-337`) — which is what the setup sheet already opens with on a project that has no scoreboard (`main.rs:1100-1111`) — with the opponent in the home slot and `MatchFormat::default()` (`scoreboard.rs:60-70`).

**Home is the venue's team, and the app cannot know it.** In the coach's own tree the opponent sits in the **home** slot and his club in **away** — an away fixture [observed] — and the user's own example for this spec (`…-<opponent>-<club>`, `<Opponent> v <Club>`) has the same shape. That is a fact about where the match was played, which is in no file and in no folder name. So the sheet has a **⇄ Swap** button (**S3**), and the folder name and project name follow the slots live, which is the feedback that makes a wrong guess obvious before Create.

**Only the two names are seeded per prior project, plus one config each.** No index is built and nothing is cached: the read happens once, when the sheet opens, over a directory that holds a handful of small JSON files.

**I6. The order of the videos: half markers, then the download suffix, then mtime — never a raw file-name sort.**

The existing rule is a byte sort on the file name (`pickers.rs:96-101`). **On one of the coach's two camera systems it puts the second half first.** That system names a game's two files `<Stem>.mp4` and `<Stem> (1).mp4` [observed]; `' '` (0x20) sorts before `'.'` (0x2E), so the byte sort yields `<Stem> (1).mp4`, `<Stem>.mp4` — the halves reversed. The coach's stored project has them the right way round [observed], so he either added them one at a time or fixed it afterwards. Reversed halves are silently wrong for the whole project: the match clock, the concat timeline, every clip's position.

`core::ordering::order_videos(&[PathBuf]) -> Vec<PathBuf>` applies, in order:

1. **A half marker.** If *every* name contains exactly one match of `(?i)\b[pP]?(\d)\b` in a period-marker position — in practice `P1`/`P2`, `-1`/`-2`, `half1`/`half2`, `1st`/`2nd` — order by that number. Tree A's files carry `-P1`/`-P2` [observed]. Strong.
2. **The download suffix.** Strip a trailing space-and-parenthesised-number — `(n)` preceded by one space — from each stem. If every name shares one stem after stripping, order by `n`, with the bare stem counting as `0`. That gives Tree B's files their correct order, and it is exactly what a browser's duplicate-download naming means: the first download is the bare name. Strong.
3. **mtime, ascending.** The fallback for names with no signal.
4. The byte sort, to make the function total and deterministic.

**And a disagreement is shown, not resolved.** When the chosen order and mtime order differ, the sheet shows one line under the list: *"These are in name order; their file dates suggest the reverse."* It changes nothing — the ↑ ↓ buttons do. It costs one comparison and one string, and it is the only cheap warning available for the one inference whose failure is invisible until the match clock is already wrong.

**I7. Which files are game videos is the coach's selection (E2).** The app applies no size floor, no duration check and no content sniff at pick time. The bus probes them during Create (**C2**), which is the app's existing and only definition of "can this be a source" (`bus/sources.rs:211-218`).

### W. Where `coach-cuts/` goes

**W1. Walk up from the match folder for an existing `coach-cuts/` directory, and use the first one found.**

From the match folder itself, then its parent, then its parent's parent, up to **four** levels, stopping at the filesystem root or at the coach's home directory (inclusive of home, never above it): test whether `<candidate>/coach-cuts` is a directory. The first hit is the projects folder.

This is the decisive rule and it answers the coach's actual question — *"note the pattern of having a 'coach-cuts' folder and then projects inside it"*. It finds the right answer in **both** of his trees, at depth 2 in each, despite the trees having different shapes [observed]:

| | match folder | depth 1 | depth 2 |
|---|---|---|---|
| Tree A | `<club>/<team>/game-videos/<match>` | `game-videos/` — no | `<team>/coach-cuts` — **hit** |
| Tree B | `<club>/<season>/<match>` | `<season>/` — no | `<club>/coach-cuts` — **hit** |

It also handles the coach reorganizing, and it needs no configuration, no `state.json` key and no memory of the last folder.

**W2. With no `coach-cuts/` anywhere above, propose two levels up from the videos.**

Default to `<match folder>/../../coach-cuts`, clamped so it is never at or above the home directory's parent and never above the mount point the videos are on; if the clamp bites, fall back to `<match folder>/../coach-cuts`.

Two levels, not one, because in **both** of the coach's layouts the folder directly above a match is a *grouping* folder shared by every match — a kind (`game-videos/`) in one, a season in the other [observed] — and a `coach-cuts/` inside it would scatter a club's projects across kinds and seasons, which is the opposite of the pattern the coach described. Two levels up is where he put it, both times.

This is the weakest rule in the document. It only ever fires on the **first** project of a brand-new tree; every project after that is decided by W1. It is a prefill in an editable field with a picker beside it (**W3**), and the sheet says so, with the hint *"No coach-cuts folder found nearby — Coach Cuts will create one here."* See **Q1**.

**W3. The field is an editable path with a Choose… button that opens the existing folder picker, prefilled.** `Pick::ProjectFolder` (`pickers.rs:17`) with its title changed to name what it is choosing. That is the "ask, with a picker prefilled" answer for every case W1 and W2 get wrong, and it is the same control the coach already knows.

**W4. The footage never moves.** Nothing is copied, nothing is renamed, nothing is hard-linked. The project stores `relative_path`, computed by the existing `relative_path` helper, which already produces a `../`-climbing path from a canonical file to a canonical project folder and is already tested for exactly that (`bus/sources.rs:246-258`, `:283-292`). Both of the coach's trees store `../../<group>/<match>/<file>` today [observed], which is what this flow will keep writing.

### N. Naming

**N1. The folder is `<YYYY-MM-DD>-<home slug>-<away slug>`**, home then away, in the slot order the sheet shows — so ⇄ Swap renames the folder, live, in the preview line.

**N2. One character rule, in core, shared with the export file names.**

`bus/export.rs:193-196` has the app's only file-name cleaning today: `part.replace(['/', ':'], "-")`, with the comment that `/` is the path separator and `:` is what *"a share to a Mac or a Windows machine trips over."* That reasoning is right and its character set is short by seven. The coach's projects live on a cloud-sync mount [observed], and exFAT, NTFS and SMB all reject `" * ? < > |` and `\` as well.

So core gains `naming::safe_file_name(&str) -> String`:

- replace each of `/ \ : * ? " < > |` and every control character with `-`;
- collapse runs of `-`;
- trim `-`, `.` and whitespace from both ends (which also disposes of `.` and `..`);
- truncate to 64 bytes **on a character boundary**;
- non-ASCII letters are **kept**. A coach's own language is not punctuation, every filesystem this app targets stores UTF-8 names, and mangling it would need a transliteration crate that `video-coach-core` will not be getting.

`bus/export.rs`'s `file_name` is changed to call it, so the app has one set of rules rather than two. The visible effect on export is that a project or tag name containing `?` or `|` now produces a file that writes on a shared drive instead of failing there.

**N3. The slug is `safe_file_name` plus lowercasing.** `naming::slug(&str)`: `safe_file_name`, then `to_lowercase()`, then whitespace runs to `-`, then collapse `-`, then trim `-`. `Rovers United` → `rovers-united`. An accented name lowercases per Unicode and keeps its letters.

**If a slug comes out empty** — a name that is entirely punctuation — the team's part is dropped from the folder name and the remaining parts are joined. A folder of the date alone is still a legal, distinct folder; it is not a good one, and the coach is looking at the preview line while it happens.

**N4. A name that is already taken gets `-2`, `-3`, … — first free wins.** `bus/export.rs:467-479`'s `de_duplicate` is the app's existing convention for this: start at 2, increment until free. **The rule is shared; the separator follows the alphabet of the thing being named** — `label (2)` for an export file, whose labels already contain spaces, and `slug-2` for a folder, whose whole point is that it has none. A double-header against the same opponent on the same day is the real case, and it wants a second folder rather than a refusal.

The **project name is not de-duplicated**: two projects may legitimately be called the same thing, `Project::name` is not a key, and nothing in the app looks a project up by it.

**N5. The project name is `<Home> v <Away>`, built by core's own `match_name`.** `metadata.rs:139-143` already derives exactly `format!("{home} v {away}")` from a project's scoreboard for the export title. It is private; it becomes `pub`, and the new project's name is built with it. A project created by this flow therefore has the name the exporter would have derived for it anyway, and there is one place that decides how a match is written down. Names keep the capitalization the coach typed; only the folder slug lowercases.

**N6. The date is not stored in `project.json`.** It names the folder and nothing else. A `Project::date` field would be a format change — `CURRENT_FORMAT_VERSION` 11 → 12 (`store.rs:21`) — for a value already visible in the folder name and already derivable at export time. `CURRENT_FORMAT_VERSION` does not move, `MIN_READABLE_FORMAT_VERSION` does not move, and this feature writes no field that did not exist in v11. See **Deferred 3** for the one thing that would like it.

### S. The sheet

**S1. One `Sheet`, at 560 px, titled "New match".** `Sheet` is the card every modal in this app is drawn as — width, chrome, heading, body (`app.slint:1386-1422`) — inside a `Scrim` (`app.slint:1377-1383`). The widest thing in this one is the folder path preview, not a table; 560 sits between the setup sheet's 520 and the editor's 640.

Like the setup sheet, it **wraps** the `Sheet` in a `Rectangle` rather than inheriting it, because the colour picker hangs over the card as an absolutely-positioned sibling and `@children` would put it in the body's layout — the exception already documented for `MatchSetupSheet` (`app.slint:1821`, spec P1).

Top to bottom:

| Field | Prefilled from | Control |
|---|---|---|
| **Date** | **I2** | `SetupField`, `YYYY-MM-DD`, with a provenance line when it came from mtime |
| **Home team** | **I5** | `TeamColumn`: name + primary + secondary, with swatches |
| **⇄** | — | a button between the columns; swaps the two `TeamColumn`s entire |
| **Away team** | **I5** | `TeamColumn` |
| **Periods / minutes** | **I5** | two `SetupField`s, the setup sheet's own validators |
| **Videos** | **I6** | an ordered list, one row per file: `<n>. <name>`, `↑ ↓ ×` |
| **Projects folder** | **W1** / **W2** | a path `SetupField` + `Choose…`, with a provenance line under W2 |
| **Folder preview** | derived | one read-only line: `<projects folder>/<slug>/` |
| **Project name** | derived, then editable | `SetupField`, seeded by **N5**, left alone once the coach types in it |

**S2. The team fields are the setup sheet's, extracted.** `SetupField` already is a caption, an `✕` mark, an optional swatch that opens the picker, and a two-way-bound `LineEdit` (`app.slint:1590-1648`); `ColorPicker` (`app.slint:1661`) is the popup it opens. A **`TeamColumn`** component is extracted from `MatchSetupSheet`'s two columns — a `FieldLabel`, a name `SetupField`, and one to three colour `SetupField`s, forwarding `pick(field, x, y)` up with its own base index — and **both** sheets use it. The setup sheet passes three colours, the new-match sheet two.

Each sheet keeps its own `picker-field` dispatch and its own popup placement, because *"Slint has no dynamic alias, so the open field is dispatched by hand"* (`app.slint:1884-1886`). That is about a dozen duplicated lines, and it is the cost of the language rather than a design choice; the fields, the swatch, the picker and the validators — the parts with rules in them — are shared.

**Two colours per team, not three.** `TeamConfig::new` already defaults the font colour to the secondary, mirroring the Swift initializer (`scoreboard.rs:38-47`), and the new sheet builds its teams with it. Overtime, the font colours and the back-anchor are not here: they are `MatchFormat::default()`'s zero, the secondary, and `false`, and `Setup…` edits all three the moment the project is open. See **Q2**.

**S3. ⇄ Swap exchanges the two columns whole** — names and both colours — and with them the folder preview and the project name. It is the answer to home/away being unknowable (**I5**), and watching the folder name flip is the fastest way to see which way round the coach wants it.

**S4. The video list reorders and drops; it does not add, and it does not probe.**

Rows are `<n>. <file name>` with `↑`, `↓` and `×`. No durations and no aspect: reading either means `probe`, which blocks for up to ten seconds per file (`probe.rs:15-16, 41`) and would do it on the UI thread, stalling the video behind the scrim. **The bus probes, once, during Create** (**C2**), which is also the only place the aspect gate can honestly run.

Dropping every row disables Create. There is no "Add files…": the picker that opened the sheet is the way in, and Cancel plus New match… again is the way to change the selection. A second file picker inside the sheet is a second pick path for a job the first one did.

**S5. Create is enabled only when everything it needs is good**, on the setup sheet's own model — *"a field can't read good and then fail to save"* (`main.rs:898-899`). It needs: a parseable date; two non-blank team names; four valid hex colours; valid periods and minutes (`match_panel::parse_periods` / `parse_minutes`, `match_panel.rs:391-407`); at least one video; a non-blank project name; a projects-folder path that is absolute; and a target folder that does not already exist. Each is marked in its own field by the same call that Create will make, so the button and the command never disagree.

**The target-folder check is a courtesy, not the guarantee.** It is a `try_exists` at prefill and on every edit of the name fields; the guarantee is the bus's `create_dir` (**C3**), which cannot race.

**S6. The key guard follows the setup sheet's branch.** `handle-key` guards the sheets in order (`app.slint:3040-3092`). This sheet has text fields, so it takes the setup sheet's shape exactly: an open colour picker takes the first Esc, then Esc closes the sheet, and everything else is `reject`ed so it reaches the focused field — which is also what delivers `Ctrl+V` into a path field. It goes **before** the setup sheet's branch, since only one sheet can be open at a time and the order is just a chain. Its `editing` folds into `text-editing` as the other sheets' do.

Unlike the match event editor, this sheet's fields **are** re-seeded from scratch every time it opens, so it needs none of the editor's Esc exceptions (spec P3).

### C. The command

**C1. One new command, `Command::NewMatch`, not a sequence of the existing four.**

```rust
/// Create `coach-cuts/<folder>/` under `projects_dir`, write a project into
/// it naming `videos` in the order given, and open it. All or nothing:
/// nothing is created until every video has been probed and accepted.
///
/// Every field is the sheet's, captured when Create was pressed. Nothing
/// here is read from the pipeline, so the caller-captured rule has nothing
/// to bite on.
NewMatch {
    projects_dir: PathBuf,
    folder: String,
    name: String,
    scoreboard: ScoreboardConfig,
    videos: Vec<PathBuf>,
},
```

Sending `OpenProject` + `AddSource` × n + `SetScoreboard` + `RenameProject` instead would be wrong in five separate ways, each of which is in the existing code:

- **`OpenProject` refuses a folder that does not exist** — *"A folder that doesn't exist is an error: saving never creates one"* (`bus/project.rs:39-44`). The UI would have to create the directory itself, putting filesystem writes on the UI thread and splitting the "who makes folders" rule in two.
- **The aspect gate fires between sources** (`bus/sources.rs:31`, `project.rs:422-444`). A second half whose shape differs is refused **after** the first half has been probed, pushed, saved and published — a project on disk holding one of a game's two halves. Tree B's camera even hands out files whose durations are identical to the microsecond [observed]; nothing about a second file is guaranteed by the first.
- **Each step saves and publishes on its own** (`bus/project.rs:187-190`). 4 + n writes of `project.json` and 4 + n `ProjectChanged` rebuilds, for one act.
- **`RenameProject` silently does nothing on a blank or unchanged name** (`bus/project.rs:79-89`), so the last step of the sequence has no failure the coach would see.
- **The half-built states are all reachable and all persistent.** The coach's own tree already contains a project folder with nothing in it [observed]. A half-built project is worse than none: it has a name, a `recordings/` folder and a `project.json` the coach will open next week expecting a match.

One command makes the whole thing one transaction, and it reuses every existing piece: `probed_source`, `check_aspect`, `store::write` and `commit`. No new machinery, no new undo action — **creating a project is not an undo step**, as opening one is not; `commit` clears the history (`bus/project.rs:104-106`).

**C2. The order of operations. Nothing touches the disk until every video has been accepted.**

1. **Sanity-check the inputs.** `projects_dir` absolute; `folder` and `name` non-blank after trimming; `videos` non-empty. A failure here is a UI bug (S5 gates all of it) and is reported as an error with nothing done.
2. **Probe every video, in order, and gate each against the ones before it.** `probe` then `check_aspect` (`bus/sources.rs:211-218`), accumulating `(path, Probe)`. The first failure aborts: `Event::Error` naming the file and the reason, **nothing created**.
3. **`create_dir_all(projects_dir)`**, which makes `coach-cuts/` and any missing parent. Already there is fine.
4. **`create_dir(project_dir)`** — the leaf, not `_all`. An existing leaf comes back `AlreadyExists` and is refused (**C3**). This, not the sheet's check, is the guarantee: it is one syscall with no window.
5. **Canonicalize the project folder**, then compute each `SourceRef`: `relative_path(canonical file, canonical folder)`, the display name, the duration and the aspect from the probe of step 2 — `probed_source`'s body with the probe already in hand (`bus/sources.rs:211-237`). Canonical on both sides because the kernel resolves `..` physically, and a cloud-sync mount may well be reached through a symlink (`bus/sources.rs:241-245`).
6. **Build the `Project`**: `Project::new(name)` (`project.rs:247-259`), `source_videos` from step 5, `scoreboard: Some(config)`. `format_version` is `CURRENT_FORMAT_VERSION` by construction.
7. **`store::write(&project_dir, &mut project)`** — creates `recordings/`, writes `project.json` through a temp file and a rename (`store.rs:175-205`).
8. **`commit(project_dir, project)`** — the existing function, unchanged (`bus/project.rs:93-126`): canonicalize, remember as last project, unload the previous one, clear history and trash, publish `ProjectOpened`, reset transcription, `ensure_loaded(0.0)`.

**C3. What each failure does.**

| Failure | What happens |
|---|---|
| A video can't be probed (missing, not video, rotated, needs a plugin — `probe.rs:29-37`) | `Event::Error(UserError::Source(_))` naming the file. **Nothing created.** The sheet stays open with its fields intact, so the coach drops that row and presses Create again. |
| A video's shape differs from the first (`UserError::AspectMismatch`, `bus/mod.rs:411-415`) | The same: named, nothing created, sheet open. |
| `create_dir_all(projects_dir)` fails — read-only disk, no permission, an unmounted path | `Event::Error(UserError::Io(_))` naming the path. Nothing created. |
| The project folder already exists | Refused by `create_dir`'s `AlreadyExists`, with a message naming it. The sheet's own check (**S5**) normally catches this first; this is the race and the hand-typed path. |
| `store::write` fails after the folder was made | `Event::Error`, **and the two directories are removed** — `remove_dir(project_dir/recordings)` then `remove_dir(project_dir)`, both non-recursive, both ignoring errors. `remove_dir` refuses a non-empty directory by definition, so this can never delete anything the coach put there, and it is what keeps a failed Create retryable rather than blocked by the empty folder it left. |
| A project was already open | Untouched until step 8. Every mutation saves as it happens (`bus/project.rs:187-190`), so there is nothing unsaved to lose, and a Create that fails at any step leaves the coach still in the project he was in. |

**C4. Errors here are modal, not notices.** `UserError::Io` and `UserError::Source` are not in `is_notice`, so they raise the error dialog, which is right: the coach pressed a button and is looking at a sheet waiting for an answer. Unlike the match event editor's refusals (spec C5), there is no live take to protect and no status line hidden behind the scrim — the dialog is drawn over everything.

**C5. The sheet closes on `ProjectOpened` and on nothing else.** Not on the click. The one event that says the project exists is the one that dismisses the sheet, so a failure leaves the sheet up with the coach's typing in it. Create is disabled between the click and the answer, so it cannot be pressed twice.

### X. What this does not do

- **It does not move, copy, rename or delete any footage.** The project points at the files where they are (**W4**).
- **It does not touch an existing project.** It refuses to create into an existing folder (**C3**) and never writes a `project.json` that is already there. The previously open project is saved-as-it-went and simply replaced in memory by `commit`.
- **It does not scan for videos.** The coach picks them (**E2**, **I7**).
- **It does not write outside the chosen projects folder** — one `coach-cuts/` and one match folder under it, and nothing else, ever.
- **It does not remember anything between runs.** No `state.json` key, no last-used projects folder: W1 re-derives it from the footage every time, which is right when a coach works on two clubs.
- **It does not change the project format.** No new field, no version bump (**N6**).

---

## Crate responsibilities

| Crate | Contents |
|---|---|
| `video-coach-core` | A new module `naming.rs`: `safe_file_name`, `slug`, `next_free(taken, base)` (the `-2`/`-3` rule). A new module `ordering.rs`: `order_videos`, plus `parse_date_in(&str) -> Option<CalendarDate>` and `opponent_from(&str) -> Option<String>` — all pure, all over `&str` and `&Path`, all tested with no filesystem. `metadata::match_name` becomes `pub`. **No new dependency:** the audit still lists exactly `serde`, `serde_json`, `thiserror`, `uuid` — no regex crate (the date shapes and the `(n)` suffix are a byte scan), no unicode crate (**N2**). |
| `video-coach-media` | **Nothing.** `Probe` gains no field; the container's creation time is not read (**I3**). |
| `video-coach-app` | `bus/mod.rs`: `Command::NewMatch`, off the allow-list. `bus/project.rs`: `new_match`, reusing `probed_source`'s body, `store::write` and `commit`. `bus/sources.rs`: `probed_source` split so the probe and the `SourceRef` construction can be called separately. `bus/export.rs`: `file_name` calls `naming::safe_file_name`. `pickers.rs`: `then` takes the whole `Vec` (**E4**). `main.rs`: the sheet's seeding — the `coach-cuts/` walk, the prior-project read, the inference, the validators, and `on_create_match`. UI: `TeamColumn` extracted from `MatchSetupSheet`, `EmptyCard`'s secondary action, `NewMatchSheet`, and its branch in `handle-key`. |
| `video-coach-harness` | The command end to end, and each of its refusals. |

**The filesystem parts stay in the app, the rules stay in core.** `order_videos` takes paths and, for its mtime fallback, a caller-supplied `&[Option<SystemTime>]` — core has no clock and no I/O, exactly as `CalendarDate` is passed in rather than derived (`metadata.rs:62-67`). The `coach-cuts/` walk and the prior-project read are `std::fs` and live in `main.rs` beside the other seeding.

## Testing

**No test writes outside a `tempfile::tempdir()`, and no test reads the coach's folders.** Every layout below is reconstructed structurally in a temp dir with empty files; the two real trees are described in this document and are not touched by anything that runs.

- **Core (`naming.rs`):**
  - `safe_file_name` over each forbidden character, a control character, a name that is entirely punctuation (→ empty), a name of leading and trailing dots, `.` and `..`, a 200-character name truncated **on a character boundary** with multi-byte characters straddling the cut, and a non-ASCII name kept intact.
  - `slug` lowercases, turns whitespace runs into one `-`, collapses and trims, and round-trips a plain two-word name.
  - `next_free` starts at 2, skips taken names, and matches `de_duplicate`'s sequence on the same input.
  - `file_name`'s output is unchanged for a label with no forbidden character (a regression pin on the export path).
- **Core (`ordering.rs`):**
  - `parse_date_in`: each accepted shape; a **bare four-digit year is not a date**; a **six-digit run is not a date**; month 13 and day 32 refused; a year outside the window refused; the leftmost of two dates wins; an eight-digit run inside a longer digit run refused.
  - `opponent_from`: a `YYYY-MM-DD-` prefix stripped; a digit-code prefix stripped; underscores and dots to spaces; title-cased; a folder name of digits alone yields `None`.
  - `order_videos`: a `P1`/`P2` pair; a `<stem>.mp4` + `<stem> (1).mp4` pair ordering **bare first** — the case a byte sort reverses, pinned explicitly; a `(2)`/`(4)` pair; a mixed-stem set falling to mtime; an empty mtime slice falling to the byte sort; and **stability**, the same input giving the same output.
  - `match_name` builds `"A v B"` and is `None` when either side is blank.
- **App (`main.rs` / a new `new_match.rs` helper, headless):**
  - the `coach-cuts/` walk over **both reconstructed layouts**, finding it at depth 2 in each;
  - the walk stopping at four levels, and at the home directory;
  - the W2 fallback in a tree with no `coach-cuts/`, and its clamp;
  - the club-from-neighbours count: three reconstructed `project.json`s where one name recurs, the recurring one seeded into the slot it last held; two projects with no recurrence; one project; none.
- **Harness (`tests/new_match.rs`), all inside a temp dir:**
  - `NewMatch` with two fixture videos creates `coach-cuts/<slug>/project.json` and `recordings/`, publishes **one** `ProjectOpened`, stores both sources in the order given with `../` relative paths, and the saved project reads back through `store::read` with the scoreboard and the name;
  - a second `NewMatch` at the same slug is **refused** and the first project's `project.json` is byte-identical afterwards;
  - a video that can't be probed: an error, **and no directory created** — asserted by listing the projects folder;
  - a second video with a different aspect: the same, and in particular **no folder holding one of the two**;
  - a read-only projects folder: an error, nothing created;
  - the previously open project is still open and unchanged after each refusal;
  - `NewMatch` while recording is dropped (the deny-by-default allow-list, `bus/mod.rs:807-835`).
- **Manual (batched, needs the user's eyes):**
  - run New match… against both real trees and confirm the projects folder, the date, the opponent and the video order are right before pressing Create;
  - confirm ⇄ Swap renames the folder preview and the project name live;
  - confirm the folder the coach's tree already has an empty one of can be created over — i.e. that an existing empty folder is refused with a message that says so, and that renaming it in the sheet works;
  - confirm the picker's multi-select still adds sources in the right order through the old `Add Source Video…` path (**E4**'s change);
  - confirm Esc closes the colour picker first and the sheet second.

## Risks

1. **W2 is a guess.** It fires once per tree, it is in an editable field with a picker, and it is right on both of the coach's layouts — but it is the one rule here derived from two samples. **Q1.**
2. **The order inference can be silently wrong.** I6's three layers cover both of the coach's camera systems, and the mtime disagreement line covers the case where they fight, but a coach who renames files by hand can still defeat all of it. The list with its ↑ ↓ is the real answer, and it is in front of him.
3. **The date is cosmetic here and load-bearing elsewhere.** It names the folder and nothing else (**N6**), so a wrong date costs a badly-named folder. The export's date tag is still mtime's (**Deferred 3**), so the two can now disagree — a coach could see a folder saying one day and a file tag saying another.
4. **`TeamColumn` is shared between two sheets with different colour counts.** The extraction is mechanical, but the setup sheet is shipped and working, and a regression there is a regression in something that already works.
5. **Probing several large files on a network mount blocks the bus for the length of the Create.** `probe` allows ten seconds each (`probe.rs:15-16`); the existing `AddSource` has exactly this property one file at a time. Create is disabled while it runs and the sheet is up, so the coach sees a modal rather than a frozen window — but there is no progress indication. **Q3.**

## Deferred

1. **Scanning a folder for candidate videos**, as an alternative to picking files. It needs a rule for which files are halves, which is the guess E2 exists to avoid. Revisit if the coach asks for it after living with the picker.
2. **Reading the container's creation time.** Useless on 100% of the coach's current footage (**I3**). Revisit only if footage from a phone or a camcorder turns up, and then as a `Probe` field rather than a second probe.
3. **Making the export's date tag agree with the match date.** `bus/export.rs:594` derives it from the first source's mtime, which the coach's own trees show is **one to two days** after the match [observed] — the doc comment's *"within a day"* is already optimistic. Once folders carry a real date, the cheapest fix is to parse it out of the project folder's name at export time: pure, no format change, and it reuses `ordering::parse_date_in`. **Revisit first.**
4. **Creating a whole season's projects at once** from a folder of match folders. The per-match sheet has to be right before a batch of them can be.
5. **Remembering the projects folder per footage tree.** W1 re-derives it in one `is_dir` per level; a `state.json` map would be a cache of something free.
6. **A "Duplicate this match's setup" action** for a second recording of the same game. `NewMatch`'s seeding (**I5**) already gets most of the way.
7. **Renaming a project's folder** to match a corrected name after the fact. It means moving a directory with a `project.json`, a `recordings/` tree and every source's `relative_path` pointing back out of it — a different feature with its own failure modes.

## Open questions for the user

Each has a default, and **the plan proceeds on it unless the user says otherwise.** (D-1 and D-2 are decided, not open.)

- **Q1. When there is no `coach-cuts/` anywhere above the videos, is two levels up right?** It is where both of the coach's trees put it, but it is inferred from two samples (**W2**).
  **Default:** two levels up, clamped, with the hint line and an editable field. The alternative — always asking on a fresh tree — costs a picker on the first project of a club and nothing after.
- **Q2. Two colour fields per team in the new-match sheet, or all three?** The setup sheet has primary, secondary and font; `TeamConfig::new` already defaults font to secondary (`scoreboard.rs:38-47`).
  **Default:** two. Six colour fields in a sheet that also holds a video list is a wall, and `Setup…` is one click away for the coach who wants the third.
- **Q3. Should Create show progress while it probes?** On a cloud-sync mount, two 1 GB files could take a few seconds each.
  **Default:** no — a disabled button and the modal sheet. Adding progress means a second event stream for an operation that is usually instant. Revisit if it is ever felt.
- **Q4. Should the date go in the project name as well as the folder?** The user's example is `<Home> v <Away>`, with no date.
  **Default:** no date in the name, as decided. The folder carries it, and the export title is derived from the name (`metadata.rs:139-143`).
- **Q5. Should `New match…` be the primary action of the no-project card, demoting `Open Project…` to a secondary button?** It changes the first thing a new coach sees.
  **Default:** yes (**E1**). Creating is the common case; opening an existing project is what the toolbar and `Ctrl+O` are for (`app.slint:3102-3108`).
- **Q6. Should the flow offer to reuse the teams of the *last opened* project when the chosen `coach-cuts/` folder is empty** — i.e. across trees, not only within one?
  **Default:** no. A coach who works on two clubs would get the other club's kit, and the opponent field is the one that matters.
