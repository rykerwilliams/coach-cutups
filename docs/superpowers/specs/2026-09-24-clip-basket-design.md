# The basket: one cut whose pieces come from different matches

**Date:** 2026-09-24
**Status:** Draft, not yet reviewed. It follows the coach's decisions of 2026-09-24, recorded in `BACKLOG.md` #86, and does not reopen them: pieces are gathered **while working**, project by project (*"so i would be in project a, do a corner kick clip and then enqueue it, then go to project 2"*); Start produces **one video of all the pieces, in the order they were added**; and **each piece carries its own match's scoreboard and clock**.
**Builds on:** Phase 5 (the export run — `docs/superpowers/specs/2026-09-19-linux-port-phase-5-design.md`), Phase 8 (the composite export: overlay, PiP, audio mix, chapters, tags, sidecars), Phase 9 (the match clock as the displayed frame's source time — `docs/superpowers/specs/2026-09-20-linux-port-phase-9-design.md`), the match event editor spec (2026-09-23) for the `Sheet` shape and the sheet key guards, and `BACKLOG.md` #77 (the export queue) and #86 (this).
**Evidence:** the code as it stands on `claude/intelligent-lamport-m2indd`. Every claim carries a `file:line`. Nothing here is measured.

Labels, as in the match event editor spec: **[cited]** points at a file in this repository or at a decision recorded in the backlog. There is no **[measured]** claim in this document; **[unmeasured]** marks the two places where a number is a guess.

The coach's clubs, opponents and players are not named anywhere below. `Rovers` / `Athletic` are the placeholders the rest of the codebase uses.

---

## Goal

The coach wants *"clips of the 'same thing' across different game projects"* — every corner of the season, one player's goals across three matches, every time a press worked. Today a clip belongs to a project and an export is built from exactly one project: `compilation_plan(project, target)` takes one `&Project` (`crates/video-coach-core/src/plan.rs:193`), and `Bus::start_run` refuses outright without one (`crates/video-coach-app/src/bus/export.rs:323-325`). The only answer available is "export three reels and join them in another program".

The **basket** is a list of clips the app holds *across* projects. The coach makes a clip in the project they are in, adds it to the basket, moves to the next project, and at the end presses Start once: one MP4 of every piece, in the order added, each piece drawing the board and clock of the match it came from.

**The basket is not a library.** It holds what the coach put in it as they worked. Going *looking* for pieces made months ago needs a view over other projects' clips, which is #86's second half and is **out of scope** here (see **Deferred**).

## Scope

In scope: the basket itself (add, see, reorder, remove), its persistence, one export job built from several projects, and the type changes in `core` and `media` that a cross-match job needs.

Out of scope, and unchanged: the export sheet and its targets (`export_targets`, `bus/export.rs:143-188`), the preview, the reel, the whole-match copy, transcription, the match editor, and every existing `ExportTarget`. A basket piece is always a **clip** — not a tag, not a reel, not a whole match (**E4**).

---

## How this differs from the export queue (#77), and what they share

The two are constantly confused because they come from the same sentence of the coach's (*"i open project 1, do stuff, enqueue. then project 2, do stuff, enqueue, then start the queue and walk away"*) and they share machinery. They are different features:

| | **#77, the export queue** | **#86, the basket** (this spec) |
|---|---|---|
| What is queued | whole **export jobs** — "All clips of match A at 1080p" | **pieces** — one clip |
| What Start produces | **several files**, one per queued job | **one file**, all the pieces spliced |
| Scoreboard | each file is one match's, as today | **per piece**, its own match's (**J3**) |
| When the work is decided | at enqueue: *"build the jobs now, run them later"* (#77's agreed shape) | at Start: the pieces are references (**E1**) |
| Run shape | N targets in one `ExportRun`, as a multi-tick run already is | **one** target in one `ExportRun` |
| Why you want it | don't sit through three exports | one film of the same thing across matches |

**What they share** is the run: `Active`, `ExportRun`, `TargetState`, the frames-not-percent progress, the rate window that carries across targets, and the one cancel (`bus/export.rs:55-116`, `:199-292`, `:409-443`). Both need `start_run` split so a run can be started from jobs the caller built rather than from `(targets, pickers)` against the open project (**C3**) — that split is the one piece of work the two features should share, and whichever lands first should do it.

**They compose.** Once both exist, a basket is one more job that can be queued behind three whole-match exports. Nothing here forecloses that; nothing here requires it.

---

## Decisions

### E. What an entry in the basket is

**E1. An entry is a reference — `(project folder, clip id)` — resolved at Start, not a snapshot captured at Add.**

The alternative is honest and was weighed: a snapshot cannot go stale. But:

- **A snapshot silently ships a stale edit.** The coach's own flow is to make the clip and add it *immediately* — which is exactly when the edit is least finished. They then watch it back, fix a stroke, trim a pause, rename it, tag the match's second half. A snapshot ignores every one of those, and nothing on screen says so. A reference's failure mode is the opposite: it is **detectable and nameable**, and the export's existing rule is already built for it — refuse before writing a byte, name the piece (`bus/export.rs:646-674`).
- **The codebase's own taste prefers a loud refusal to a quiet wrong answer.** The match editor refuses an out-of-range time rather than clamping it, because *"a clamped goal is a wrong timestamp that looks right"*. A snapshot of a clip the coach has since fixed is the same class of thing.
- **A snapshot is a copy of project data living outside every project.** To survive a restart (**H1**) it would need a serialized form of `Clip` (its whole event log), the recording path, the source paths, the `ScoreboardConfig`, the absolute match events, the highlights and the avatar — a second format for project data, with its own version discipline, in `state.json`. That is a large thing to invent for a feature whose refusals are cheap.
- **Undo would disagree with it.** `Ctrl+Z` after deleting a clip restores it from `.trash` (`crates/video-coach-app/src/bus/clips.rs:220-247`); a snapshot would hold a copy that undo neither sees nor can correct.

**What the reference costs, stated plainly:** the clip can be deleted (refused by name, **V1**); the footage can be moved or the project folder renamed (refused, **V1**, **V3**); the match can be re-tagged, which changes the clock burned into that piece. The last one is not a cost — it is the point. A coach who fixes a mis-tagged kick-off wants the fix in the film.

**E2. `(folder, clip id)` is the key, and the folder is canonical.** Clip ids are `Uuid::new_v4()` and unique *within* a project by construction, not across projects, so the pair is the key. `Open::folder` is already absolute and canonical (`crates/video-coach-app/src/bus/mod.rs:544-549`; canonicalized in `commit`, `bus/project.rs:93-97`), so two references to the same project compare equal whatever path the coach opened it by.

**E3. Nothing is cached for display.** A row's labels — the match, the clip's name, its duration — are read from the project each time the basket is shown (**U3**), never stored beside the reference. A cached name goes stale the moment the coach renames a clip, and a stale name in a list whose whole job is "which piece is this" is worse than a file read. For the **open** project the in-memory `Open::project` is used rather than the file: a failed save leaves memory ahead of disk and says so (`bus/project.rs:185-205`).

**E4. A piece is one clip.** Not a tag (which would be "all of project A's corners", a set that changes under the basket), not a reel, not a whole match. One clip is what the coach described adding, it is the only thing that has a stable id to point at, and it is the only thing whose entry the plan already knows how to build (`plan.rs:216-236`). See **Deferred**.

### H. Where the basket lives

**H1. In `state.json`, machine-wide.** `$XDG_CONFIG_HOME/coach-cuts/state.json` already holds the last project, the speech model, the pen and the window size, and its module comment states the rule this follows: *"**None is a project's.** … `Preferences` lives in `project.json`, where a new field is a format change that `store::read`'s exact-version guard would make every existing project unreadable for"* (`crates/video-coach-app/src/bus/state.rs:1-12`). Every field is `#[serde(default)]` (`state.rs:26-44`), so a new one costs no migration and an older build reads the file fine.

**`project.json` would be wrong by construction.** The basket belongs to no project: half its pieces are in projects that are closed. Putting it in one project's `Preferences` would also mean a `CURRENT_FORMAT_VERSION` bump (11 today, `crates/video-coach-core/src/store.rs:21`) for data that project has no business holding.

**Why persist at all, rather than hold it in memory for the session.** The coach's flow crosses projects, and plausibly crosses an evening — three matches is three opens, and an app restart (or a crash) in the middle would lose a list the coach cannot see the ingredients of any more. Persisting costs nothing in correctness *because* the entries are references: a stale reference in `state.json` behaves exactly as a stale reference in memory does — greyed out in the sheet, refused by name at Start. Losing the file costs a re-gather, which is the same as every other thing in there (`state.rs:11-12`).

**H2. One object, not five fields.** The whole basket is one value in `state.json`:

```json
"basket": {
  "name": "Corners",
  "outputDir": "/home/coach/Videos/Coach Cuts",
  "resolution": "r1080",
  "quality": "medium",
  "pieces": [ { "folder": "/home/coach/matches/20260917", "clip": "5f2c…" } ]
}
```

`name`, `outputDir`, `resolution` and `quality` are the basket's, not a project's (**O1**, **O2**): the film is not any one match's, so neither are the choices about it. `resolution` and `quality` reuse the existing `Resolution` / `Quality` serde (`crates/video-coach-core/src/project.rs`, already `Serialize`/`Deserialize` for `Preferences`).

**H3. The bus owns it, outside `Open`.** A new `crates/video-coach-app/src/bus/basket.rs`, with the list on `Bus` itself, loaded at `Bus::spawn` from `StateFile` and written back on every change (the bus is already the one writer of `state.json` — `Command::SetPen`'s doc says so, `bus/mod.rs:257-260`). It must not live on `Open`, which is cleared and replaced on every open (`bus/project.rs:93-126`); the basket is precisely the thing that has to survive that.

### J. How one job is built from several matches — the crux

An `ExportJob` is **nearly** self-contained already, which is what makes this feature small. It carries the whole compilation (every output frame and the plan), the output path, the cues, the renderer and the file tags (`crates/video-coach-media/src/composite/export.rs:120-147`); each entry carries its recording and a **clone of its `Clip`** (`EntryMedia`, `:151-158`); the job is explicitly documented as *"A snapshot: later edits to the project don't reach a running export"* (`bus/export.rs:618-621`). Nothing in the render path reads a `Project`.

**Five things in it are one-per-job where a cross-match cut needs one-per-match** [cited]:

| What | Where | Why it breaks |
|---|---|---|
| `ExportJob::sources: Vec<PathBuf>` | `export.rs:126-130`, read at `:561-566`, `composite/audio.rs:104-108`, `composite/copy.rs:232-244` | one flat list indexed by `PlanEntry::source_index`; two matches' indices collide |
| `Encode::scoreboard: Option<ScoreboardContext>` | `export.rs:105-109`, read at `:594-597` | one board and one match timeline for the whole film |
| `Encode::highlights: Vec<PlayerHighlight>` | `export.rs:110-113`, read at `:601-608` | keyed by `source_index` too (`crates/video-coach-core/src/highlight.rs:87-89`): match A's ring would land on match B's footage |
| `Encode::avatar: Option<PathBuf>` | `export.rs:114-117`, opened once at `:522-536` | one image per run |
| the audio volumes | `audio_regions(compilation, &prefs)`, `crates/video-coach-core/src/audio.rs:113-133`, called at `bus/export.rs:696` | `Preferences::preview_source_volume` / `preview_commentary_volume` are per project |

**J1. `PlanEntry` gains `match_index`, and the job carries one record per contributing match.** The entry's coordinate becomes `(match_index, source_index)`, and **`source_index` keeps exactly the meaning it is documented with** — *"Index into `Project::source_videos`"* (`plan.rs:76-78`). That is the whole trick: because `source_index` stays project-local, `ScoreboardContext::state_at(source_index, source_time)` needs **no change at all** (`crates/video-coach-core/src/scoreboard.rs:663-672`), and neither does `highlight_shapes`.

```rust
// core, plan.rs
pub struct PlanEntry {
    pub clip_id: Option<Uuid>,
    /// Which of the compilation's matches this entry's footage belongs to.
    /// Zero for every export built from one project, which is all of them but
    /// a basket; `source_index` indexes *that* match's videos.
    pub match_index: usize,
    pub source_index: usize,
    // … unchanged
}
```

Three construction sites set `match_index: 0` mechanically (`plan.rs:227`, `crates/video-coach-core/src/reel.rs:178`, `crates/video-coach-core/src/whole_match.rs:33`). The field is on `PlanEntry` rather than in a parallel vector in media because it qualifies `source_index`, which is core's; two parallel vectors that must agree in length and order is the thing this avoids.

**J2. `Render::Copy` takes its own file list, and the per-match media moves onto `Encode`.**

```rust
// media, composite/export.rs
pub enum Render {
    Encode(Encode),
    /// The stream copy: the files to join, in entry order.
    Copy(Vec<PathBuf>),
}

pub struct Encode {
    pub entries: Vec<Option<EntryMedia>>,   // unchanged
    pub audio: Vec<Region>,                 // unchanged
    pub resolution: Resolution,
    pub quality: Quality,
    /// One per contributing match, indexed by `PlanEntry::match_index`.
    pub matches: Vec<MatchMedia>,
}

/// Everything a piece needs from the match it came from, rather than from its
/// clip: which is to say, everything that is a property of a project.
pub struct MatchMedia {
    /// That match's game videos, indexed by `PlanEntry::source_index`.
    pub sources: Vec<PathBuf>,
    pub scoreboard: Option<ScoreboardContext>,
    pub highlights: Vec<PlayerHighlight>,
    pub avatar: Option<PathBuf>,
}
```

`ExportJob::sources` is **removed**. Its three readers become `encode.matches[entry.match_index].sources.get(entry.source_index)` (`export.rs:561-566`, `audio.rs:104-108`) and `Render::Copy`'s own list (`copy.rs:232-244`, whose `files()` disappears — the bus already builds exactly that list for `can_copy`, `bus/export.rs:563` and `:608-615`).

This also *repairs* `Render`'s stated invariant rather than bending it: *"Everything only one of them reads travels inside it, so a job can't carry a resolution for a run that copies or a cue list drawn by an encoder"* (`export.rs:69-81`). `sources` was the field that broke it, and a copy run carrying a `ScoreboardContext` it will never draw would have broken it further.

**J3. A piece from match A draws A's board and clock, exactly here:**

```rust
// composite/export.rs, inside the frame loop (today :594-597)
let m = &encode.matches[entry.match_index];
let scoreboard = m.scoreboard.as_ref().and_then(|context| {
    let state = context.state_at(entry.source_index, frame.source_time)?;
    Some((context.config(), state))
});
```

One line of indirection, and everything Phase 9 pinned still holds: the clock is **the displayed frame's source time**, asked per frame, never a per-clip constant plus record time (`plan.rs:97-113` and the loop's own comment, `export.rs:591-593`). Each match's `ScoreboardContext` is built by `ScoreboardContext::for_project` from *its own* project, so its `source_offsets` and its `AbsoluteMatchEvent`s are its own match's concat timeline (`scoreboard.rs:640-656`, `:742-751`) — the caching warning on both (*"never cache one across a source add, move, remove or relink"*) is satisfied by the existing rule: the contexts are derived when the run starts and the job is a snapshot. A piece from a match with no scoreboard configured draws no board, per entry, which is what `None` already means.

**The config travels with the state.** `frame.scoreboard` is `Option<(&ScoreboardConfig, ScoreboardState)>` already (`crates/video-coach-media/src/overlay.rs:354-356`), so the teams, colours and format redraw per entry with no change in the overlay: piece 1 is `Rovers 2 - 1 Athletic`, piece 2 is a different pair of names in different kit colours. The overlay's fitted labels are what make that safe — *"every label is fitted … nothing here clips and a centred line that overflows spills out of both ends of its cell"* (`overlay.rs:215-220`) — so a longer club name in the second match shrinks in its cell rather than spilling.

**J4. Per-match highlights and per-match avatar fall out of the same table.** `highlight_shapes(&m.highlights, entry.source_index, frame.source_time, …)`. The avatar becomes one `AvatarInset` per match that needs one, opened as the single one is today (`export.rs:522-536`) and cached by `match_index` beside the source decoders: a coach who records against an avatar in every project keeps their inset in every piece, and one who has an avatar in one project only gets the PiP filler in the others, which is already the "no usable inset" path (`export.rs:679-685`). **The filler must stay GL memory** (`export.rs:17-21`): an unfed pad stalls the run and a system-memory filler breaks `glupload` when a later entry has a real inset — a basket makes that alternation the normal case rather than the odd one.

**J5. The decoder cache keys on the path, not the index.** `HashMap<usize, Decoder>` becomes `HashMap<PathBuf, Decoder>` (`export.rs:539-542`). One decoder per distinct *file* for the whole run is the same rule it is now, it dedups a match walked by six pieces exactly as before, and it removes the need for a `(match_index, source_index)` tuple key. The `Diagnostics` line takes the first entry's decoder the same way (`export.rs:646-651`).

**J6. Volumes are per match: `audio_regions(compilation, prefs: &[&Preferences])`, indexed by `match_index`.** One line inside the loop (`audio.rs:113-133`); single-project callers pass a one-element slice. A coach who turned the game sound down in one project should hear that piece as they edited it — the volumes are part of how the clip sounds, and taking whichever project happened to be open would make the film depend on something invisible. Everything else about the mix is untouched: one audio-only pipeline per file, the first 1024 samples dropped for the encoder's priming, pushed at or ahead of the video into an unbounded `appsrc`.

**J7. Core builds the plan, as it does for every other target.** Two functions beside `compilation_plan` / `compilation_schedule`:

```rust
// core, plan.rs
/// One piece of a basket: which of `matches` it comes from, and the clip it
/// plays. The caller resolves the clip — and refuses what it can't find —
/// because it is the one that can name what is missing.
pub struct BasketPiece<'a> { pub match_index: usize, pub clip: &'a Clip }

pub fn basket_plan(matches: &[&Project], pieces: &[BasketPiece]) -> CompilationPlan;
// core, export.rs
pub fn basket_schedule(matches: &[&Project], pieces: &[BasketPiece]) -> Compilation;
```

Two shared helpers rather than two copies of the arithmetic:

- the per-clip entry (`playback_segments`, the source-duration fallback, `frame_count`'s per-entry quantization) becomes one function called by `compilation_plan`'s loop and `basket_plan`'s (`plan.rs:216-236`);
- the frame walker becomes `fn schedule(plan, events_for: impl Fn(&PlanEntry) -> &[CommentaryEvent]) -> Compilation`, so `compilation_schedule` finds a clip in its one project and `basket_schedule` finds it in `matches[entry.match_index]` (`crates/video-coach-core/src/export.rs:125-140`). Everything downstream of the walker — `total_frames`, `start_frame` quantization, `record_time` — is untouched, and `CompilationPlan::total_frames` stays **the** denominator (`plan.rs:137-146`).

**J8. It is not a new `ExportTarget` and not a new `Render`.** `ExportTarget` is *"which clips an export covers"* **of one project** — every variant is resolved against a single `&Project` (`plan.rs:14-39`, `:193-210`), and `export_targets`, `label`, `file_tags` and `default_scoreboard_mode` all match on it exhaustively against one project (`bus/export.rs:143-188`, `:447-461`, `metadata.rs:152-166`). A `Basket` variant would carry data none of those five can read and would force a `todo!()`-shaped arm into each. It is not a new `Render` either: it renders through `Render::Encode` like every other clip compilation. **It is a job built by a different builder and run through the same run machinery** (**C3**).

### O. What Start produces

**O1. One file, named by the basket, in a folder that is nobody's project.**

- **Name:** `<basket name>.mp4`, from the sheet's name field (default `Basket`), with `/` and `:` replaced as `file_name` already does — *"the path separator, and … which a share to a Mac or a Windows machine trips over"* (`bus/export.rs:190-196`). No `<project>` suffix — `file_name` joins the label and the project name with a dash today — because there is no one project to name. `file_name(label, project)` gains a sibling, or takes an `Option<&str>` project; either is two lines.
- **Where:** **not** a project's `exports/`. A basket written into whichever project happened to be open would move depending on the order the coach worked in, and would sit in a folder whose `project.json` does not describe it. The output folder is the basket's own, remembered in `state.json` (**H2**), defaulting to **`<XDG Videos>/Coach Cuts/`** (`glib::user_special_dir`, and `$HOME/Videos/Coach Cuts` when there is no such directory). The sheet shows the path with a **Change…** button that opens the same folder picker Open Project… uses. Created on demand, after the refusals, exactly as `exports/` is (`bus/export.rs:349-354`).
- **A repeat Start overwrites the file at that path**, which is what every export already does (`.part` then rename, `export.rs:375-386`, `:403-404`). `de_duplicate` is about two *targets in one run* colliding (`bus/export.rs:463-480`) and a basket run has one target; inventing an on-disk `" (2)"` rule here would be a new behaviour the rest of the app doesn't have.

**O2. Resolution and quality are the basket's, not a project's.** The sheet has those two pickers and no Scoreboard picker (**O5**). They are remembered in `state.json` beside the pieces. The basket must **not** write into the open project's `Preferences` the way `start_run` does for a normal run (`bus/export.rs:356-365`): those fields are that project's memory of its own last export.

**O3. Chapters: one per piece, unchanged.** `entry_chapters` titles each chapter with the entry's text bar line and skips a plan with fewer than two entries (`plan.rs:148-158`) — for a basket that is exactly right: jump to each corner. The `chpl` box, the `.chapters.txt` beside the file and the YouTube rules `chapter_list` enforces (first line `0:00`, at least three, ten seconds apart — `crates/video-coach-core/src/chapters.rs:10-31`) all apply as they stand, including its removal of a stale list (`export.rs:465-499`). A basket of two 8-second pieces gets no pasteable list, for the same reason a two-clip compilation doesn't.

**O4. Tags: a sibling of `file_tags`, telling the truth about a film that spans matches.** `file_tags(project, target, date)` is per project (`metadata.rs:103-131`), and the module's own rule is the one to follow: *"**Where a tag can't be told the truth it is left out** rather than guessed"* (`metadata.rs:16-19`). So `metadata::basket_tags(name: &str, matches: &[&Project]) -> FileTags`:

- `title` — the basket's name.
- `description` — `"A Coach Cuts basket of 7 pieces from 3 matches."`
- `comment` — **empty**. `final_score` states one match's result (`metadata.rs:168-176`); a film of three matches has no result to state.
- `keywords` — every contributing match's team names, deduped with `team_keywords`' existing blank-and-duplicate rules (`metadata.rs:178-192`). This is the one tag that is *more* useful across matches: a library can group the film under all six clubs.
- `encoder` — `APP_NAME` and the version, unchanged.
- `date` — **`None`**. `source_date` is *"the footage's date, not the export's"* (`bus/export.rs:583-604`); a film spanning three match days has no footage date, and the earliest of them would be a guess.

`match_name` becomes `pub` (`metadata.rs:139-143`) since the text bar needs it too (**T2**).

**O5. The scoreboard is burned in, and there is no cue sidecar.** `carry_scoreboard` already settles this for everything but the whole match: a clip *"is drawn on, zoomed and captioned, so it re-encodes either way, and a subtitle line repeating its own text bar would be clutter"*, and its cue slot is `None` so that a `.srt` beside the output — the coach's own file — is neither written nor removed (`bus/export.rs:538-550`). A basket is clips. So: `cues: None`, `scoreboard` burned per entry (**J3**), and `scoreboard_cues` is untouched — which matters, because it reads one context per compilation (`crates/video-coach-core/src/cues.rs:50-60`) and would be the second thing needing a per-match rewrite if a basket ever offered a cue track. See **Deferred**.

**O6. Mixed resolutions, aspect ratios and frame rates need no rule, because the graph already handles them** [cited]. The mixer is pinned to 1920×1080@30 and every entry is letterboxed into it by `fit_rect`, recomputed when the entry changes, with the `appsrc`'s caps reset on the same frame — *"Caps are safe to set from the pushing thread: the change lands on exactly the frame pushed after it (measured)"* (`export.rs:572-587`, `:668-677`). Source frame rate never mattered: the pump answers *"last decoded frame with PTS ≤ `source_time`"*, and *"that one rule covers freezes, 25→30 fps duplication and 60→30 fps drops"* (`crates/video-coach-core/src/export.rs:4-7`). So a 4:3 phone clip between two 1440p pieces letterboxes; it is not refused. The project-level `AspectMismatch` refusal (`bus/mod.rs:411-415`) is about one project's sources sharing a chrome coordinate space and does not apply across matches.

### T. What the text bar and the chapter titles say

**T1. The bar is `"<n> / <total> | <match> | <clip name> | tags"`, empty parts dropped.** Today it is `"<n> / <total> | <name> | tags"` where `<total>` is the target's clip count (`plan.rs:90-95`, `:160-171`).

**T2. The match is named, because that is what changes between pieces.** Two adjacent corners from two different games look identical. The match label is `match_name(project)` — `"Rovers v Athletic"` from the scoreboard config — falling back to the project's own `name`, and to `UNTITLED` for a project with neither (`metadata.rs:133-143`, `:38`). Not the folder name: the coach's own folders are called things like `20260917-canfield` (#85), which names nothing a viewer knows.

**T3. The count stays, and it means this film.** The user's framing was that *"across matches, numbering means nothing"*. What means nothing is a *project's* clip count; `"3 / 7"` is a position in **this output**, which is the same thing it means today and is the only orientation a viewer of a 7-piece film has. It is also first, which matters: the bar is left-aligned and **ellipsized, never shrunk** (`min_font_size` equal to the style's size — *"it is one long sentence, and a sentence that shrank with its length would leave the bar's size dancing entry to entry"*, `overlay.rs:226-232`, `:341-350`). So the tail is what is lost on a long line, and the order `count | match | clip | tags` drops the tags first, which are also in the chapter title and the file's keywords. **A four-part bar is the one thing in this spec most likely to need the coach's eye** (see **Testing**).

**T4. Chapter titles are the same line, unchanged.** `entry_chapters` uses the entry's text (`plan.rs:148-158`), so `"3 / 7 | Rovers v Athletic | Corner, 2nd half"` is both the burned-in caption and the chapter, and `one_line`'s whitespace collapsing already protects the pasteable list from a clip name with a newline in it (`chapters.rs:47-60`).

### U. The UI

**U1. Added from the clip row's own menu: "Add to basket".** That menu already holds *Jump to clip start / Preview clip / Export video… / Delete clip* (`crates/video-coach-app/ui/app.slint:3505-3527`), and "Export video…" is its nearest neighbour in meaning. It acts on the row it was opened on, not on the selection, like every other item there. A basket item is added for the open project, so it needs no gate beyond the menu's own `enabled: !root.recording`.

**No key in v1.** `b` is free (`app.slint:3176-3264` binds `r`, space, `a`/`d`, `,`/`.`, `[`/`]`, `j`/`l`, `1`, `2`/`3`, plus `z`/`x`/`v` and the Ctrl set), so one can be added later at no cost. See **Open questions**.

**U2. The badge is the answer to "is a basket waiting".** The bottom bar's button reads **"Basket (4)…"** and sits beside **Export…** (`app.slint:4139-4152`). The count comes from the stored list, so it is right the moment the app starts and survives every project switch — which is the whole point: the coach is in project B and must be able to see that four pieces are waiting. It reads **"Basket…"** and is disabled when the basket is empty. Unlike Export… it does **not** require an open project: a basket can be started with none, and it stays enabled while a run is going so the run can be watched and cancelled (the same reasoning as Export…'s comment, `app.slint:4141-4143`).

**U3. One `Sheet`, not a fourth panel shape.** `BasketSheet inherits Sheet` at **520 px** — the widest thing in it is a row's `"Rovers v Athletic — Corner, 2nd half"`, so it wants the setup sheet's width rather than the export sheet's 480 (`app.slint:1385-1422`, `:1454`). Wrapped in `Scrim` like the other four (`app.slint:1374-1383`). Top to bottom:

1. the **piece list** — one row per entry, in order: `↑` `↓` to move, the match, the clip's name, its duration, and `✕` to remove. A row whose reference is dead is drawn in `alternate-foreground` with why (`the project is gone`, `the clip was deleted`, `the game video is missing`) and is not exportable (**V1**). A `ListView` with a height of its own in the house idiom (`height: min(280px, …)`, as `app.slint:1460-1461` does) so a 20-piece basket scrolls rather than growing the sheet past the window.
2. the **name** field, the **output folder** with **Change…**, and the **Resolution** and **Quality** pickers — the export sheet's own two, verbatim.
3. the **run row**: the one target's progress and "Finishes at …", exactly as `ExportSheet` renders a run from the bus's events rather than from its own state (`app.slint:1424-1435`).
4. one **message line** for refusals, then **Clear**, **Start** and **Close**.

**U4. The sheet takes the export sheet's key guard, plus the setup sheet's `reject`.** The guards are ordered in `handle-key` (`app.slint:3044-3110`). The basket sheet has text fields (the name), so it takes the setup sheet's branch — Esc closes it, everything else goes to whatever has focus — and its `editing` folds into `text-editing` as `Inspector`'s and the match editor's do. Esc closing from inside the name field is acceptable here and not in the match editor, for the reason the editor's own comment gives: the editor's paste box holds a half-typed block, and a name field holds a word.

**U5. The sheet is modal, so nothing changes under it.** That is what lets the rows be resolved **once**, when the sheet opens (**E3**): the coach cannot rename a clip, delete one or open another project while the scrim is up. Start resolves again from scratch anyway, because a refusal has to be current (**V1**).

**U6. Reordering is `↑`/`↓`, not drag.** The clip list's drag-reorder exists and carries real machinery (`DragArea` / `DropArea` and `dropped-index`, `app.slint:3478-3491`); a 5-row modal list does not earn a second copy of it. Two buttons per row, disabled at the ends.

**U7. Clear empties the basket, and there is no undo for it.** Removing eight rows one at a time at the end of a session is worse than the risk, and what is lost is a list of *references* — every clip is still in its project — so the cost of a mis-click is a re-gather, not data. Stated plainly in the sheet's tooltip. See **Open questions** for whether it should confirm.

**U8. Start leaves the basket alone.** It is not emptied on success: the coach may re-run at another resolution, or add a ninth piece and run again. Clear is how a basket ends.

### V. What is refused, and when

**V1. Everything is refused before a byte is written, naming the piece.** The rule is the export's already: *"Every target is checked before any of them runs, so a missing file can't stop a run half-way through"* (`bus/export.rs:330-331`), and media only warns and degrades, so this check is *"what makes the loss visible at all"* (`bus/export.rs:9-13`). A basket resolves every piece at Start, and the **first** failure refuses the whole run as `UserError::CantExport` (`bus/mod.rs:465-467`) — all-or-nothing, not best-effort: a film silently missing the piece the coach cared about is worse than no film.

Named refusals, in the order they can be hit:

| What | Message |
|---|---|
| the basket is empty | `can't export: the basket is empty` |
| the project folder is gone, or `project.json` is unreadable | `can't export: the project at <folder> can't be read: <why>` |
| the project's format is too new or too old | the existing `TooNewProject` / `LegacyProject` wording, naming the folder (`bus/mod.rs:425-431`) |
| the clip was deleted | `can't export: a piece's clip is gone (<match>)` — the same fact `label` reports as *"the clip is gone"* (`bus/export.rs:451-457`) |
| the piece's game video is missing | `can't export: <match> — <clip>'s game video is missing; relink it first` (`bus/export.rs:646-661`) |
| the commentary recording is missing | `can't export: <match> — <clip>'s commentary recording is missing` (`bus/export.rs:666-670`) |
| a run or a preview is going | the existing `an export is running` / `a preview is open; close it first` (`bus/export.rs:315-322`) |

**Every message names the match as well as the clip**, because two projects can hold clips with the same name and *"Corner's game video is missing"* would not say which one to go and fix.

**V2. The missing-file check is done per piece, not from `Bus::missing`.** `missing` is one flag per source of the **open** project, refreshed on open and on every source-list change (`bus/mod.rs:343-345`, used at `bus/export.rs:646`). It says nothing about a closed project, so the basket stats each piece's own source file (and its recording) at Start. Cheap — one `Path::exists` per piece — and it is the same question `job()` asks.

**V3. Footage that moved is refused, never guessed at.** Relink is a project-level command that needs the project open (`Command::RelinkSource`, `bus/mod.rs:87`), so the refusal has to point the coach at the project: *"relink it first"* is already the wording.

**V4. Nothing about mixed resolutions or frame rates is refused** — see **O6**.

**V5. Adding a clip already in the basket is a no-op with a notice.** `Transcribe`'s doc sets the precedent: *"Does nothing if it is queued or running already"* (`bus/mod.rs:296-303`). The notice says `already in the basket`.

### C. Commands, events and the bus contract

**C1. Six commands, in `bus/basket.rs`.**

```rust
/// Put the open project's clip in the basket, at the end (spec E1, U1).
AddToBasket { clip_id: Uuid },
RemoveFromBasket { index: usize },
/// `Vec::remove` + `Vec::insert`, as `MoveClip` is.
MoveBasketEntry { from: usize, to: usize },
ClearBasket,
/// Resolve every piece against its project and publish the rows (spec U5).
ShowBasket,
/// Render the basket as one file. `resolution`, `quality`, `name` and
/// `folder` are the sheet's, and become the basket's (spec H2, O2).
ExportBasket { name: String, folder: PathBuf, resolution: Resolution, quality: Quality },
```

**C2. None of them touches a project, and none is on the recording allow-list.** The allow-list is a `matches!` at the top of `Bus::command` — *"everything not listed is refused, so commands added later are too"* (`bus/mod.rs:803-840`). Nothing here belongs on it: a clip only exists once its recording has stopped, and `ExportBasket` waits exactly as `Command::Export` does (*"Recording and export never overlap (a user decision)"*, `bus/export.rs:25-27`). The UI greys the button while recording, as it does Export….

**C3. `start_run` splits, and both halves keep the one-run / one-progress / one-cancel shape.** Today `start_run(targets, pickers)` labels the targets, builds a job each, writes the pickers into the project's `Preferences` and starts the first (`bus/export.rs:313-387`). It becomes:

```rust
/// Begins a run over jobs the caller built: refuses a second run, a preview,
/// or an empty list; creates nothing until the refusals are past; then starts
/// the first and publishes the run.
fn begin(&mut self, jobs: Vec<(String, ExportJob)>, out_dir: &Path) -> Result<(), UserError>;
```

`export()` builds its jobs from the open project and writes the pickers back (which is its business and not `begin`'s); `export_basket()` builds one job from the pieces. `Active`, `ExportRun`, `TargetState`, `finish_target`, `next_job`, `export_message`, the rate window and `CancelExport` are **untouched** (`bus/export.rs:55-116`, `:199-292`, `:399-443`). A basket run is a one-row run, so its progress, its estimate, its `bus: exported …` log line and its cancel are the ones that already work. This is the split #77 needs too.

**C4. `Event::Basket(BasketView)` is the one thing the UI renders from**, following `Event::Export(ExportRun)`'s rule — *"The sheet renders the run it is handed, so it can't be left holding a state the bus has moved past"* (`bus/export.rs:15-18`).

```rust
pub struct BasketView {
    pub name: String,
    pub folder: PathBuf,
    pub resolution: Resolution,
    pub quality: Quality,
    pub pieces: Vec<BasketRow>,
}
pub struct BasketRow {
    /// `"Rovers v Athletic"`, or the project's name (spec T2).
    pub match_label: String,
    pub clip_label: String,
    /// The piece's output length, from its plan entry's frames.
    pub seconds: f64,
    /// Why this piece can't be exported, or empty (spec V1).
    pub problem: String,
}
```

Published at `Bus::spawn`, on every basket mutation, and on `ShowBasket`. **Not** on `ProjectChanged`: that fires on every clip edit, and re-reading three `project.json` files per keystroke to refresh labels nothing can see (the sheet is modal, **U5**) would be waste. The badge's count is `pieces.len()`.

**C5. The caller-captured-timestamp rule has nothing to bite on.** CLAUDE.md's bus contract is about commands that land in the commentary event log, whose timestamps would drift with queue delay. No basket command carries a position or a time; `ExportBasket` reads no pipeline. (The same argument the match editor spec makes for typed times, C2 there.)

**C6. Refusals are `UserError::CantExport`, which is a modal, not a notice** — as every export refusal is today (`bus/mod.rs:465-467`; `is_notice` at `:486-494` does not list it). The sheet's own message line also shows it, because the status bar's notice line renders *behind* the scrim (the match editor spec's C5: the notice line is `app.slint:4330`, the scrims are siblings drawn after it at `:4381`–`:4497`).

### F. Format

**F1. `project.json` does not change and `CURRENT_FORMAT_VERSION` stays 11.** Nothing about the basket is a project's (**H1**). No `Project` field, no `Preferences` field, no new `Clip` field.

**F2. `state.json` gains one `basket` object, all of it defaulted** (`bus/state.rs:26-44`). An older build reading it ignores the key; a build that writes the file drops keys it doesn't know, which the module already documents and accepts.

**F3. A dead reference is never pruned automatically.** A piece whose project is gone stays in the basket, greyed out with its reason, until the coach removes it. Silently dropping it would hide the one thing they need to know — that the film they asked for is missing a piece — and the folder might be an unmounted drive that is back tomorrow.

### N. What it must not break

- **The single-project export paths.** Every existing `ExportTarget` keeps its behaviour byte for byte: `match_index` is 0, `matches` has one element, and the only changed call sites are the three that read `job.sources` and the one that reads `encode.scoreboard`. The Phase 8 and Phase 9 media tests are the proof.
- **The Phase 9 clock rule.** `state_at(entry.source_index, frame.source_time)` per frame, per entry's own board. No per-clip constant, nothing cached on `PlanEntry` (`plan.rs:97-113`).
- **`core` declares no media dependency.** `basket_plan`, `basket_schedule`, `basket_tags` and the per-match volumes are pure; the `&Project`s they take are core's own type.
- **Every pad gets a buffer for every frame**, with a **GL** filler (`export.rs:17-21`, `:63`). A basket alternates fed and filler PiP pads between matches far more often than a single project does.
- **`plan.total_frames()` is the denominator**, never a duration sum (`plan.rs:137-146`).
- **The `.part` then rename**, so a refused or cancelled basket never touches a file already at its path (`export.rs:375-386`).
- **No test reaches the network, and none opens the real camera or mic.**

---

## Crate responsibilities

| Crate | What it gains | What it must not gain |
|---|---|---|
| `video-coach-core` | `PlanEntry::match_index`; `BasketPiece`; `basket_plan`; `basket_schedule`; the shared per-clip-entry helper and the shared frame walker; `audio_regions` taking per-match `Preferences`; `metadata::basket_tags`; `metadata::match_name` made public | any notion of "the basket" as state, any path to a *folder* it must read, any media dependency |
| `video-coach-media` | `Render::Copy(Vec<PathBuf>)`; `Encode::matches: Vec<MatchMedia>` replacing `scoreboard` / `highlights` / `avatar` and `ExportJob::sources`; per-match board, highlights and avatar in the frame loop; the decoder cache keyed by path; one `AvatarInset` per match | any knowledge of projects, folders or `state.json`; a second renderer |
| `video-coach-app` | `bus/basket.rs` (the list, `state.json` read/write, resolution, refusals, the job builder); six commands; `Event::Basket`; `start_run` split into `begin`; `BasketSheet` and the bottom-bar button | the basket on `Open`; a write into any project's `Preferences`; a second run loop |
| `video-coach-harness` | `tests/basket.rs` — two projects, pieces from both, over the bus | — |

---

## Testing

**`video-coach-core` (no GStreamer):**

1. `basket_plan` over two projects: entry order is the piece order; `match_index` and `source_index` are each piece's own; `start_frame`s are the running quantized sum; `total_frames` is the last entry's end.
2. A piece from match B keeps B's `source_index` even when A has more sources than B has — the regression that a flat merged source list would cause.
3. Each entry's text is `"n / total | match | clip | tags"`, with empty parts dropped, and a project with no scoreboard falling back to its `name`.
4. `chapters` is one per piece, at `start_frame / 30`, titled with that line — and empty for a one-piece basket.
5. `basket_schedule` gives each entry its **own clip's** events: a piece whose clip has a zoom event zooms, and its neighbour from another project does not.
6. `basket_tags`: no comment, no date, keywords from all matches deduped, title the basket's name, description counting pieces and matches.
7. `audio_regions` with two matches' volumes: each entry's gains are its own match's.
8. **The clock test, which is the point of the feature:** two projects whose kick-offs are tagged differently; `ScoreboardContext::for_project` each; assert that a frame of piece 1 and a frame of piece 2 at the same *output* time read their own match's clock and score. The existing pause test (`core`'s Phase 9 one) is the model, and it must keep passing.

**`video-coach-media` (GStreamer, llvmpipe on CI):**

9. An `ExportJob` with two `MatchMedia`, two fixture sources of **different resolutions**, and a board on each: the file is 1920×1080@30, the entry boundary re-letterboxes, and the burned board changes team names across it. Read back by decoding a frame either side of the boundary (the Phase 9 scoreboard-pixel tests are the model).
10. One match with an avatar and one without: the second piece gets the filler, the run completes, and no pad stalls.
11. `Render::Copy(files)` still joins a whole match — the refactor's regression test.
12. A missing file in `MatchMedia::sources` fails the run with the entry named, and leaves no `.part`.

**`video-coach-harness` (over the bus):**

13. Two projects in one temp dir, each with a fixture video and one clip. Open A, `AddToBasket`; open B, `AddToBasket`; `ExportBasket`. Assert: one `ExportRun` with one target, progress events, `TargetState::Done`, the file at the basket folder, its `.chapters.txt`, and **no `.srt`** (**O5**).
14. The basket survives a project open: `Event::Basket` after opening B still lists A's piece with A's match label.
15. The basket survives a **bus restart**: write `state.json`, spawn a fresh bus, and the pieces are there (the `restore_last_project` tests are the model).
16. Each refusal in **V1**'s table, each naming its piece, each leaving no file: delete the clip, delete the source, delete the recording, remove the project folder, empty basket, run while a run is going.
17. `MoveBasketEntry` and `RemoveFromBasket` reorder and shorten the run's entries in the expected order.
18. A basket run cancels like any other (`CancelExport` → `TargetState::Cancelled`, no file).
19. `AddToBasket` for a clip already in it changes nothing and emits the notice.

**What needs the coach's eyes** (batched with the other hands-on checks, per the port's working order):

- **The four-part text bar on real footage** (**T3**) — is `"3 / 7 | Rovers v Athletic | Corner, 2nd half | corners"` readable at 1080p, or does the ellipsis eat the clip name? This is the decision most likely to need changing, and changing it is one function in `plan.rs`.
- **The board changing between pieces** — whether two clubs' kit colours flipping mid-film reads as intentional or as a glitch.
- **The join between two matches recorded differently** (exposure, white balance, a different camera position). No cut, no fade — is that acceptable, or does a basket want a 6-frame dip to black between pieces? (See **Open questions**.)
- **The default output folder** — `<Videos>/Coach Cuts` is a guess about where the coach wants films that belong to no match. **[unmeasured]**

---

## Risks

1. **The `PlanEntry::match_index` + `MatchMedia` refactor touches shared types for a feature with one caller.** It is the honest shape (the per-project data really is per-project), it is type-checked end to end, and it repairs `Render`'s stated invariant on the way through — but it is the largest diff here and it lands in the middle of the export path. Mitigation: it is mechanical, the existing Phase 8/9 media tests cover the single-project case exactly, and nothing about the frame loop's logic changes.
2. **Per-match `AvatarInset`s are a new allocation pattern in the run.** One GL texture per contributing match instead of one per run. Small **[unmeasured]**, but it is a per-run GL resource and #55 already records an fd leak in export runs.
3. **A reference basket can be "all dead" after a folder move**, and the coach's only recourse is to remove the rows and re-gather. That is the accepted cost of **E1**, and **F3** keeps the rows visible so the recourse is obvious.
4. **Two projects pointing at the same game video** get one decoder (**J5**) and two boards. That is correct — the same footage tagged twice is two matches' worth of events — but it is an odd enough case to state.

---

## Deferred

1. **The library view over old clips** — #86's second half: a set of project folders the coach can *browse*, to find pieces made months ago. Needs a project set (a folder of folders? the recents list? a "season"?), a clip list per project, and a search over tags. Not in this spec, and the basket does not depend on it: this spec's basket is filled as you work.
2. **A tag, a reel or a whole match as a basket piece** (**E4**). `basket_plan` takes pieces, so a piece that expanded to several entries is a change to the builder and to nothing else. Revisit when the coach asks for "every corner of match A" as one item rather than five.
3. **A cue-track basket** (**O5**). Would need `scoreboard_cues` to take a per-match board (`cues.rs:50-60`). No reason to yet: a basket is clips, and clips burn the board in.
4. **A transition between pieces** — a dip to black, or a title card naming the match. The film is a hard cut today. Needs the coach's eye first (see **Testing**).
5. **Reordering by drag** (**U6**), and a keyboard shortcut for Add (**U1**).
6. **The basket as a saved, named, re-exportable compilation** ("my corners reel, kept"). That is a document, which means a file format and a home for it — a real feature, and a different one from a scratch list.
7. **Music under a basket** — #84's question, unchanged by this: a basket of goals is exactly the film people expect music under, and the licensing half is still the blocker.
8. **Queuing a basket behind other exports** — the composition of #77 and this. Free once both exist, given **C3**'s split.

---

## Open questions for the user

Each has a recommended default, which is what will be built if nothing is said.

1. **Where should the file go?** *Default: `<Videos>/Coach Cuts/`, remembered and changeable in the sheet (**O1**).* The alternative is a folder picker on every Start — explicit, but friction on the one flow the coach described as "walk away".
2. **Should Start empty the basket?** *Default: no (**U8**); Clear is the way out.* The opposite reading — "Start consumed it, the basket is for the next one" — is defensible, and is what a queue does.
3. **Should Clear confirm?** *Default: no.* It destroys references, not clips, and the app's other immediate action (Delete clip) has undo where this has none.
4. **Does the text bar keep `"n / total"`?** *Default: yes, first (**T3**).* Dropping it buys room for the match and the clip name on a narrow line.
5. **A key for Add?** *Default: none; `b` is free.* `b` on the selected clip would suit the coach's "make it, add it, move on" rhythm.
6. **Should a dead piece be skipped with a warning instead of refusing the run?** *Default: refuse, naming it (**V1**).* Best-effort would let a coach walk away and come back to a film quietly missing the piece they cared about.
7. **Should the basket's pieces be limited?** *Default: no limit.* Thirty pieces is a 20-minute film and the run machinery does not care; the sheet's list scrolls.
