# The basket: one cut whose pieces come from different matches

**Date:** 2026-09-24
**Status:** Reviewed (simplify + correctness applied). It follows the coach's decisions of 2026-09-24, recorded in `BACKLOG.md` #86, and does not reopen them: pieces are gathered **while working**, project by project (*"so i would be in project a, do a corner kick clip and then enqueue it, then go to project 2"*); Start produces **one video of all the pieces, in the order they were added**; and **each piece carries its own match's scoreboard and clock**.
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

In scope: the basket itself (add, see, reorder, remove), its persistence, one export job built from several projects, the additions to `core` that a cross-match plan needs (all of them additive — see **J1**), and the one type reshape in `media`.

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

**What they share** is the run: `Active`, `ExportRun`, `TargetState`, the frames-not-percent progress, the rate window that carries across targets, and the one cancel (`bus/export.rs:55-116`, `:199-292`, `:409-443`). Both need `start_run` split so a run can be started from jobs the caller built rather than from `(targets, pickers)` against the open project (**C3**) — that split is the one piece of work the two features should share, and **whichever lands first does it**. #86's backlog entry says "revisit after #77"; that ordering is void, and this feature is the smaller of the two (**C7**).

**They compose.** Once both exist, a basket is one more job that can be queued behind three whole-match exports. Nothing here forecloses that; nothing here requires it.

---

## Decisions

### E. What an entry in the basket is

**E1. An entry is a reference — `(project folder, clip id)` — resolved at Start, not a snapshot captured at Add.**

The alternative is honest and was weighed: a snapshot cannot go stale. But:

- **A snapshot silently ships a stale edit.** The coach's own flow is to make the clip and add it *immediately* — which is exactly when the edit is least finished. They then watch it back, fix a stroke, trim a pause, rename it, tag the match's second half. A snapshot ignores every one of those, and nothing on screen says so. A reference's failure mode is the opposite: it is **detectable and nameable**, and the export's existing rule is already built for it — refuse before writing a byte, name the piece (`bus/export.rs:646-674`).
- **The codebase's own taste prefers a loud refusal to a quiet wrong answer.** The match editor refuses an out-of-range time rather than clamping it, because *"a clamped goal is a wrong timestamp that looks right"*. A snapshot of a clip the coach has since fixed is the same class of thing.
- **A snapshot is a copy of project data living outside every project.** To survive a restart (**H1**) it would need a serialized form of `Clip` (its whole event log), the recording path, the source paths, the `ScoreboardConfig`, the absolute match events, the highlights and the avatar — a second format for project data, with its own version discipline, in the app's own config directory. That is a large thing to invent for a feature whose refusals are cheap.
- **Undo would disagree with it.** `Ctrl+Z` after deleting a clip restores it from `.trash` (`crates/video-coach-app/src/bus/clips.rs:220-247`); a snapshot would hold a copy that undo neither sees nor can correct.

**What the reference costs, stated plainly:** the clip can be deleted (refused by name, **V1**); the footage can be moved or the project folder renamed (refused, **V1**, **V3**); the match can be re-tagged, which changes the clock burned into that piece. The last one is not a cost — it is the point. A coach who fixes a mis-tagged kick-off wants the fix in the film.

**And one genuinely silent wrong render, which the argument above does not cover.** If the coach **relinks** a source to *different* footage — a re-download that starts at a different moment, the wrong half, a different camera's file — the reference resolves cleanly, the probe writes the new duration back, and the piece renders footage the coach did not mean, with no refusal anywhere. `Command::RelinkSource` exists precisely so a project can point at a moved file, and nothing compares the new file's content with the old. A snapshot would not save us either (it would ship the *old* path, which is gone), so this is not an argument between the two designs — but the claim "a reference's failure mode is detectable" is not universally true, and pretending otherwise would be the kind of overclaim this spec is supposed to avoid. Relinking to different footage already corrupts every ordinary export of that project the same way; a basket only widens the blast radius to a film the coach may not be looking at while it renders.

**E2. `(folder, clip id)` is the key, and the folder is canonical.** Clip ids are `Uuid::new_v4()` and unique *within* a project by construction, not across projects, so the pair is the key. `Open::folder` is already *"absolute and canonical, so source paths resolve against it directly"* (`crates/video-coach-app/src/bus/mod.rs:545-549`; canonicalized in `commit`, `bus/project.rs:93-97`), so two references to the same project compare equal whatever path the coach opened it by.

**E3. Nothing is cached for display.** A row's labels — the match, the clip's name, its length — are read from the project each time the basket is shown (**U3**), never stored beside the reference. A cached name goes stale the moment the coach renames a clip, and a stale name in a list whose whole job is "which piece is this" is worse than a file read.

**E3a. One authority per piece, and it is the same one for the sheet and for Start** — and one **read per match**, not per piece (`basket::distinct_matches`, shared by both callers since the 2026-09-25 review): a coach gathers several clips from the match they are in, so a sheet of twenty pieces from three matches opens on three `store::read`s. If a piece's folder is the open project's, it is resolved from the in-memory `Open::project`; otherwise from `store::read` of that folder. **Both `ShowBasket` and `ExportBasket` go through the one function that makes that choice**, because a failed save leaves memory ahead of disk and says so (`bus/project.rs:185-205`) — so a sheet reading memory and a Start reading the file would show a piece that Start then refuses as gone. One function, one answer:

```rust
/// The project a piece's folder names: the open one in memory, or the file.
/// Every basket refusal about a project comes from here (spec V1, V6).
fn project_for(&self, folder: &Path) -> Result<Cow<'_, Project>, UserError>;
```

**E4. A piece is one clip.** Not a tag (which would be "all of project A's corners", a set that changes under the basket), not a reel, not a whole match. One clip is what the coach described adding, it is the only thing that has a stable id to point at, and it is the only thing whose entry the plan already knows how to build (`plan.rs:216-236`). See **Deferred**.

### H. Where the basket lives

**H1. Machine-wide, beside `state.json` — but in its own file, `$XDG_CONFIG_HOME/coach-cuts/basket.json`.** (`AppFiles` was called `StateFile` while this was built; it was renamed in the same day's review, because it is now *where the app's own files are* — the state file, its siblings and the user's videos folder — rather than any one of them.) Not a *key in* `state.json`, which was the first draft and is wrong: `AppFiles::read` **discards the whole document on any parse error** (`state.rs:158-169`, returning `State::default()` with a log line) and **every setter rewrites it whole** (*"Every write reads first, so a field one setter doesn't know about survives the other's write: the document is rewritten whole"*, `state.rs:154-157`). So one unparseable basket value — a resolution label from a later build, a hand-edited path — would silently take the last project, the pen, the speech model and the window size down with it, and the next `set_pen` would write the loss to disk. That same hazard is *why* that file already stores the model and the pen as **string labels rather than enums** (`state.rs:34-42`: *"a label this version doesn't know reads as the default rather than throwing the whole document away"*).

A separate file makes the blast radius the basket's own, which is the only thing whose loss is a re-gather.

**`project.json` would be wrong by construction.** The basket belongs to no project: half its pieces are in projects that are closed. Putting it in one project's `Preferences` would also mean a `CURRENT_FORMAT_VERSION` bump (11 today, `crates/video-coach-core/src/store.rs:21`) for data that project has no business holding. Its sibling rule — *"**None is a project's.** … `Preferences` lives in `project.json`, where a new field is a format change that `store::read`'s exact-version guard would make every existing project unreadable for"* (`state.rs:1-12`) — is the one this follows.

**Why persist at all, rather than hold it in memory for the session.** The coach's flow crosses projects, and plausibly crosses an evening — three matches is three opens, and an app restart (or a crash) in the middle would lose a list the coach cannot see the ingredients of any more. Persisting costs nothing in correctness *because* the entries are references: a stale reference on disk behaves exactly as a stale reference in memory does — greyed out in the sheet, refused by name at Start.

**H2. One object, and the label discipline applies inside it too.**

```json
{
  "name": "Corners",
  "resolution": "r1080",
  "quality": "medium",
  "pieces": [ { "folder": "/home/coach/matches/20260917", "clip": "5f2c…" } ]
}
```

- **No `outputDir`.** The film goes to `<XDG Videos>/Coach Cuts/` and that is all (**O1**).
- **`resolution` and `quality` are read as labels, not as `Resolution` / `Quality` directly**, and a label this build doesn't know reads as the **default** rather than throwing the pieces away. That is `state.rs`'s discipline applied one level in: the pickers are worth nothing and the pieces are worth an evening, so the pickers must not be able to take them down. It costs two `label` / `from_label` pairs, private to `bus/basket.rs`; core keeps its serde as it is.
- **`pieces` is the document.** A malformed piece list is the one thing that does cost a re-gather, logged the way `AppFiles` logs its own failures (`state.rs:158-169`).
- Every field `#[serde(default)]`, so a file from before a field, or without one, reads.

**H3. The bus owns it, outside `Open`.** A new `crates/video-coach-app/src/bus/basket.rs` holding both the list and its file, with the list on `Bus` itself, loaded at `Bus::spawn` and written back on every change (the bus is already the one writer of the app's own state — `Command::SetPen`'s doc says so, `bus/mod.rs:257-260`). It must not live on `Open`, which is cleared and replaced on every open (`bus/project.rs:93-126`); the basket is precisely the thing that has to survive that. The file's location follows `AppFiles`'s: `AppFiles::in_config_dir`'s shape, so tests point `XDG_CONFIG_HOME` at a scratch directory exactly as they do today.

### J. How one job is built from several matches — the crux

An `ExportJob` is **nearly** self-contained already, which is what makes this feature small. It carries the whole compilation (every output frame and the plan), the output path, the cues, the renderer and the file tags (`crates/video-coach-media/src/composite/export.rs:120-147`); each entry carries its recording and a **clone of its `Clip`** (`EntryMedia`, `:151-158`); the job is explicitly documented as *"A snapshot: later edits to the project don't reach a running export"* (`bus/export.rs:618-621`). Nothing in the render path reads a `Project`.

**Five things in it are one-per-job where a cross-match cut needs one-per-entry** [cited]:

| What | Where | Why it breaks |
|---|---|---|
| `ExportJob::sources: Vec<PathBuf>` | `export.rs:126-130`, read at `:561-566`, `composite/audio.rs:104-108`, `composite/copy.rs:232-244` | one flat list indexed by `PlanEntry::source_index`; two matches' indices collide |
| `Encode::scoreboard: Option<ScoreboardContext>` | `export.rs:105-109`, read at `:594-597` | one board and one match timeline for the whole film |
| `Encode::highlights: Vec<PlayerHighlight>` | `export.rs:110-113`, read at `:601-608` | keyed by `source_index` too (`crates/video-coach-core/src/highlight.rs:85-89`): match A's ring would land on match B's footage |
| `Encode::avatar: Option<PathBuf>` | `export.rs:114-117`, opened once at `:522-532`, gated at `:533-536` | one image per run |
| the audio volumes | `audio_regions(compilation, &prefs)`, `crates/video-coach-core/src/audio.rs:113-133`, called at `bus/export.rs:696` | `Preferences::preview_source_volume` / `preview_commentary_volume` are per project |

The last of those is **not** fixed here — see **J6**.

**J1. The match's record hangs off the entry, and `core` is untouched.** An earlier draft added `PlanEntry::match_index` and a `Vec<MatchMedia>` on `Encode` indexed by it. That is a worse design than it looks: it puts a media-only coordinate into `core`'s pure plan type, forces a mechanical `match_index: 0` into **four** `PlanEntry` construction sites (`plan.rs:227`, `crates/video-coach-core/src/reel.rs:178`, `crates/video-coach-core/src/whole_match.rs:33`, `crates/video-coach-core/tests/audio.rs:125`), makes every ordinary export carry a one-element `Vec` and an index into it, and leaves an **unenforced invariant** — "`matches[entry.match_index]` is the project `entry.source_index` belongs to" — that nothing type-checks.

So the record goes on the entry's media instead, where media already keeps per-entry things:

```rust
// media, composite/export.rs
pub enum Render {
    Encode(Encode),
    /// The stream copy: the files to join, in entry order.
    Copy(Vec<PathBuf>),
}

pub struct Encode {
    /// One per `compilation.plan.entries`, in the same order. **No longer an
    /// `Option`**: every entry has a file and a match even when it has no clip.
    pub entries: Vec<EntryMedia>,
    pub audio: Vec<Region>,     // unchanged
    pub resolution: Resolution, // unchanged
    pub quality: Quality,       // unchanged
}

/// What one entry needs beside its `PlanEntry`, which carries the edit but
/// neither the files nor the drawings.
pub struct EntryMedia {
    /// The game video this entry's frames come from — **the file, not an
    /// index**: `PlanEntry::source_index` stays the project-local index the
    /// board and the highlights are keyed by, and nothing here has to map it.
    pub source: PathBuf,
    /// `None` exactly for an entry with no clip (`PlanEntry::clip_id`): the
    /// PiP filler, no drawings, no commentary.
    pub clip: Option<ClipMedia>,
    /// The match this entry's footage belongs to. One value per contributing
    /// project, **shared by `Arc`** between that project's entries: a
    /// twenty-piece basket from three matches holds three `ScoreboardContext`s,
    /// not twenty.
    pub match_media: Arc<MatchMedia>,
}

pub struct ClipMedia {
    /// The commentary recording, under its project's `recordings/`.
    pub recording: PathBuf,
    /// The clip, for the drawings the overlay replays.
    pub clip: Clip,
}

/// Everything a piece needs from the match it came from, rather than from its
/// clip: which is to say, everything that is a property of a project.
/// Built once per contributing project when the run starts, and — like every
/// `ScoreboardContext` — **never reused across a source add, move, remove or
/// relink**, which a job being a snapshot already guarantees.
pub struct MatchMedia {
    pub scoreboard: Option<ScoreboardContext>,
    pub highlights: Vec<PlayerHighlight>,
    pub avatar: Option<PathBuf>,
}
```

**What this buys:**

- **`core` does not change at all.** `PlanEntry`, `plan.rs`, `reel.rs`, `whole_match.rs` and `source_index`'s documented project-local meaning (*"Index into `Project::source_videos`"*, `plan.rs:76-78`) are all exactly as they are. `ScoreboardContext::state_at(source_index, source_time)` (`scoreboard.rs:669-672`) and `highlight_shapes` need no change, for the same reason the earlier draft claimed but now without paying for it.
- **The unenforced invariant is gone**: there is no index to get wrong, because the board that draws an entry is reached *through* that entry.
- **`ExportJob::sources` is removed entirely**, and it has exactly **three** readers: `export.rs:561-566` (the decoder cache) and `audio.rs:104-108` (the mixer's game track) both become `encode.entries[…].source`, and `copy.rs:232-244`'s `files()` disappears into `Render::Copy`'s own list.
- It *repairs* `Render`'s stated invariant rather than bending it: *"Everything only one of them reads travels inside it, so a job can't carry a resolution for a run that copies or a cue list drawn by an encoder"* (`export.rs:73-75`). `sources` was the field that broke it.
- **`copy_files` disappears rather than becoming fallible.** Today it is a `filter_map` (`bus/export.rs:608-615`) that would silently drop an entry with no game video and produce a **shorter film** where `copy.rs`'s own `files()` refuses — two readers of the same two fields disagreeing. With the reshape, the copy list is `entries.iter().map(|e| e.source.clone())` over the entry list the resolver already refused a missing source in (**C3a**), so there is nothing to drop and nothing to report. That is strictly better than making the `filter_map` return a `Result`.

**J2. The decoder cache keys on the path, and is bounded.** `HashMap<usize, Decoder>` becomes `HashMap<PathBuf, Decoder>` (`export.rs:542`): one decoder per distinct *file*, which dedups a match walked by six pieces exactly as the index key does today and needs no `(match, source)` tuple.

**But one live decoder per contributing file for the whole run is unbounded across matches.** Today a compilation walks one or two files of one project, so "alive for the whole compilation" costs one or two decode sessions. A thirty-piece basket from thirty projects would hold **thirty** decode pipelines, their threads and their fds at once — and #55 already records an fd leak in export runs. So apply the rule the audio mixer already applies to itself, for the same reason, written on it verbatim: *"Close a file nothing later reads … Kept open, a compilation of two hundred clips would hold two hundred pipelines at once, where the picture holds one recording at a time"* (`composite/audio.rs:144-158`). At each entry boundary the outgoing entry's source is dropped **unless some entry at or after the new one reads it** — a linear scan over the remaining entries, the same shape as the mixer's `active.chain(order[next..])`. A single-project compilation therefore keeps its one decoder from the first entry to the last, exactly as now.

**The one thing that must move with it:** `Rendered::diagnostics` reads *the first entry's* decoder **after the loop** (`export.rs:646-651`) — the zero-copy line CLAUDE.md calls the diagnostic. With bounded decoders that decoder may be long gone, and the run would report an empty line. So the diagnostics are taken **when the first entry's decoder is opened**, not at the end.

**J3. A piece from match A draws A's board and clock, exactly here:**

```rust
// composite/export.rs, inside the frame loop (today :594-597)
let media = &encode.entries[frame.entry];
let scoreboard = media.match_media.scoreboard.as_ref().and_then(|context| {
    let state = context.state_at(entry.source_index, frame.source_time)?;
    Some((context.config(), state))
});
```

One line of indirection, and everything Phase 9 pinned still holds: the clock is **the displayed frame's source time**, asked per frame, never a per-clip constant plus record time (`plan.rs:97-113` and the loop's own comment, `export.rs:591-593`). Each match's `ScoreboardContext` is built by `ScoreboardContext::for_project` from *its own* project, so its `source_offsets` and its `AbsoluteMatchEvent`s are its own match's concat timeline (`scoreboard.rs:640-656`, `:742-751`) — the caching warning on both (*"never cache one across a source add, move, remove or relink"*) is satisfied by the existing rule: the contexts are derived when the run starts and the job is a snapshot. A piece from a match with no scoreboard configured draws no board, per entry, which is what `None` already means.

**The config travels with the state.** `frame.scoreboard` is `Option<(&ScoreboardConfig, ScoreboardState)>` already (`crates/video-coach-media/src/overlay.rs:354-356`), so the teams, colours and format redraw per entry with no change in the overlay: piece 1 is `Rovers 2 - 1 Athletic`, piece 2 is a different pair of names in different kit colours. The overlay's fitted labels are what make that safe — *"every label is fitted … nothing here clips and a centred line that overflows spills out of both ends of its cell"* (`overlay.rs:215-220`) — so a longer club name in the second match shrinks in its cell rather than spilling.

**J4. Per-match highlights come through the same field.** `highlight_shapes(&media.match_media.highlights, entry.source_index, frame.source_time, …)` (`highlight.rs:424-431`).

**J5. The avatar is opened per distinct image, and gated per entry.**

Today there is one avatar for the run, opened once at `export.rs:522-532` **behind a per-job gate** — `.filter(|_| encode.entries.iter().flatten().any(|m| m.clip.shows_avatar()))` — and that gate also decides whether `pulse_levels` runs at all (`export.rs:533-536`). Per match, both become per entry, and the per-job gate disappears rather than growing a match dimension:

- **The images are known up front.** Every entry carries its match's `avatar` path, so the distinct set of images some entry actually asks for is one pass over `encode.entries` before the loop. Each is opened once into a `HashMap<PathBuf, AvatarInset>` — **keyed on the path, as the decoder cache keys on the source's** — so two projects sharing one image (this coach's own case: the same photo in every project) hold one texture and one GL upload, not one per match.
- **The per-entry condition is `media.clip.as_ref().is_some_and(|c| c.clip.shows_avatar()) && media.match_media.avatar.is_some()`.** That is the *whole* gate, and it is the same expression in both places: the pre-pass that opens the insets, and `pulse_levels`' per-entry filter (`export.rs:981-993`, today filtering on `shows_avatar()` alone). The old `match avatar { Some(_) => pulse_levels(…), None => vec![1.0; …] }` gate then earns nothing — with the avatar path in the condition, `pulse_levels` over a basket in which nobody asks for one decodes nothing and returns all ones by itself — so it is **deleted**. One expression, two readers, no per-job flag that a per-match world could make wrong.
- An entry whose match has no avatar, or whose image is gone, gets the PiP filler, which is already the "no usable inset" path (`export.rs:679-685`). **The filler must stay GL memory** (`export.rs:17-21`): an unfed pad stalls the run and a system-memory filler breaks `glupload` when a later entry has a real inset — a basket makes that alternation the normal case rather than the odd one.

**J6. There are no per-match audio volumes, and `audio_regions` keeps its signature.** An earlier draft made it `audio_regions(compilation, prefs: &[&Preferences])` indexed per entry, so each piece would be mixed at the volumes its own project remembered. It is a defensible idea and it is not worth it:

- It is the **one** item in this feature that forces a change on **eight** call sites of an otherwise untouched pure function (`bus/export.rs:696`, `core/tests/audio.rs:73` and `:151`, `media/tests/export.rs:349`, `:456`, `:1162`, `:1316`, `:1517`), every one of them a single-project caller that would pass a one-element slice as ceremony.
- A film whose sound level jumps between pieces for a reason nothing on screen or in the sheet explains is **worse than one that doesn't**. `preview_source_volume` is a *scanning* convenience — how loud the coach wants the crowd while they work — and treating it as an authored mix across matches reads as a fault, not as intent.

**So the basket mixes at `Preferences::default()`: both volumes 1.0** (`project.rs:95-108`). Everything else about the mix is untouched: one audio-only pipeline per file, the first 1024 samples dropped for the encoder's priming, pushed at or ahead of the video into an unbounded `appsrc`. Per-piece gain, if the coach ever wants it, is a property of the *piece* and belongs on the basket row — see **Deferred**.

**J7. Core builds the plan, as it does for every other target — and the pieces carry what they need, so core needs no `Project` list.**

```rust
// core, plan.rs
/// One piece of a basket: the clip it plays, the duration it is planned
/// against, and what its match is called. **The caller resolves all three**
/// — and refuses what it can't find — because it is the one that can name
/// what is missing.
pub struct BasketPiece<'a> {
    pub clip: &'a Clip,
    /// From [`clip_source_duration`] against the clip's own project.
    pub source_duration: f64,
    /// [`crate::metadata::match_name`], or the project's name (spec T2).
    pub match_label: String,
}

/// The duration `compilation_plan` plans `clip` against: its source's, or the
/// documented fallback for a source that isn't there. **The** duration
/// authority, in one place, for both builders.
pub fn clip_source_duration(project: &Project, clip: &Clip) -> f64;

pub fn basket_plan(pieces: &[BasketPiece]) -> CompilationPlan;
// core, export.rs
pub fn basket_schedule(pieces: &[BasketPiece]) -> Compilation;
```

**One extracted helper, and no generification of `compilation_schedule`.** An earlier draft turned the walker into `fn schedule(plan, events_for: impl Fn(&PlanEntry) -> &[CommentaryEvent])`. That is unnecessary — `walk` (`crates/video-coach-core/src/export.rs:143-152`) is *already* the shared unit, and both schedulers are in the same module, so each writes its own four-line loop around it. More importantly, a basket's entries are **1:1 with its pieces, in order**, so `basket_schedule` reads `pieces[i].clip.events` **directly**, where `compilation_schedule` looks its clip up by `clip_id` (`export.rs:130-137`). That closes the invariant the `match_index` draft left unenforced — "find the clip in `matches[entry.match_index]`", which on a miss **degrades silently to identity zoom** via `map_or(&[][..], …)` — rather than restating it in a closure.

So the only thing extracted is the per-clip entry itself: `playback_segments`, the quantized `frame_count`, and the fields around them (`plan.rs:216-236`) become one private function that `compilation_plan`'s loop and `basket_plan`'s both call, with the text bar's line passed in (**T1**). Everything downstream — `total_frames`, `start_frame` quantization, `record_time`, `entry_chapters` — is untouched, and `CompilationPlan::total_frames` stays **the** denominator (`plan.rs:137-146`).

**J8. It is not a new `ExportTarget` and not a new `Render`.** `ExportTarget` is *"which clips an export covers"* **of one project** — every variant is resolved against a single `&Project` (`plan.rs:14-39`, `:193-210`), and `export_targets`, `label`, `file_tags` and `default_scoreboard_mode` all match on it exhaustively against one project (`bus/export.rs:143-188`, `:447-461`, `metadata.rs:152-166`). A `Basket` variant would carry data none of those five can read and would force a `todo!()`-shaped arm into each. It is not a new `Render` either: it renders through `Render::Encode` like every other clip compilation. **It is a job built by a different builder and run through the same run machinery** (**C3**).

### O. What Start produces

**O1. One file, named by the basket, in a folder that is nobody's project.**

- **Where: `<XDG Videos>/Coach Cuts/`, full stop.** `glib::user_special_dir(UserDirectory::Videos)` (glib 0.22, returns `Option<PathBuf>`), falling back to `$HOME/Videos/Coach Cuts` and then to the current directory when there is no home at all. Created on demand, after the refusals, exactly as `exports/` is (`bus/export.rs:349-354`).

  **Not** a project's `exports/`: a basket written into whichever project happened to be open would move depending on the order the coach worked in, and would sit in a folder whose `project.json` does not describe it.

  **And no remembered folder, no `outputDir` key and no Change… button.** An earlier draft had all three. They are an **export setting**, and BACKLOG #78 is exactly the item that says export settings should *"arrive with its home already decided rather than as six checkboxes"* — its own list of what would become a setting (the `.srt`, the chapter track, the file tags) is the company this belongs in. Shipping a one-off picker here means a folder preference that lives in the basket's file while every other export writes to `exports/`, a second folder-picker call site, a path to validate and refuse, and a UI row — for a default nobody has yet complained about. When the coach asks where their exports go, the answer arrives once, for exports and baskets together.

- **Name:** `<basket name>.mp4`, from the sheet's name field, with `/` and `:` replaced as `file_name` already does — *"the path separator, and … which a share to a Mac or a Windows machine trips over"* (`bus/export.rs:190-196`). No `<project>` suffix — `file_name` joins the label and the project name with a dash today — because there is no one project to name. So `file_name` gains a sibling for the basket rather than an `Option<&str>` parameter, since the basket's rules below are its own.
  - **Trimmed, and a default when there is no usable stem.** The field is free text: `"  "` or `""` would give `" .mp4"` or `".mp4"` — a dotfile with no stem. So the name is trimmed, and an empty result becomes **`Basket`**. The sheet shows that default as the field's placeholder, so what the coach sees is what they get. **Corrected in review (2026-09-25):** the empty test is not enough, because `/` and `:` are *replaced* rather than stripped — `"."`, `".."` and `".hidden"` all survive cleaning and give a hidden file the folder the sheet just named does not show. So any cleaned stem that **starts with a dot** takes the default too.
  - **And cut to 200 bytes, on a character boundary** (review, 2026-09-25). Every filesystem here caps one name at 255 bytes, and cutting at `File::create` — after twenty pieces have resolved — is the worst moment to find out. 200 leaves room for `.mp4`, a ` (10)` suffix and a multi-byte character straddling the cut, and no name a coach types reaches it.
  - **A repeat Start does not silently overwrite a different film.** `<label> - <project>.mp4` is safe to overwrite because it is derived from stable identity — the same target of the same project. A basket's name is typed, free-form, and the basket's *contents* change under it: `Corners` in October and `Corners` in March are two films. So if `<name>.mp4` exists, the run writes `<name> (2).mp4`, then `(3)`, and **the sheet's message line names the file that was written**. That borrows `de_duplicate`'s numbering shape (`bus/export.rs:468-487`) but is its own few lines: `de_duplicate` settles two *targets in one run* colliding, and this settles a path already on disk.
  - **Suffixing rather than refusing**, because Start is the "walk away" button: refusing after twenty pieces have resolved, at the last possible moment, over a file the coach may not care about, is the worse failure. Nothing is lost either way, and the message line says what happened. The cost is that re-running the same basket at a different resolution leaves both files — see **Open questions**.
- **The `.part` then rename is unchanged** (`export.rs:375-386`, `:403-404`), so a refused or cancelled basket never touches a file already at its path.

**O2. Resolution and quality are the basket's, not a project's.** The sheet has those two pickers and no Scoreboard picker (**O5**). They are remembered in `basket.json` beside the pieces, as labels (**H2**). The basket must **not** write into the open project's `Preferences` the way `start_run` does for a normal run (`bus/export.rs:356-365`): those fields are that project's memory of its own last export.

**O3. Chapters: one per piece, unchanged.** `entry_chapters` titles each chapter with the entry's text bar line and skips a plan with fewer than two entries (`plan.rs:148-158`) — for a basket that is exactly right: jump to each corner. The `chpl` box, the `.chapters.txt` beside the file and the YouTube rules `chapter_list` enforces (first line `0:00`, at least three, ten seconds apart — `crates/video-coach-core/src/chapters.rs:10-31`) all apply as they stand, including its removal of a stale list (`export.rs:465-499`). A basket of two 8-second pieces gets no pasteable list, for the same reason a two-clip compilation doesn't.

**O4. Tags: a sibling of `file_tags`, telling the truth about a film that spans matches.** `file_tags(project, target, date)` is per project (`metadata.rs:103-131`), and the module's own rule is the one to follow: *"**Where a tag can't be told the truth it is left out** rather than guessed"* (`metadata.rs:16-19`). So `metadata::basket_tags(name: &str, matches: &[&Project]) -> FileTags`:

- `title` — the basket's name.
- `description` — `"A Coach Cuts basket of 7 pieces from 3 matches."`
- `comment` — **empty**. `final_score` states one match's result (`metadata.rs:168-176`); a film of three matches has no result to state.
- `keywords` — every contributing match's team names. `team_keywords` (`metadata.rs:181-193`) dedupes **within one project**: it drops blanks and collapses a pair of identical names. Across matches, *"Rovers"* appearing in three of them is a new duplicate that function has never seen, so the cross-match dedupe is **new code, not reuse** — a second pass over the concatenated lists, applying the same two rules one level up. Small, but it is a behaviour to write and to test, not a function to call. This is the one tag that is *more* useful across matches: a library can group the film under all six clubs.
- `encoder` — `APP_NAME` and the version, unchanged.
- `date` — **`None`**. `source_date` is *"the footage's date, not the export's"* (`bus/export.rs:583-604`); a film spanning three match days has no footage date, and the earliest of them would be a guess.

`match_name` becomes `pub` (`metadata.rs:139-147`) since the text bar needs it too (**T2**).

**O5. The scoreboard is burned in, and there is no cue sidecar.** `carry_scoreboard` already settles this for everything but the whole match: a clip *"is drawn on, zoomed and captioned, so it re-encodes either way, and a subtitle line repeating its own text bar would be clutter"*, and its cue slot is `None` so that a `.srt` beside the output — the coach's own file — is neither written nor removed (`bus/export.rs:538-550`). A basket is clips. So: `cues: None`, `scoreboard` burned per entry (**J3**), and `scoreboard_cues` is untouched — which matters, because it reads one context per compilation (`crates/video-coach-core/src/cues.rs:50-60`) and would be the second thing needing a per-match rewrite if a basket ever offered a cue track. See **Deferred**.

**O6. Mixed resolutions, aspect ratios and frame rates need no rule, because the graph already handles them** [cited]. The mixer is pinned to 1920×1080@30 and every entry is letterboxed into it by `fit_rect`, recomputed when the entry changes, with the `appsrc`'s caps reset on the same frame — *"Caps are safe to set from the pushing thread: the change lands on exactly the frame pushed after it (measured)"* (`export.rs:572-587`, `:668-677`). Source frame rate never mattered: the pump answers *"last decoded frame with PTS ≤ `source_time`"*, and *"that one rule covers freezes, 25→30 fps duplication and 60→30 fps drops"* (`crates/video-coach-core/src/export.rs:4-7`). So a 4:3 phone clip between two 1440p pieces letterboxes; it is not refused. The project-level `AspectMismatch` refusal (`bus/mod.rs:411-415`) is about one project's sources sharing a chrome coordinate space and does not apply across matches.

### T. What the text bar and the chapter titles say

**T1. The bar is three parts: `"<match> | <clip name> | tags"`, empty parts dropped.** Today it is `"<n> / <total> | <name> | tags"` where `<total>` is the target's clip count (`plan.rs:90-95`, `:160-171`). A basket's bar **drops the position** and puts the match in its place.

**T2. The match is named, because that is what changes between pieces.** Two adjacent corners from two different games look identical. The match label is `match_name(project)` — `"Rovers v Athletic"` from the scoreboard config — falling back to the project's own `name`, and to `UNTITLED` for a project with neither (`metadata.rs:139-147`, `:38`). Not the folder name: the coach's own folders are called things like `20260917-canfield` (#85), which names nothing a viewer knows.

**T3. The position goes, and this is why the bar is three parts and not four.** The reason is mechanical, not a matter of taste. The bar is left-aligned and **ellipsized, never shrunk** (`min_font_size` equal to the style's size — *"it is one long sentence, and a sentence that shrank with its length would leave the bar's size dancing entry to entry"*, `overlay.rs:226-232`, `:341-350`). So a long line loses its **tail**, and whatever comes first spends the safe end of the line. A four-part `count | match | clip | tags` therefore spends that safe end on `"3 / 7"` — the one part the coach himself said *"across matches, numbering means nothing"* about — and puts the clip's name, the thing that says *which corner this is*, where the ellipsis eats it. Three parts hand the safe end to the match and the clip name, in that order, and drop the tags first, which are also in the chapter title and the file's keywords.

**And the position is not lost: it is in the chapters.** `entry_chapters` writes one chapter per piece, in order (**O3**), so a viewer's player shows "3 of 7" in its own chapter list — rendered by something built for it, at a size that doesn't compete with the clip's name.

**T4. Chapter titles are the same line, unchanged.** `entry_chapters` uses the entry's text (`plan.rs:148-158`), so `"Rovers v Athletic | Corner, 2nd half"` is both the burned-in caption and the chapter, and `one_line`'s whitespace collapsing already protects the pasteable list from a clip name with a newline in it (`chapters.rs:47-60`).

### U. The UI

**U1. Added from the clip row's own menu: "Add to basket".** That menu already holds *Jump to clip start / Preview clip / Export video… / Delete clip* (`crates/video-coach-app/ui/app.slint:3509-3523`), and "Export video…" is its nearest neighbour in meaning. It acts on the row it was opened on, not on the selection, like every other item there. A basket item is added for the open project, so it needs no gate beyond the menu's own `enabled: !root.recording`.

**No key in v1.** `b` is free (`app.slint:3176-3264` binds `r`, space, `a`/`d`, `,`/`.`, `[`/`]`, `j`/`l`, `1`, `2`/`3`, plus `z`/`x`/`v` and the Ctrl set), so one can be added later at no cost. See **Open questions**.

**U2. The badge is the answer to "is a basket waiting".** The bottom bar's button reads **"Basket (4)…"** and sits beside **Export…** (`app.slint:4140-4152`). The count comes from the stored list, so it is right the moment the app starts and survives every project switch — which is the whole point: the coach is in project B and must be able to see that four pieces are waiting. It reads **"Basket…"** and is disabled when the basket is empty. Unlike Export… it does **not** require an open project: a basket can be started with none, and it stays enabled while a run is going so the run can be watched and cancelled (the same reasoning as Export…'s comment).

**U3. One `Sheet`, not a fifth panel shape.** `BasketSheet inherits Sheet` at **520 px** — the widest thing in it is a row's `"Rovers v Athletic — Corner, 2nd half"`, so it wants the setup sheet's width rather than the export sheet's 480 (`Sheet` is `app.slint:1392-1428`; the widths are `:1454`, `:1952`, `:2229`). Wrapped in `Scrim` like the other four (`app.slint:1377-1383`). Top to bottom:

1. the **piece list** — one row per entry, in order: `↑` `↓` to move, the match, the clip's name, its length, and `✕` to remove. A row whose reference is dead is drawn in `alternate-foreground` with why (`the project is gone`, `the clip was deleted`, `the game video is missing`) and is not exportable (**V1**). A list with a height of its own in the house idiom (`height: min(280px, …)`, as `app.slint:962`, `:1461`, `:1520` and `:2245` do) so a 20-piece basket scrolls rather than growing the sheet past the window.
2. the **name** field (placeholder `Basket`, **O1**) and the **Resolution** and **Quality** pickers — the export sheet's own two, verbatim. **No output-folder row and no Change… button** (**O1**).
3. the **run row**: the one target's progress and "Finishes at …", exactly as `ExportSheet` renders a run from the bus's events rather than from its own state (`app.slint:1513-1525`).
4. one **message line** — for refusals, and for the file the run wrote when the name was suffixed (**O1**) — then **Clear**, **Start** and **Close**.

**U4. The sheet takes the export sheet's key guard, plus the setup sheet's `reject`.** The guards are ordered in `handle-key` (`app.slint:3044-3110`: the error dialog at `:3045`, the export sheet at `:3054`, the match setup sheet at `:3068`, the match editor's, then the `text-editing` yield at `:3108`). The basket sheet has a text field (the name), so it takes the setup sheet's branch — Esc closes it, everything else goes to whatever has focus — and its `editing` folds into `text-editing` (`app.slint:2832`) as `Inspector`'s and the match editor's do. **Miss that fold and Esc never leaves the field**, and the first Esc closes the sheet from inside it. Esc closing from inside the name field is acceptable here and not in the match editor, for the reason the editor's own comment gives: the editor's paste box holds a half-typed block, and a name field holds a word.

**U5. The sheet is modal, so nothing changes under it.** That is what lets the rows be resolved **once**, when the sheet opens (**E3**): the coach cannot rename a clip, delete one or open another project while the scrim is up. Start resolves again from scratch anyway, because a refusal has to be current (**V1**).

**U6. Reordering is `↑`/`↓`, not drag.** The clip list's drag-reorder exists and carries real machinery (`DragArea` / `DropArea` and `dropped-index`, `app.slint:3478-3491`); a 5-row modal list does not earn a second copy of it. Two buttons per row, disabled at the ends.

**U7. Clear empties the basket, and there is no undo for it.** Removing eight rows one at a time at the end of a session is worse than the risk, and what is lost is a list of *references* — every clip is still in its project — so the cost of a mis-click is a re-gather, not data. Stated plainly in the sheet's tooltip. See **Open questions** for whether it should confirm.

**U8. Start leaves the basket alone.** It is not emptied on success: the coach may re-run at another resolution, or add a ninth piece and run again. Clear is how a basket ends.

### V. What is refused, and when

**V1. Everything is refused before a byte is written, naming the piece.** The rule is the export's already: *"Every target is checked before any of them runs, so a missing file can't stop a run half-way through"* (`bus/export.rs:330-331`), and media only warns and degrades, so this check is *"what makes the loss visible at all"* (`bus/export.rs:9-13`). A basket resolves every piece at Start, and the **first** failure refuses the whole run as `UserError::CantExport` (`bus/mod.rs:465-467`) — all-or-nothing, not best-effort: a film silently missing the piece the coach cared about is worse than no film.

**The cheap refusals come first, before any I/O** (**C3**): a run in progress and a preview open are two field checks, and there is no reason to stat three projects' worth of files before hitting them.

Named refusals, in the order they can be hit:

| What | Message |
|---|---|
| a run or a preview is going | the existing `an export is running` / `a preview is open; close it first` (`bus/export.rs:315-322`) |
| the basket is empty | `can't export: the basket is empty` |
| the project folder is gone, `project.json` is unreadable, **or its format is too new or too old** | `can't export: the project at <folder> can't be read: <why>` (**V6**) |
| the clip was deleted | `can't export: <match> — a piece's clip is gone` — the same fact `label` reports as *"the clip is gone"* (`bus/export.rs:447-467`) |
| the piece's game video is missing | `can't export: <match> — <clip>'s game video is missing; relink it first` (`bus/export.rs:645-661`) |
| the commentary recording is missing | `can't export: <match> — <clip>'s commentary recording is missing` (`bus/export.rs:666-670`) |

**Every message names the match as well as the clip**, because two projects can hold clips with the same name and *"Corner's game video is missing"* would not say which one to go and fix. The wording is not copied: **the same resolver produces both the basket's and the export sheet's sentences** (**C3a**), with the match prefix empty for an ordinary export.

**V2. The missing-file check is a `stat` per piece, not `Bus::missing`.** `missing` is one flag per source of the **open** project, refreshed on open and on every source-list change (`bus/mod.rs:343-345`, read by `job()` at `bus/export.rs:645`). It says nothing about a closed project. Since the shared resolver (**C3a**) has to stat anyway, it stats for *both* callers and stops reading `missing` at all: one `Path::exists` per entry, beside the recording check that already does exactly that (`bus/export.rs:667`). The ordinary export gets marginally *more* current as a result — a source deleted since the last refresh is now caught — and `Bus::missing` keeps its real job, which is the UI's relink affordance.

**V3. Footage that moved is refused, never guessed at.** Relink is a project-level command that needs the project open (`Command::RelinkSource`, `bus/mod.rs:87`), so the refusal has to point the coach at the project: *"relink it first"* is already the wording.

**V4. Nothing about mixed resolutions or frame rates is refused** — see **O6**.

**V5. Adding a clip already in the basket is a no-op with a notice.** `Transcribe`'s doc sets the precedent: *"Does nothing if it is queued or running already"* (`bus/mod.rs:296-303`). The notice says `already in the basket`.

**V6. Every basket refusal about a project is one shape, and it names the folder.** `From<StoreError> for UserError` maps a format mismatch to `TooNewProject` / `LegacyProject`, whose wording is *"this project was made by a newer version of Coach Cuts (format v12)"* — **it names no path** (`bus/mod.rs:430-431`, `:497-510`). That is fine when the project in question is the one the coach just asked to open, and misleading when it is one of three the basket is reaching into: *which* project? So `project_for` (**E3a**) wraps whatever comes back from `store::read` into the one shape, keeping the store error's own sentence as the reason:

```text
can't export: the project at /home/coach/matches/20260917 can't be read:
this project was made by a newer version of Coach Cuts (format v12)
```

One shape for a missing folder, a malformed file, a legacy format and a future one — because from the basket's point of view they are one fact ("this piece's match can't be read") plus a reason, and the folder is the only thing the coach can act on.

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
/// Render the basket as one file. `name`, `resolution` and `quality` are the
/// sheet's, and become the basket's (spec H2, O2). **No folder**: the output
/// directory is fixed (spec O1).
ExportBasket { name: String, resolution: Resolution, quality: Quality },
```

**C2. None of them touches a project, and none is on the recording allow-list.** The allow-list is a `matches!` at the top of `Bus::command` — *"everything not listed is refused, so commands added later are too"*, and a command it doesn't list is **dropped with a log line** (`return eprintln!("bus: refused while recording: {cmd:?}")`, `bus/mod.rs:803-836`), not queued for afterwards. Nothing here belongs on it: a clip only exists once its recording has stopped, and `ExportBasket` waits exactly as `Command::Export` does (*"Recording and export never overlap (a user decision)"*, `bus/export.rs:25-27`). The UI greys the button while recording, as it does Export…, so reaching the guard is a UI bug — which is what the log line is for.

**C3. `start_run` splits, and both halves keep the one-run / one-progress / one-cancel shape.** Today `start_run(targets, pickers)` labels the targets, builds a job each, creates `exports/`, writes the pickers into the project's `Preferences` and starts the first (`bus/export.rs:313-387`). It becomes:

```rust
/// Begins a run over jobs the caller built: refuses a second run, a preview,
/// or an empty list; then starts the first and publishes the run.
///
/// **The caller creates its own output directory**, after its own refusals
/// and before this: `export()` the project's `exports/`, the basket its one
/// folder. A directory is the last thing either does before starting, so a
/// run that can't start leaves none behind.
fn begin(&mut self, jobs: Vec<(String, ExportJob)>);
```

**`begin` takes no output directory.** An earlier draft passed one, so that `begin` could create it after its own refusals. But then the two callers' refusals and the one directory are interleaved across two functions, and neither caller can put its *own* refusals before the directory it owns. Each caller already knows its folder, creates it after refusing, and hands `begin` nothing but jobs — which is also exactly what #77's queue will want, since a queued run's jobs were built (and their folders decided) long before Start.

**The two cheap refusals stay at the front of both paths.** `a run is going` and `a preview is open` are two field checks; resolving a basket is three `store::read`s and a `stat` per piece. So they live in one small `refuse_if_busy()`, which **each builder calls first**, before any I/O. **Corrected in review (2026-09-25):** an earlier draft had `begin` call it a second time, "so no caller can skip them", which made `begin` fallible for a case neither caller can produce — both refuse first, and both refuse an empty list of targets before they build a single job. `begin` is infallible, and a run that reaches it starts.

**The pickers are written back after `begin`.** Today the write-back is after every refusal, so it is correct by accident of ordering (`bus/export.rs:356-365`); with the split it stays after the call, so nothing on the way to a refusal can dirty `Preferences` and save the project. The basket is the other way round: `basket.json` is written on every change, a refused Start included, because that file *is* the sheet's memory and there is no project to dirty.

`Active`, `ExportRun`, `TargetState`, `finish_target`, `next_job`, `export_message`, the rate window and `CancelExport` are **untouched** (`bus/export.rs:55-116`, `:199-292`, `:399-443`). A basket run is a one-row run, so its progress, its estimate, its `bus: exported …` log line and its cancel are the ones that already work. **This is the split #77 needs too, and whichever lands first does it** — see **BACKLOG note** below.

**C3a. One per-entry resolver, shared by both job builders.** `job()`'s per-entry body (`bus/export.rs:636-675`) — find the clip, refuse a missing game video, refuse a missing recording, build the media — becomes a function both builders call:

```rust
/// One entry's media, or why it can't run. **The one place a missing game
/// video or commentary recording is refused** (spec V1).
///
/// `whose` prefixes the refusal: empty for an export of the open project,
/// `"Rovers v Athletic — "` for a basket piece, where two projects can hold
/// clips with the same name.
fn entry_media(
    folder: &Path,
    project: &Project,
    match_media: &Arc<MatchMedia>,
    entry: &PlanEntry,
    whose: &str,
) -> Result<EntryMedia, UserError>;
```

**Not a second refusal path.** The alternative — a basket resolver that writes the same three sentences again — is how the export sheet's *"…'s game video is missing; relink it first"* and a basket's near-copy of it drift apart on the first edit to either. One function, one set of sentences, identical **by construction**.

**C4. `Event::Basket(BasketView)` is the one thing the UI renders from**, following `Event::Export(ExportRun)`'s rule — *"The sheet renders the run it is handed, so it can't be left holding a state the bus has moved past"* (`bus/export.rs:15-18`).

```rust
pub struct BasketView {
    pub name: String,
    pub resolution: Resolution,
    pub quality: Quality,
    pub pieces: Vec<BasketRow>,
}
pub struct BasketRow {
    /// `"Rovers v Athletic"`, or the project's name (spec T2).
    pub match_label: String,
    pub clip_label: String,
    /// **`Clip::recording_duration`** — roughly how long the piece runs.
    pub seconds: f64,
    /// Why this piece can't be exported, or empty (spec V1).
    pub problem: String,
}
```

**The length is the clip's own `recording_duration` (`project.rs:166`), not a plan.** A row wants "about 12 s"; building a `CompilationPlan` per piece to get it means `playback_segments`, the duration authority and the per-entry frame quantization run once per row, every time the sheet opens, to produce a number whose only job is to tell one row from another. `recording_duration` is exact for the common clip and differs only where the coach skipped or froze inside it. So the row shows it, and **the run's own frame counts remain the only authority on the film's length** — `plan.total_frames()`, as everywhere else.

Published at `Bus::spawn`, on every basket mutation, and on `ShowBasket`. **Not** on `ProjectChanged`: that fires on every clip edit, and re-reading three `project.json` files per keystroke to refresh labels nothing can see (the sheet is modal, **U5**) would be waste. The badge's count is `pieces.len()`.

**The name and the two pickers are seeded while the sheet is closed, and only then** (settled in the 2026-09-25 review, which found the doc and the Slint comment describing two different rules). The pieces are re-rendered from every event; the three fields follow the bus's event whenever `basket-sheet-open` is false and are left alone whenever it is true. So the sheet opens on the basket's own values — the bus takes the sheet's at Start — and re-seeding a name under the coach's hands, the one thing a field must never do, is impossible. There is no "seeded once" flag: the sheet's own open state is the condition.

**C5. The caller-captured-timestamp rule has nothing to bite on.** CLAUDE.md's bus contract is about commands that land in the commentary event log, whose timestamps would drift with queue delay. No basket command carries a position or a time; `ExportBasket` reads no pipeline. (The same argument the match editor spec makes for typed times, C2 there.)

**C6. Refusals are `UserError::CantExport`, which is a modal, not a notice** — as every export refusal is today (`bus/mod.rs:465-467`; `is_notice` at `:486-494` lists only `Avatar`, `DeviceFallback`, `StopNotClean` and `Scoreboard`). The sheet's own message line also shows it, because the status bar's notice line renders *behind* the scrim (the match editor spec's C5: the notice line is `app.slint:4329`, the scrims are siblings drawn after it at `:4381`, `:4403` and `:4497`). **V5**'s "already in the basket" is the one basket message that is a *notice*, following `Command::Transcribe`'s precedent — a refusal of one click with nothing to answer.

**C7. BACKLOG note: #86's ordering is void, and #77 inherits `begin`.** #86 today says *"When to revisit: after #77's queue, whose machinery it shares."* That ordering was written before **C3**, and it is backwards as a dependency: the split is one function either feature can do, and this one is the smaller. So the basket does not wait on the queue. What #77 inherits is `begin(jobs)` and `refuse_if_busy()` already in place, which is most of *"don't tie the run loop to the open project"* — its own listed work. Both backlog entries say so once this ships.

### F. Format

**F1. `project.json` does not change and `CURRENT_FORMAT_VERSION` stays 11.** Nothing about the basket is a project's (**H1**). No `Project` field, no `Preferences` field, no new `Clip` field.

**F2. `state.json` does not change either.** The basket is its own file, `basket.json`, beside it (**H1**, **H2**). `AppFiles` gains nothing, so no existing setter can lose a field to a basket value it can't parse, and an older build ignores a file it has never heard of.

**F3. A dead reference is never pruned automatically.** A piece whose project is gone stays in the basket, greyed out with its reason, until the coach removes it. Silently dropping it would hide the one thing they need to know — that the film they asked for is missing a piece — and the folder might be an unmounted drive that is back tomorrow.

### N. What it must not break

- **The single-project export paths.** Every existing `ExportTarget` keeps its behaviour byte for byte. The changed call sites are: the three readers of `job.sources`, the one reader of `encode.scoreboard`, the one of `encode.highlights`, the avatar's open-and-gate, and `encode.entries` becoming a `Vec<EntryMedia>` whose clip is the `Option`. The Phase 8 and Phase 9 media tests are the proof, and **they must pass unchanged in behaviour** — only in the construction of their fixtures.
- **`core` does not change at all for the reshape.** `PlanEntry`, `plan.rs`, `reel.rs`, `whole_match.rs` and `source_index`'s project-local meaning are untouched (**J1**); what core *gains* is additive — `BasketPiece`, `clip_source_duration`, `basket_plan`, `basket_schedule`, `basket_tags`, the extracted per-clip entry helper, and `match_name` made public.
- **`audio_regions` keeps its signature** and its eight callers (**J6**).
- **The Phase 9 clock rule.** `state_at(entry.source_index, frame.source_time)` per frame, per entry's own board. No per-clip constant, nothing cached on `PlanEntry` (`plan.rs:97-113`).
- **`core` declares no media dependency.** Everything it gains is pure; the `&Project`s and `&Clip`s it takes are its own types.
- **Every pad gets a buffer for every frame**, with a **GL** filler (`export.rs:17-21`, `:63`). A basket alternates fed and filler PiP pads between matches far more often than a single project does.
- **`plan.total_frames()` is the denominator**, never a duration sum (`plan.rs:137-146`) — and never `BasketRow::seconds`, which is a label (**C4**).
- **The zero-copy diagnostic survives the bounded decoder cache** (**J2**): `bus: loaded …` must still name the decoder, the caps and the GL platform after a run whose first decoder was closed.
- **The `.part` then rename**, so a refused or cancelled basket never touches a file already at its path (`export.rs:375-386`).
- **No test reaches the network, and none opens the real camera or mic.**

---

## Crate responsibilities

| Crate | What it gains | What it must not gain |
|---|---|---|
| `video-coach-core` | `BasketPiece`; `clip_source_duration`; `basket_plan`; `basket_schedule`; the extracted per-clip-entry helper; `metadata::basket_tags` with its own cross-match keyword dedupe; `metadata::match_name` made public | `PlanEntry::match_index` or any other media coordinate; a change to `audio_regions`; a generified frame walker; any notion of "the basket" as state; any path to a *folder* it must read; any media dependency |
| `video-coach-media` | `Render::Copy(Vec<PathBuf>)`; `EntryMedia { source, clip: Option<ClipMedia>, match_media: Arc<MatchMedia> }` replacing `ExportJob::sources` and `Encode`'s `scoreboard` / `highlights` / `avatar`; the board, highlights and avatar read per entry; the decoder cache keyed by path **and bounded**; the avatar cache keyed by path; the diagnostics taken at open | any knowledge of projects, folders or the app's state files; a second renderer; a per-job avatar gate |
| `video-coach-app` | `bus/basket.rs` (the list, `basket.json` read/write with its label discipline, `project_for`, the refusals, the job builder); six commands; `Event::Basket`; `start_run` split into `begin(jobs)` + `refuse_if_busy()`; the shared `entry_media` resolver; `BasketSheet` and the bottom-bar button | the basket on `Open`; a write into any project's `Preferences`; a key in `state.json`; a second refusal path; a second run loop |
| `video-coach-harness` | `tests/basket.rs` — two projects, pieces from both, over the bus | — |

---

## Testing

**`video-coach-core` (no GStreamer):**

1. `basket_plan` over pieces from two projects: entry order is the piece order; each entry's `source_index` is its own clip's; `start_frame`s are the running quantized sum; `total_frames` is the last entry's end.
2. A piece from match B keeps B's `source_index` even when A has more sources than B has — the regression a flat merged source list would cause, and the reason `source_index` stays project-local (**J1**).
3. `clip_source_duration`: a clip whose source is present plans against `SourceRef::duration_seconds`; one whose source is gone against `start_source_seconds + recording_duration`. The same two answers `compilation_plan` gives today, now from one function both builders call.
4. Each entry's text is `"<match> | <clip> | tags"`, with empty parts dropped, **no position**, and a project with no scoreboard falling back to its `name` and then to `UNTITLED`.
5. `chapters` is one per piece, at `start_frame / 30`, titled with that line — and empty for a one-piece basket.
6. `basket_schedule` gives each entry its **own clip's** events, read from `pieces[i].clip` directly: a piece whose clip has a zoom event zooms, and its neighbour from another project does not. And the events are never looked up by id, so no miss can degrade to identity zoom (**J7**).
7. `basket_tags`: no comment, no date, title the basket's name, description counting pieces and matches, and **keywords deduped across matches** — three projects two of which are `Rovers v …` yield one `Rovers`, and a blank name yields none.
8. **The clock test, which is the point of the feature:** two projects whose kick-offs are tagged differently; `ScoreboardContext::for_project` each; assert that a frame of piece 1 and a frame of piece 2 at the same *output* time read their own match's clock and score. The existing pause test (`core`'s Phase 9 one) is the model, and it must keep passing.
9. **`audio_regions` is unchanged**, which its existing tests already pin (`core/tests/audio.rs`). No new test, and no new signature (**J6**).

**`video-coach-media` (GStreamer, llvmpipe on CI):**

10. An `ExportJob` whose entries carry **two different `MatchMedia`**, two fixture sources of **different resolutions**, and a board on each: the file is 1920×1080@30, the entry boundary re-letterboxes, and the burned board changes team names across it. Read back by decoding a frame either side of the boundary (the Phase 9 scoreboard-pixel tests are the model).
11. One match with an avatar and one without, alternating: the no-avatar piece gets the **GL** filler, the run completes, no pad stalls, and one `AvatarInset` is opened for two entries that share an image.
12. **The bounded decoder cache** (**J2**): a three-entry job whose middle entry reads a different file, asserting the first file's decoder is dropped before the last entry and reopened for it — and that a single-file compilation opens exactly one decoder and keeps it. The check is on the cache, not on a timing.
13. **The diagnostics survive it**: `Rendered::diagnostics` still names a decoder after a run whose first entry's decoder was dropped mid-run.
14. `Render::Copy(files)` still joins a whole match — the reshape's regression test, and the one that proves `files()`'s removal lost nothing.
15. An entry whose `source` file does not exist fails the run with the entry named, and leaves no `.part`.

**`video-coach-app` (unit, no GStreamer needed):**

16. `basket.json` round-trips; an **unknown resolution or quality label reads as the default and the pieces survive** (**H2**) — the test that pins the reason this is not a key in `state.json`; a corrupt file reads as an empty basket and logs; and writing the basket **does not touch `state.json`**, asserted by setting the pen and the last project first and reading them back after.
17. The output name: trimmed; empty becomes `Basket`; `/` and `:` replaced; an existing `Corners.mp4` gives `Corners (2).mp4` and then `(3)` (**O1**).

**`video-coach-harness` (over the bus):**

18. Two projects in one temp dir, each with a fixture video and one clip. Open A, `AddToBasket`; open B, `AddToBasket`; `ExportBasket`. Assert: one `ExportRun` with one target, progress events, `TargetState::Done`, the file in the output folder, its `.chapters.txt`, and **no `.srt`** (**O5**).
19. The basket survives a project open: `Event::Basket` after opening B still lists A's piece with A's match label.
20. The basket survives a **bus restart**: write `basket.json`, spawn a fresh bus, and the pieces are there (the `restore_last_project` tests are the model).
21. Each refusal in **V1**'s table, each naming its piece, each leaving no file: delete the clip, delete the source, delete the recording, remove the project folder, **make its `formatVersion` too new** (the one that must name the folder, **V6**), empty basket, run while a run is going.
22. **A refused Start changes nothing about the project.** With the open project's `Preferences` at a known value, a Start refused for a missing source leaves `project.json` byte-identical and publishes no `ProjectChanged` (**C3**). The same test with an ordinary `Command::Export` pins the write-back's new position.
23. **A piece of the open project resolves from memory, not the file** (**E3a**): rename a clip, and both the sheet's row and the run's text bar use the new name in the same command sequence.
24. `MoveBasketEntry` and `RemoveFromBasket` reorder and shorten the run's entries in the expected order.
25. A basket run cancels like any other (`CancelExport` → `TargetState::Cancelled`, no file).
26. `AddToBasket` for a clip already in it changes nothing and emits the notice.
27. **Every existing export test passes unchanged.** The reshape's real proof is `crates/video-coach-harness/tests/` and `crates/video-coach-media/tests/export.rs` going green with no behavioural edit.

**What needs the coach's eyes** (batched with the other hands-on checks, per the port's working order):

- **The three-part text bar on real footage** (**T1**, **T3**) — is `"Rovers v Athletic | Corner, 2nd half | corners"` readable at 1080p, or does the ellipsis still eat the clip name? Changing it is one function in `plan.rs`.
- **Whether the position is missed.** It is in the chapter list now, not on the picture (**T3**). Watching a 7-piece film is the only way to know whether that is enough.
- **The board changing between pieces** — whether two clubs' kit colours flipping mid-film reads as intentional or as a glitch.
- **The join between two matches recorded differently** (exposure, white balance, a different camera position). No cut, no fade — is that acceptable, or does a basket want a 6-frame dip to black between pieces? (See **Open questions**.)
- **The level of a piece from a project whose volumes were turned down** (**J6**) — the basket mixes at 1.0/1.0, so a coach who scans that match quietly gets a louder film than the preview they remember. Expected to be right; worth one listen.
- **The output folder** — `<Videos>/Coach Cuts` is a guess about where the coach wants films that belong to no match, and there is no way to change it until #78. **[unmeasured]**
- **The suffixed file name** (**O1**) — whether `Corners (2).mp4` after a re-run at another resolution is helpful or just litter.

---

## Risks

1. **The `EntryMedia` reshape touches the export path's central types for a feature with one caller.** It is the honest shape — the per-project data really is per-project, and it now hangs where per-entry data already hangs — and it removes a field (`ExportJob::sources`) rather than adding a coordinate. It repairs `Render`'s stated invariant on the way through and leaves `core` untouched. But it is the largest diff here and it lands in the middle of the run. Mitigation: it is mechanical, it is type-checked end to end (there is no index to get wrong), the existing Phase 8/9 media and harness tests cover the single-project case exactly, and nothing about the frame loop's logic changes. **It lands first, on its own, with every existing export working unchanged.**
2. **The bounded decoder cache is the one behavioural change to today's runs** (**J2**). A single-project compilation is unaffected by construction — every entry reads the same file, so nothing is ever dropped — but the rule is new code in the hot loop, and the `Rendered::diagnostics` move is a consequence that is easy to miss. Tests 12 and 13 exist for exactly this.
3. **Per-match `AvatarInset`s are a new allocation pattern in the run.** One GL texture per distinct avatar image instead of one per run. Small **[unmeasured]**, but it is a per-run GL resource and #55 already records an fd leak in export runs. Keying on the path is what keeps the common case (one image reused across projects) at one.
4. **A reference basket can be "all dead" after a folder move**, and the coach's only recourse is to remove the rows and re-gather. That is the accepted cost of **E1**, and **F3** keeps the rows visible so the recourse is obvious.
5. **A source relinked to different footage renders the wrong pictures silently** (**E1**). Not introduced here — it is true of every export of that project — but a basket is the film most likely to be rendered unwatched.
6. **Two projects pointing at the same game video** get one decoder (**J2**) and two boards. That is correct — the same footage tagged twice is two matches' worth of events — but it is an odd enough case to state.

---

## Deferred

1. **The library view over old clips** — #86's second half: a set of project folders the coach can *browse*, to find pieces made months ago. Needs a project set (a folder of folders? the recents list? a "season"?), a clip list per project, and a search over tags. Not in this spec, and the basket does not depend on it: this spec's basket is filled as you work.
2. **A tag, a reel or a whole match as a basket piece** (**E4**). `basket_plan` takes pieces, so a piece that expanded to several entries is a change to the builder and to nothing else. Revisit when the coach asks for "every corner of match A" as one item rather than five.
3. **Per-piece gain** (**J6**). Not per-*match* volumes, which were considered and rejected as an invisible cause of an audible jump, but a number on the basket row — "this one quieter" — which is visible, is the piece's own, and would arrive with a control beside it. It also wants the per-clip case first (#52: there is still no UI for the source and commentary volumes at all), so it belongs with that.
4. **An output-folder preference** (**O1**), with BACKLOG #78's other export settings, and with its home decided once for exports and baskets together.
5. **A cue-track basket** (**O5**). Would need `scoreboard_cues` to take a per-match board (`cues.rs:50-60`). No reason to yet: a basket is clips, and clips burn the board in.
6. **A transition between pieces** — a dip to black, or a title card naming the match. The film is a hard cut today. Needs the coach's eye first (see **Testing**).
7. **Reordering by drag** (**U6**), and a keyboard shortcut for Add (**U1**).
8. **The basket as a saved, named, re-exportable compilation** ("my corners reel, kept"). That is a document, which means a file format and a home for it — a real feature, and a different one from a scratch list.
9. **Music under a basket** — #84's question, unchanged by this: a basket of goals is exactly the film people expect music under, and the licensing half is still the blocker.
10. **Queuing a basket behind other exports** — the composition of #77 and this. Free once both exist, given **C3**'s split, which this feature lands.

---

## Open questions for the user

Each has a recommended default, which is what will be built if nothing is said. Four of the first draft's seven are now **settled by the review** and are recorded here as decisions rather than questions.

1. **Should Start empty the basket?** *Default: no (**U8**); Clear is the way out.* The opposite reading — "Start consumed it, the basket is for the next one" — is defensible, and is what a queue does.
2. **Should Clear confirm?** *Default: no.* It destroys references, not clips, and the app's other immediate action (Delete clip) has undo where this has none.
3. **A key for Add?** *Default: none; `b` is free.* `b` on the selected clip would suit the coach's "make it, add it, move on" rhythm.
4. **Should a dead piece be skipped with a warning instead of refusing the run?** *Default: refuse, naming it (**V1**).* Best-effort would let a coach walk away and come back to a film quietly missing the piece they cared about.
5. **Should the basket's pieces be limited?** *Default: no limit.* Thirty pieces is a 20-minute film and the run machinery does not care; the sheet's list scrolls, and the decoders are now bounded (**J2**).
6. **Does `Corners (2).mp4` grate?** *Default: suffix, and say which file was written (**O1**).* The alternative is to overwrite, which is what an ordinary export does — but an export's name is derived and a basket's is typed over changing contents.

**Settled, not asked:**

- **Where the file goes** — `<Videos>/Coach Cuts/`, fixed, no picker; the preference goes to #78 (**O1**).
- **Whether the text bar keeps `"n / total"`** — it does not. The bar ellipsizes rather than shrinks, so a four-part bar spends the safe end of the line on the part the coach himself said means nothing across matches; the position lives in the chapters instead (**T3**).
- **Whether volumes are per match** — they are not; the basket mixes at the defaults, and per-*piece* gain is deferred (**J6**).
- **Where the basket is stored** — its own file, not a key in `state.json`, because that file is discarded whole on any parse error (**H1**).
