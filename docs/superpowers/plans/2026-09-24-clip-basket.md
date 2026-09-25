# Clip Basket Plan (one film, pieces from several matches)

**Date:** 2026-09-24
**Spec:** `docs/superpowers/specs/2026-09-24-clip-basket-design.md` (decisions E, H, J, O, T, U, V, C, F, N), status *Reviewed (simplify + correctness applied)*. It proceeds on the defaults for the six remaining open questions. The coach's fixed decisions — pieces gathered **while working**, Start gives **one video in the order added**, each piece carries **its own match's** board and clock — are settled in `BACKLOG.md` #86 and in the spec, and are not re-litigated here. So are the four the review settled: the output folder is fixed, the bar is three parts, there are no per-match volumes, and the basket lives in its own file.
**Status:** Not started.

**Scope.** Five tasks: the `EntryMedia` reshape in media, core's basket plan, the bus (the list, its file, the commands, the run split), the sheet, and the closeout. No format change (F1, F2), no new crate dependency in `core`, no new `UndoAction`, nothing on the recording allow-list, and **no change to `core::audio::audio_regions`**.

**Execution.** A fresh subagent per task (`superpowers:subagent-driven-development`), given this plan, the spec and `CLAUDE.md`.
- Tasks run one at a time, in the order written, in one tree. The cargo lock serialises every build anyway.
- The orchestrator runs the gate below and commits each task. It stages paths explicitly, never with `git add -A`.
- **At the end of every task the workspace builds and every test passes.** A task that changes a type fixes every user of it in the same task, in every crate.
- Each task writes its own `CLAUDE.md` addition, where it names one.
- **Task 1 comes first and every later task depends on it.** It must land with **every existing export working unchanged** — see its own section.

**The gate, for every task:** run the `verify` skill, with every cargo call under the machine-wide lock (`flock /tmp/claude-1000/cargo.lock nice -n 19 cargo …`), because other sessions build here too.

**Test-first.** Each task names the test that must fail first. Write it, run it and watch it fail for the stated reason, then build until it passes. A test that fails only because the code doesn't compile yet counts, but the plan names a behavioural failure wherever one exists — and for Tasks 1, 2 and 3 almost every test can be behavioural.

**What tests may touch.** No test reaches the network, the real camera or mic, or the user's footage. The exception is the `#[ignore]`d `COACH_FOOTAGE` tests, which this plan does not add to. Real footage and anything that identifies a team or player is never committed: the repository is public and the footage shows children. That includes file names, team names and shirt numbers, in code, tests, commit messages and docs. **Every test project here uses invented team names** ("Rovers", "Athletic", "City") and `video_coach_media::fixtures`' generated videos. Any test that touches the app's config directory points `XDG_CONFIG_HOME` at a scratch dir (or uses `StateFile::in_config_dir`), never the real one.

---

## Known facts. Don't re-derive these.

Each was checked in the code while writing this plan.

**The three readers of `ExportJob::sources` — all of them, and nothing else**

- The field is `crates/video-coach-media/src/composite/export.rs:126-130`.
- **The decoder cache**, `export.rs:561-566`: `job.sources.get(entry.source_index).ok_or_else(|| ExportError::Failed(format!("{} has no game video", entry.text)))?`.
- **The audio mixer's game track**, `composite/audio.rs:104-108`, inside `Mixer::new`.
- **The copy path**, `composite/copy.rs:232-244` — `fn files(job) -> Result<Vec<&Path>, ExportError>`, whose one caller zips it with the plan's entries at `copy.rs:203`.

A fourth *would-be* reader is the bus's own `copy_files` (`crates/video-coach-app/src/bus/export.rs:608-615`), which builds the same list from the same two values with a **`filter_map`** — a silent drop where `copy.rs:238-243` refuses. It goes away (spec J1).

**The frame loop and its per-job values**

- `Encode` is `export.rs:93-118`: `entries: Vec<Option<EntryMedia>>` at `:97`, `audio` `:102`, `resolution` `:103`, `quality` `:104`, `scoreboard: Option<ScoreboardContext>` `:109`, `highlights: Vec<PlayerHighlight>` `:113`, `avatar: Option<PathBuf>` `:117`. `EntryMedia { recording: :155, clip: :157 }` is `:152-158`.
- `Render` is `export.rs:82-88` — `Encode(Encode)` at `:84`, and **`Copy` at `:87` carries nothing today**. Its invariant is written at `:73-75`.
- The **avatar open** is `export.rs:522-532`, with the per-job gate `.filter(|_| encode.entries.iter().flatten().any(|m| m.clip.shows_avatar()))`; the **pulse gate** is `:533-536` (`match avatar { Some(_) => pulse_levels(job, encode, cancel), None => vec![1.0; …] }`); `pulse_levels` is `:981-993` and filters per entry on `m.clip.shows_avatar()` **alone**.
- The **decoder cache** is `let mut sources: HashMap<usize, Decoder> = HashMap::new();` at `export.rs:542`, with the "alive for the whole compilation" comment at `:539-541`.
- The **board** is read at `export.rs:591-597`, with the BACKLOG #27 comment on it; the **highlights** at `:598-608`; `encode.entries[frame.entry].as_ref()` at `:560`.
- `Rendered::diagnostics` reads **the first entry's decoder after the loop**: `export.rs:646-651`. This is the `bus: loaded …` zero-copy diagnostic CLAUDE.md names.
- The PiP filler's GL rule is `export.rs:17-21`; `FILLER_RECT` is `:63`; the "no usable inset" path is `:679-685`.

**The mixer's reader-closing rule — the model for bounding the decoders**

`composite/audio.rs:144-158`, verbatim: *"Close a file nothing later reads — in practice every recording, as its entry ends. Kept open, a compilation of two hundred clips would hold two hundred pipelines at once, where the picture holds one recording at a time. The source video's reader stays, because the next entry normally reads it again."* The test is `active.iter().chain(&self.order[self.next..]).any(|&i| self.paths[i] == *path)`.

**`StateFile`, and why the basket is not in it**

- `StateFile::read` is `crates/video-coach-app/src/bus/state.rs:158-169`: an unreadable or unparseable file returns **`State::default()`** with an `eprintln!` — the *whole document*, not one field.
- `save` (`:171-178`) writes the whole document, and the doc at `:154-157` says every write reads first for exactly that reason. `the_settings_are_independent` pins it.
- `State`'s fields are `:31-44`, every one `#[serde(default)]`. `whisper_model` and `pen` are **`Option<String>` labels**, with the reason written on them at `:34-42`: *"a label this version doesn't know reads as the default rather than throwing the whole document away."*
- `StateFile { path: Option<PathBuf> }` is `:68-71`; `default_location` `:75-85`; `in_config_dir` `:87-91`; `config_dir`/`base_dir`/`cache_dir` `:183-208`. `path` is private.
- `Bus::spawn` takes a `StateFile` (see `crates/video-coach-harness/src/lib.rs:103-121`), so **do not change its signature**: add one accessor to `StateFile` for the directory it sits in.

**`PlanEntry`'s four construction sites**

`crates/video-coach-core/src/plan.rs:227`, `crates/video-coach-core/src/reel.rs:178`, `crates/video-coach-core/src/whole_match.rs:33`, `crates/video-coach-core/tests/audio.rs:125`. (The reshape touches none of them — this list is here so nobody "discovers" a fifth and thinks the spec was wrong.)

**Core's plan and schedule**

- `PlanEntry` is `plan.rs:72-93`; `source_index`'s doc (*"Index into `Project::source_videos`"*) is `:76-78`; `record_time` and its BACKLOG #27 comment are `:97-113`; `total_frames` (*"**the** denominator"*) is `:137-146`; `entry_chapters` is `:148-158`; `entry_text` is `:160-171`.
- `compilation_plan`'s per-clip loop is `plan.rs:216-237`: the source-duration fallback at `:217-221`, `playback_segments`, `frame_count`, and the `PlanEntry` at `:227-234`.
- `compilation_schedule` is `crates/video-coach-core/src/export.rs:125-140`; its clip lookup is `:130-137` and **`map_or(&[][..], …)` is the silent-identity-zoom default**. `walk` — already the shared unit — is `:143-152`.
- `audio_regions(compilation: &Compilation, prefs: &Preferences)` is `crates/video-coach-core/src/audio.rs:113-133`, with **eight** callers: `bus/export.rs:696`, `core/tests/audio.rs:73` and `:151`, `media/tests/export.rs:349`, `:456`, `:1162`, `:1316`, `:1517`. **None of them changes.** `Preferences::default()` is both volumes at 1.0 (`project.rs:95-108`).
- `ScoreboardContext::for_project` is `scoreboard.rs:642`; `config()` `:659`; `state_at(source_index, source_time)` `:669-672`; `source_offsets` `:637`.
- `highlight_shapes(highlights, source_index, t, zoom, w, h)` is `crates/video-coach-core/src/highlight.rs:424-431`; `PlayerHighlight::source_index` is `:85-89`.
- `metadata::match_name` is **private** at `metadata.rs:139-147`; `file_tags` `:111-130`; `final_score` `:169-178`; `team_keywords` (dedupes **within one project only**) `:181-193`; `UNTITLED` `:38`. The module's rule — *"Where a tag can't be told the truth it is left out rather than guessed"* — is `:16-19`.
- `Clip::recording_duration` is `project.rs:166`. `SourceRef::duration_seconds` is *"**the** duration authority"*, `project.rs:111-117`.

**The bus's export path**

- `start_run` is `bus/export.rs:313-387`. Order today: run check `:315-317`, preview check `:318-322`, no-project `:323-325`, empty-targets `:326-327`, labels `:331-335`, `de_duplicate` `:336`, one `job()` each `:337-347`, `create_dir_all(exports)` `:348-354`, **pickers written into `Preferences`** `:355-365`, start the first `:366-386`.
- `job()` is `:622-716`; its per-entry body — clip lookup, the `missing` check, the recording check, `EntryMedia` — is `:636-675`; the source list `:677-682`; `carry_scoreboard` `:683-691`; the `ExportJob` `:692-715`.
- `Open { folder, project }` is `bus/mod.rs:545-549`, and `folder` is documented *"Absolute and canonical, so source paths resolve against it directly"*; it is canonicalized in `commit` (`bus/project.rs:93-97`).
- `Bus::missing: Arc<[bool]>` is `bus/mod.rs:580`, published at `:345`, read by `job()` at `bus/export.rs:645`.
- `file_name(label, project_name)` is `bus/export.rs:190-196`; `de_duplicate` `:463-487`; `label` `:446-467`; `carry_scoreboard` `:513-593`; `source_date` `:583-604`.
- `Active`, `ExportRun`, `TargetState` and the run loop that must not change: `bus/export.rs:55-116`, `:199-292`, `:399-443`.

**Which refusals are notices, and which are modals**

`UserError::is_notice` (`bus/mod.rs:486-494`) returns true for **exactly** `Avatar`, `DeviceFallback`, `StopNotClean` and `Scoreboard`. So:
- **`CantExport` (`:465-467`) is a modal** — every export refusal, and every basket refusal at Start. The sheet needs its **own** message line because the status bar's notice (`ui/app.slint:4329`) renders behind the scrims (`:4381`, `:4403`, `:4497`).
- **"already in the basket" (spec V5) must therefore be a different variant** — a notice, following `Command::Transcribe`'s *"Does nothing if it is queued or running already"* (`bus/mod.rs:296-303`). Reuse `UserError::Scoreboard`'s *shape*, not that variant: add `UserError::Basket(String)` and list it in `is_notice`.
- `From<StoreError> for UserError` is `bus/mod.rs:497-510`; `TooNewProject` / `LegacyProject`'s wording (`:425-431`) **names no path**, which is why the basket wraps it (spec V6).
- The recording guard is `bus/mod.rs:803-836` and **drops** an unlisted command with `return eprintln!("bus: refused while recording: {cmd:?}")` — it does not queue it.

**The UI**

- `Scrim` `ui/app.slint:1377-1383`; `Sheet` `:1392-1428`; the four card widths `:1454` (export, 480), `:1952` (setup, 520), `:2229` (match editor, 640), `:4499` (error, 440).
- `text-editing` is `app.slint:2832`; `handle-key`'s guard order is `:3044-3110` (error dialog `:3045`, export sheet `:3054`, setup sheet `:3068`, the match editor's, `text-editing` yield `:3108`); the Esc cascade is `:3280`.
- The clip row's menu is `app.slint:3509-3523` (*Jump to clip start / Preview clip / Export video… / Delete clip*); the clip list's drag-reorder is `:3478-3491`.
- The bottom bar's **Export…** is `app.slint:4140-4152`.
- A list with a height of its own: `height: min(…)` at `app.slint:962`, `:1461`, `:1520`, `:2245`.
- The export sheet's run rows are `app.slint:1513-1525`.

**glib**

`glib::user_special_dir(glib::UserDirectory::Videos) -> Option<PathBuf>` exists in glib 0.22.9 (the workspace's). `video-coach-app` already depends on `gstreamer::glib` (`bus/export.rs:33`). `core` must not.

**Tests**

- Media: `crates/video-coach-media/tests/{export.rs,copy.rs,job.rs,avatar.rs}`.
- Harness: `crates/video-coach-harness/tests/{export.rs,whole_match.rs,reel.rs,project_and_sources.rs}`. `Harness::new(config_dir)` is `harness/src/lib.rs:44-52`; `Harness::production` `:86-101`; the `StateFile::in_config_dir(&dirs.config())` pattern is `tests/project_and_sources.rs:109`.
- Core: `crates/video-coach-core/tests/{plan.rs,export.rs,metadata.rs,scoreboard.rs,chapters.rs,audio.rs}`.

---

## Task 1: The `EntryMedia` reshape — and every existing export working unchanged

The whole feature's leverage. **It ships no basket**: at the end of this task the app does exactly what it does today, through a different shape, and its own proof is that the existing media and harness export tests pass without a behavioural edit.

**Files:**
- `crates/video-coach-media/src/composite/{export.rs,audio.rs,copy.rs}`, `crates/video-coach-media/src/lib.rs`
- `crates/video-coach-app/src/bus/export.rs`
- `crates/video-coach-media/tests/{export.rs,copy.rs,job.rs,avatar.rs}`

**What to build:**

1. **The types** (spec J1), replacing `ExportJob::sources` and `Encode`'s `scoreboard` / `highlights` / `avatar`:
   - `Render::Copy(Vec<PathBuf>)` — the files to join, in entry order.
   - `Encode { entries: Vec<EntryMedia>, audio, resolution, quality }`. **`entries` loses its `Option`**: every entry has a file and a match even when it has no clip.
   - `EntryMedia { source: PathBuf, clip: Option<ClipMedia>, match_media: Arc<MatchMedia> }`.
   - `ClipMedia { recording: PathBuf, clip: Clip }` — today's `EntryMedia`, renamed.
   - `MatchMedia { scoreboard: Option<ScoreboardContext>, highlights: Vec<PlayerHighlight>, avatar: Option<PathBuf> }`, carrying `ScoreboardContext`'s "never cache across a source change" warning.
   - Doc on `EntryMedia::source`: **the file, not an index** — `PlanEntry::source_index` stays the project-local index the board and the highlights are keyed by, and nothing in media maps it.
2. **The three readers** (see Known facts): `export.rs:561-566` and `audio.rs:104-108` read `encode.entries[…].source`; `copy.rs`'s `files()` is **deleted** and `run` takes `Render::Copy`'s own list. `audio.rs`'s commentary arm reads `entries[…].clip.as_ref().map(|c| c.recording.clone())`.
3. **The board, the highlights and `media` in the loop** read through `encode.entries[frame.entry].match_media` (spec J3, J4). The BACKLOG #27 comment at `export.rs:591-593` **stays**, word for word: it is the reason the call is `state_at(entry.source_index, frame.source_time)` and not a per-clip constant.
4. **The decoder cache: keyed by path, and bounded** (spec J2).
   - `HashMap<PathBuf, Decoder>`.
   - At each entry change, drop the outgoing entry's source **unless some entry at or after the new one reads it**, applying the mixer's rule (`audio.rs:144-158`) and citing it in the comment, including *"a compilation of two hundred clips would hold two hundred pipelines at once"*.
   - **Move `Rendered::diagnostics` to where the first entry's decoder is opened** (`export.rs:646-651` reads it after the loop, where it may be gone). Keep it the *first* entry's, so the logged line means what it means today.
5. **The avatar: per distinct image, gated per entry** (spec J5).
   - One pre-pass over `encode.entries` opening `HashMap<PathBuf, AvatarInset>`, **keyed on the path** so two matches sharing an image hold one texture.
   - The condition, in **one** expression used by both the pre-pass and `pulse_levels`: `media.clip.as_ref().is_some_and(|c| c.clip.shows_avatar()) && media.match_media.avatar.is_some()`.
   - **Delete the per-job `match avatar { Some(_) => … }` gate** (`export.rs:533-536`): with the avatar path in the per-entry condition it earns nothing.
   - `Pip::open` takes the entry's own inset. The **GL** filler rule (`export.rs:17-21`) is untouched and the comment gains a line: a basket alternates fed and filler pads between matches as the normal case.
6. **The bus's `job()`** builds the new shape (`bus/export.rs:622-716`): one `Arc<MatchMedia>` for the open project, cloned into every entry; the entry list built once; `Render::Copy(entries.iter().map(|e| e.source.clone()).collect())` where it copies. **`copy_files` is deleted** — the list comes from entries a missing source was already refused in, so there is nothing to `filter_map` away. `carry_scoreboard` takes that list.
7. **Nothing in `core` changes.** If a change to `core` looks necessary, the design is wrong — stop and say so.

**Test that must fail first:** `two_matches_draw_their_own_boards` in `crates/video-coach-media/tests/export.rs` — an `ExportJob` whose two entries carry **two different `Arc<MatchMedia>`**, each with its own `ScoreboardContext` over its own project's kick-off, and two fixture sources of **different resolutions**. Assert the output is 1920×1080@30, that the entry boundary re-letterboxes, and that a frame decoded either side of it reads a **different** burned score (the Phase 9 scoreboard-pixel tests are the model). It cannot compile against today's `Encode`, and once it does it fails behaviourally against a single `scoreboard` field.

Then:
- `one_avatar_image_shared_by_two_matches_opens_one_inset`, and a piece whose match has no avatar getting the GL filler with no stall (spec J5).
- `a_decoder_is_dropped_when_no_later_entry_reads_it`: three entries, the middle one on a different file; the cache holds at most two, the first file is reopened for the last entry, and a **single-file** compilation opens exactly one decoder and never drops it. Assert on the cache, never on a timing.
- `the_diagnostics_survive_a_dropped_first_decoder`: `Rendered::diagnostics` still names a decoder after such a run.
- `an_entry_whose_source_is_missing_fails_and_leaves_no_part`.
- **`Render::Copy(files)` still joins a whole match** — `tests/copy.rs` green, and one test asserting the list it was handed is the entry order.
- **Every existing test in `media/tests/{export.rs,copy.rs,job.rs,avatar.rs}` and `harness/tests/{export.rs,whole_match.rs,reel.rs}` passes with fixture construction changed and no assertion changed.** If an assertion has to change, the reshape is not behaviour-preserving and the task stops.

**Verify:** the gate. Plus: `cargo test -p video-coach-core` must pass **untouched**, and the core dependency audit must still list exactly `serde`, `serde_json`, `thiserror`, `uuid`.

**CLAUDE.md:** nothing yet — Task 4 writes the paragraph this feature earns. (Task 3 adds the one line the export path needs about `begin`.)

Commit: `refactor(media): the match's record hangs off the entry, not the job`.

---

## Task 2: Core's basket plan

Pure, so it runs on CI with no GStreamer, and it is where most of this feature's tests live.

**Files:**
- `crates/video-coach-core/src/{plan.rs,export.rs,metadata.rs}`
- `crates/video-coach-core/tests/{plan.rs,export.rs,metadata.rs,chapters.rs}`

**What to build:**

1. **`clip_source_duration(&Project, &Clip) -> f64`** in `plan.rs`: the source's `duration_seconds`, or the documented fallback `start_source_seconds + recording_duration`. Lifted from `compilation_plan`'s loop (`plan.rs:217-221`) and called by it, so **the** duration authority has one reader-facing name for both builders.
2. **The extracted per-clip entry.** One private function taking `(clip, source_duration, start_frame, text)` and returning a `PlanEntry` — `playback_segments`, the quantized `frame_count`, the fields. `compilation_plan`'s loop calls it with `entry_text(clip, i+1, count)`; `basket_plan` with the basket's three-part line. **No other extraction**: `walk` is already the shared unit (spec J7).
3. **`BasketPiece<'a> { clip: &'a Clip, source_duration: f64, match_label: String }`**, with the doc saying the caller resolves all three and refuses what it can't find, because it is the one that can name what is missing.
4. **`basket_plan(pieces: &[BasketPiece]) -> CompilationPlan`** — entries 1:1 with pieces in order, `start_frame`s the running quantized sum, `chapters` from the existing `entry_chapters`. **No `&[&Project]` parameter**: the pieces carry everything.
5. **The three-part line** (spec T1, T3): `"<match> | <clip name> | tags"`, empty parts dropped, **no position**. Document *why* on the function: the bar ellipsizes rather than shrinks (`overlay.rs:226-232`), so the first part spends the safe end of the line, and the position — which the coach said means nothing across matches — is in the chapters instead. Without that comment the next reader adds `"n / total"` back.
6. **`basket_schedule(pieces: &[BasketPiece]) -> Compilation`** in `export.rs`: `basket_plan`, then one `walk` per entry over **`pieces[i].clip.events` directly**. No lookup by `clip_id`, so no miss can degrade to identity zoom the way `compilation_schedule:130-137`'s `map_or(&[][..], …)` can.
7. **`metadata::basket_tags(name: &str, matches: &[&Project]) -> FileTags`** (spec O4): title the basket's name; description counting pieces and matches; **`comment` empty** (no one result to state); **`date` `None`** (no one footage date); keywords **deduped across matches** — a second pass over the per-project lists applying `team_keywords`' own two rules (blank dropped, duplicate collapsed) one level up. Say on it that this is new behaviour, not a call into `team_keywords`, which dedupes within one project only.
8. **`match_name` becomes `pub`** (`metadata.rs:139-147`).
9. **`audio_regions` is not touched**, and neither is any of its eight callers (spec J6).

**Test that must fail first:** `a_pieces_clip_is_the_only_source_of_its_events` in `crates/video-coach-core/tests/export.rs` — two pieces from two different projects, the first's clip carrying a zoom event and the second's carrying none. Assert the first entry's frames zoom, the second's are identity, and **that swapping the projects' clip ids changes nothing** (the id is never consulted). It fails to compile on an empty module, then fails behaviourally the moment the schedule looks a clip up rather than reading the piece's.

Then, all behavioural:
- `basket_plan`: entry order is piece order; each entry's `source_index` is its own clip's; `start_frame`s are the running quantized sum; `total_frames` is the last entry's end.
- **A piece from match B keeps B's `source_index` even when A has more sources** — the regression a flat merged list would cause.
- `clip_source_duration` both ways: a present source gives `duration_seconds`, a missing one the fallback. And `compilation_plan` is unchanged by the extraction (its existing tests prove it; add none).
- The line: all three parts; a clip with no tags; an unnamed clip; a project with no scoreboard falling back to `name` and then to `UNTITLED`; **no position anywhere in it**.
- `chapters` is one per piece at `start_frame / 30`, titled with that line, and **empty for a one-piece basket**.
- `basket_tags`: no comment, no date, the title, the description's counts, and keywords over three projects — `Rovers` appearing twice yields one, a blank name yields none.
- **The clock test, which is the point of the feature:** two projects whose kick-offs are tagged differently; `ScoreboardContext::for_project` each; assert a frame of piece 1 and a frame of piece 2 at the same *output* time read their own match's clock and score. The existing Phase 9 pause test is the model, and it must keep passing.

**Verify:** the gate. `cargo test -p video-coach-core` alone must pass with no GStreamer, and the dependency audit must still list exactly the four crates.

Commit: `feat(core): a plan whose pieces come from several matches`.

---

## Task 3: The bus — the list, its file, the commands, and the run split

**Files:**
- `crates/video-coach-app/src/bus/basket.rs` (new), `crates/video-coach-app/src/bus/{mod.rs,export.rs,state.rs}`
- `crates/video-coach-harness/tests/basket.rs` (new), `crates/video-coach-harness/tests/export.rs`
- `CLAUDE.md`

**What to build:**

1. **`basket.json`, beside `state.json` and never inside it** (spec H1, H2).
   - `StateFile` gains one accessor — `pub fn sibling(&self, file: &str) -> Option<PathBuf>` — so **`Bus::spawn`'s signature does not change** (the harness and `main.rs` both call it: `harness/src/lib.rs:103-121`).
   - `BasketFile { path: Option<PathBuf> }` in `basket.rs`, read and written whole, with the `.json.tmp`-then-rename `state.rs:181-190` uses, and every failure logged and otherwise ignored.
   - **Resolution and quality are read as labels**, with two private `label`/`from_label` pairs, and an unknown label reads as the **default** so it cannot take the pieces with it. Write the reason on them, citing `state.rs:34-42` and `state.rs:158-169`.
   - Every field `#[serde(default)]`. A malformed `pieces` list reads as an empty basket, logged.
2. **`project_for(&self, folder: &Path) -> Result<Cow<Project>, UserError>`** (spec E3a, V6): the open project **in memory** when `folder == open.folder` (which is canonical, `bus/mod.rs:545-549`), otherwise `store::read`, with **every** store error wrapped into one shape naming the folder: `can't export: the project at <folder> can't be read: <the store error's own sentence>`. Say on it why `From<StoreError>`'s `TooNewProject` is not used here: it names no path (`bus/mod.rs:425-431`).
3. **The six commands** (spec C1), in `bus/basket.rs`, dispatched from `Bus::command`: `AddToBasket { clip_id }`, `RemoveFromBasket { index }`, `MoveBasketEntry { from, to }`, `ClearBasket`, `ShowBasket`, `ExportBasket { name, resolution, quality }` — **no folder field**. None joins the recording allow-list (`bus/mod.rs:803-836`), which is deny-by-default, so that is a thing *not done*: the task must not add an arm.
4. **`UserError::Basket(String)`**, listed in `is_notice` (`bus/mod.rs:486-494`), for **V5**'s `already in the basket`. Start's refusals stay `CantExport`, which is a modal.
5. **`start_run` splits** (spec C3), and the run loop does not change (`bus/export.rs:55-116`, `:199-292`, `:399-443`):
   - `fn refuse_if_busy(&self) -> Result<(), UserError>` — a run going, a preview open. **`begin` calls it, and each builder calls it first**, before any I/O: the duplicated *call* is what keeps a refused Start from reading three projects, and `begin`'s call is what stops a caller skipping it.
   - `fn begin(&mut self, jobs: Vec<(String, ExportJob)>) -> Result<(), UserError>` — **no output directory**. Each caller creates its own after its own refusals and before calling this.
   - **The project's `Preferences` are written back only after `begin` returns `Ok`** (today the write-back at `:355-365` is correct only by ordering). The basket's own file is written on every change, a refused Start included: it is the sheet's memory, and there is no project to dirty.
6. **`entry_media(...) -> Result<EntryMedia, UserError>`** (spec C3a): `job()`'s per-entry body (`bus/export.rs:636-675`) factored out and called by **both** builders, with a `whose: &str` prefix — empty for an export, `"Rovers v Athletic — "` for a basket piece. It **stats** the source and the recording rather than reading `Bus::missing`, which says nothing about a closed project (spec V2); `Bus::missing` keeps its UI job. One function, one set of sentences, identical by construction.
7. **The basket's job builder**: `refuse_if_busy`; refuse an empty basket; `project_for` each distinct folder once; one `Arc<MatchMedia>` per project; `basket_schedule` over the pieces; `entry_media` each; `basket_tags`; `cues: None`; the output path (below); `create_dir_all`; `begin(vec![(label, job)])`.
8. **The output path** (spec O1): `<XDG Videos>/Coach Cuts/` via `glib::user_special_dir(UserDirectory::Videos)`, falling back to `$HOME/Videos/Coach Cuts` and then the current directory. The name is **trimmed**, empty becomes `Basket`, `/` and `:` replaced as `file_name` does (`bus/export.rs:190-196`), and an existing file gets ` (2)`, ` (3)` — **never a silent overwrite**, because a basket's name is typed over changing contents where an export's is derived. The message line names the file written when it was suffixed.
9. **`Event::Basket(BasketView)`** (spec C4), published at `Bus::spawn`, on every mutation and on `ShowBasket`, and **not** on `ProjectChanged`. `BasketRow::seconds` is **`Clip::recording_duration`** (`project.rs:166`) — no plan is built per row.

**Test that must fail first:** `a_refused_start_changes_no_project` in `crates/video-coach-harness/tests/basket.rs` — two projects, a piece from each, the second's game video deleted. `ExportBasket` refuses **naming that match and clip**; then assert the open project's `project.json` is byte-identical to before, **no `ProjectChanged` was published**, and no file exists in the output folder. It fails today because the pickers are written before the per-entry refusals can be reached from a basket path at all.

Then:
- The happy path: open A, `AddToBasket`; open B, `AddToBasket`; `ExportBasket` → one `ExportRun` with one target, progress events, `TargetState::Done`, the file, its `.chapters.txt`, and **no `.srt`**.
- The basket **survives a project open** (`Event::Basket` after opening B still lists A's piece with A's match label) and **survives a bus restart** (write `basket.json`, spawn a fresh bus — `project_and_sources.rs:109`'s `StateFile::in_config_dir` pattern is the model).
- Each refusal in spec **V1**'s table, each naming its piece, each leaving no file — including a project whose `formatVersion` is bumped past `CURRENT_FORMAT_VERSION`, whose message **must name the folder** (spec V6).
- **A piece of the open project resolves from memory**: rename a clip and the run's text bar carries the new name in the same command sequence (spec E3a).
- `MoveBasketEntry` and `RemoveFromBasket` reorder and shorten the run's entries.
- A basket run cancels like any other; `AddToBasket` for a clip already in it changes nothing and emits the **notice**.
- In `basket.rs`'s own `#[cfg(test)] mod tests`: the file round-trips; **an unknown resolution label reads as the default and the pieces survive**; a corrupt file reads as empty; and **writing the basket does not touch `state.json`** — set the pen and the last project first, write the basket, read them back (the test that pins why this is not a key in that file).
- The output name: trimmed, empty → `Basket`, `/` and `:` replaced, an existing `Corners.mp4` giving `Corners (2).mp4` then `(3)`.
- **`harness/tests/export.rs` gains one test for the write-back's new position**: an ordinary `Command::Export` refused for a missing source leaves `Preferences` untouched.

**Verify:** the gate.

**CLAUDE.md:** one line under the export rules — *a run is started by `begin(jobs)` over jobs the caller built; the caller owns its own refusals, its own output directory and its own picker write-back, and the write-back happens only after `begin` returns `Ok`*. The rest waits for Task 4.

Commit: `feat(app): the basket, and a run started from jobs the caller built`.

---

## Task 4: The sheet

**Files:**
- `crates/video-coach-app/ui/app.slint`
- `crates/video-coach-app/src/main.rs`
- `CLAUDE.md`

**What to build:**

1. **`BasketSheet inherits Sheet`** at `card-width: 520px` (spec U3) — the setup sheet's width, because the widest thing in it is a row's `"Rovers v Athletic — Corner, 2nd half"`. Wrapped in `Scrim` like the other four (`app.slint:1377-1383`). Top to bottom: the piece list, the **name** field with placeholder `Basket` plus the **Resolution** and **Quality** pickers, the run row, one **message line**, then **Clear**, **Start**, **Close**. **No output-folder row and no Change… button** (spec O1).
2. **The piece list**: one row per entry — `↑` `↓`, the match, the clip's name, its length, `✕`. A row with a `problem` is drawn in `alternate-foreground` showing it. A height of its own in the house idiom (`app.slint:962`, `:1461`, `:1520`, `:2245`) so twenty pieces scroll rather than growing the sheet past the window. `↑`/`↓`, **not drag**: the clip list's `DragArea`/`DropArea` machinery (`app.slint:3478-3491`) is real, and a five-row modal list does not earn a second copy (spec U6).
3. **The run row** renders `Event::Export`'s one target, as `ExportSheet` does (`app.slint:1513-1525`) — from the bus's events, never from its own state.
4. **The key guard**, placed with the other sheets (`app.slint:3044-3110`), taking the setup sheet's shape: Esc closes when `!text-editing`, then `return reject` so everything else reaches whatever has focus. **And `text-editing` at `app.slint:2832` gains `|| basket.editing`** — without it the `!text-editing` test is always true, the first Esc closes the sheet from inside the name field, and the Esc cascade at `:3280` is dead code.
5. **"Add to basket" in the clip row's menu** (`app.slint:3509-3523`), beside "Export video…", acting on the row it was opened on, not the selection, under the menu's own `enabled: !root.recording`.
6. **The bottom bar's button** beside Export… (`app.slint:4140-4152`): **"Basket (4)…"**, or "Basket…" and disabled when empty. It does **not** require an open project, and stays enabled during a run so the run can be watched and cancelled. `keys.focus()` first, as the other sheet buttons do.
7. **The message line** (spec C6): refusals **and** the file the run wrote when the name was suffixed. The status bar's notice (`app.slint:4329`) renders behind the scrims, so it is invisible until Close — that is the whole reason this line exists. Cleared on close and on the next command.
8. **The sheet renders `Event::Basket` and nothing else** (spec C4), seeded on open the way `open_match_setup` seeds the setup sheet.

**Test that must fail first:** there is no headless test for a Slint sheet, and the plan does not pretend otherwise — Task 3 pins every string and every refusal, Task 2 every line of the bar. **The failing-first artefact is a screenshot pass**, written and run before the sheet exists (`XDG_CONFIG_HOME` pointed at a scratch dir, per CLAUDE.md):
- two scratch projects, four pieces, one of them dead;
- the sheet open, with the name field, both pickers and the dead row's reason;
- a refusal on the message line;
- the bottom-bar button reading `Basket (4)…` from another project.

Capture all four; the first run cannot produce them, which is the failure.

**Hands-on (batched, the coach's eyes — Task 5 collects them):** the Esc order, the greying during a recording and a preview, and the four items in the spec's *"What needs the coach's eyes"*.

**Verify:** the gate, plus the screenshot pass.

**CLAUDE.md:** one paragraph after the match-clock rules:
- **the basket is a list of `(project folder, clip id)` references the app holds across projects**, in `$XDG_CONFIG_HOME/coach-cuts/basket.json` — **its own file, not a key in `state.json`**, because `StateFile::read` discards that whole document on any parse error and every setter rewrites it, so one bad basket value would lose the last project, the pen and the speech model;
- **each piece draws its own match's board and clock**, because the match's record hangs off `EntryMedia` and the clock is still `state_at(entry.source_index, frame.source_time)` per frame — `PlanEntry` knows nothing about matches, and `source_index` is still project-local;
- **the export's decoders are bounded**: one per distinct file, dropped as soon as no later entry reads it, the audio mixer's own rule — and the zero-copy diagnostic is therefore taken when the first decoder opens, not after the loop;
- **a basket's text bar is three parts** (`<match> | <clip> | tags`): the bar ellipsizes rather than shrinks, so the position would spend the safe end of the line on the part that means nothing across matches; it lives in the chapters;
- **the basket mixes at the default volumes** — a project's preview volumes are a scanning convenience, and a film whose level jumps between pieces for an invisible reason is worse than one that doesn't;
- **a run is started by `begin(jobs)`**: the caller owns its refusals, its output directory and its picker write-back, and writes the pickers only after `begin` returns `Ok`.

Commit: `feat(app): the basket sheet`.

---

## Task 5: Closeout

1. **Adversarial review** of the whole diff (the `adversarial-review` skill, CLAUDE.md's pattern). Apply, skip or defer. Pay particular attention to Task 1's reshape, which is the largest diff and the one in the hot loop.
2. **Backlog** what is deferred, in `BACKLOG.md`'s format — the spec's whole Deferred list, plus:
   - **#86 is updated**: its *"revisit after #77"* ordering is **void**, and #77 now inherits `begin(jobs)` and `refuse_if_busy()`, which is most of its own *"don't tie the run loop to the open project"*. Both entries say so.
   - **#78 gains the output folder** (spec O1), as an export setting whose home is decided once for exports and baskets together.
   - **Per-piece gain** (spec J6), marked *after #52*, which is still "no UI for the source and commentary volumes".
3. **`docs/hands-on-checklist.md`:** a section in the checklist's own voice:
   - **[must work] Gather across three matches.** A clip in each of three projects, added as you go; the badge reads `Basket (3)…` from the third project; Start; one file in `~/Videos/Coach Cuts`.
   - **[must work] The clock and the board change with the piece.** Scrub to each cut: the score, the team names and the running clock are that match's, and the clock agrees with the footage either side of a pause in the middle of a piece.
   - **The text bar.** Is `"Rovers v Athletic | Corner, 2nd half | corners"` readable at 1080p, or does the ellipsis still eat the clip name? **Your call**, and changing it is one function in `plan.rs`.
   - **Is the position missed?** It is in the chapter list now, not on the picture.
   - **The joins.** Two matches shot differently, cut together with no fade. Acceptable, or does a basket want a dip to black?
   - **The sound.** A piece from a project whose scan volume you keep low: the film plays it at full, by decision. Right or wrong?
   - **A dead piece.** Rename a project folder and press Start: the refusal names the folder, and nothing is written.
   - **A repeat.** Start twice with the same name: the second is `… (2).mp4` and the message line says so. Litter, or useful?
   - **Keys and gating.** Esc leaves the name field, then closes the sheet; the button is greyed during a recording; Add to basket is greyed during a recording.
   - **A restart.** Quit with four pieces waiting and relaunch: they are still there, with their match labels.
4. **Check each task's CLAUDE.md addition is there** and still accurate, and that Task 3's `begin` line and Task 4's paragraph do not say the same thing twice.

Commit: `docs: close out the clip basket` (along with the review's fixes, in their own commits).

---

## The user's own steps

1. **Install the build that carries this** (there is no release point of its own in this plan; it rides whatever version ships next) and run the checklist section above, using **copies** of three tagged projects — the basket only reads them, but Start resolves live footage and a relink is a real edit.
2. **Watch one three-match film end to end** before trusting the feature: the bar, the boards, the joins and the level, in that order.
3. **Answer the six open questions** if any of the defaults grate. The two most likely to are the three-part bar (is the position missed?) and the ` (2)` suffix.

## Deliberately not in this plan

- Everything in the spec's Deferred list: the library view over old projects, a tag or reel or whole match as a piece, per-piece gain, an output-folder preference, a cue-track basket, a transition between pieces, drag reordering, a key for Add, the basket as a saved document, music, and queuing a basket behind other exports.
- **`PlanEntry::match_index`, `Encode::matches`, and any other media coordinate in `core`.** Spec J1 says why; a task that reaches for one has taken a wrong turn.
- **Any change to `core::audio::audio_regions`** or to its eight callers (spec J6).
- **A generified `compilation_schedule`** (spec J7). `walk` is already the shared unit.
- **A format change.** `CURRENT_FORMAT_VERSION` stays 11 (spec F1); no task touches `store.rs`, and no task adds a field to `state.json` (spec F2).
- **A second `ExportTarget` variant, a second `Render` variant, or a second run loop** (spec J8, C3).
