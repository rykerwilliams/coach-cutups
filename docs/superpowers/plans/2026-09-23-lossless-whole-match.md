# The lossless whole match: plan

**Date:** 2026-09-23
**Spec:** `docs/superpowers/specs/2026-09-23-lossless-whole-match-design.md` (decisions L, T, U, M, X, N, E), reviewed and applied on 2026-09-23. It carries no open questions: the user settled the picker and `Preferences`, and the simplify and correctness reviews settled the rest.
**Status:** not started.

**Scope.** Five tasks: the cue list and the mode in core (format v11), the copy graph in media, the sidecar in media, the picker and the wiring in the app, then the closeout and the build the user checks their own match with. Nothing here needs a GPU, an encoder or the network.

**Execution.** A fresh subagent per task (`superpowers:subagent-driven-development`), given this plan, the spec and `CLAUDE.md`.
- Tasks run one at a time, in the order written, in one tree. The cargo lock serialises every build anyway.
- The orchestrator runs the gate below and commits each task. It stages paths explicitly, never with `git add -A`.
- **At the end of every task the workspace builds and every test passes.** A task that changes a type fixes every user of it in the same task, in every crate.
- Each task writes its own `CLAUDE.md` addition, where it names one.

**The gate, for every task:** run the `verify` skill, with every cargo call under the machine-wide lock (`flock /tmp/claude-1000/cargo.lock nice -n 19 cargo …`), because other sessions build here too.

**Test-first.** Each task names the test that must fail first. Write it, run it and watch it fail for the stated reason, then build until it passes. A test that fails only because the code doesn't compile yet counts, but the plan names a behavioural failure wherever one exists.

**What tests may touch.** No test reaches the network, the real camera or mic, or the user's footage. Every media test here runs on generated fixtures. Real footage and anything that identifies a team or player is never committed: the repository is public and the footage shows children. That includes file names, team names and shirt numbers, in code, tests, commit messages and docs.

---

## Known facts. Don't re-derive these.

**Measured on this machine on 2026-09-23** (the spec's evidence; the user's two-file match, header reads only):

- **The copy is 2.10 GB in 26.6 s** (9.8 s from page cache), against 7.9 GB in about an hour re-encoded.
- **It is lossless:** 97,684 output video packets = 48,697 + 48,987; 152,642 audio packets = 76,094 + 76,548. Every packet byte-identical **but one** — the second half's first keyframe grows 34 bytes, which is `h264parse` writing the parameter sets in-band at the resync point. Duration 3256.459 s against an expected 3256.4587 s. Container overhead of the join: **+134 KB**.
- **`queue` after every demux pad and before every mux pad is not optional:** without them the graph deadlocks on the first file (`qtdemux`'s one streaming thread pushes video into an aggregator waiting on that same thread's audio).
- **With `reserved-max-duration` set, the copy's boxes are `ftyp / free / moov / free(4.72 MB) / uuid / free(8) / mdat`** — the layout `chapters::splice` needs. The reserve costs ~10.6 MB on a 54-minute match; the `moov` came to 1.22 MB against 5.4 MB reserved.
- **Concatenating 320×240 and 640×480 H.264 produced no error and no warning** — one file, one `stsd`, describing most of the samples wrongly. **This is why the caps gate exists**; nothing downstream refuses for us.
- **`mp4mux` writes a zero-length sample after every text cue:** 3,400 one-second cues came out as 6,799 samples. So pushing cues ourselves buys nothing, and the embedded track is deferred.
- **VLC 3.0.20** lists an embedded `tx3g` track (`adding track[Id 0x3] subtitle (enable)`) but builds no decoder for it; a matching-basename sidecar is auto-detected (`autodetected subtitle: …/side.srt with priority 4`) and shown without being asked.
- **The A/V delta is 9 ms** (audio 3256.450 s, video 3256.459 s) and it is **each source's own video-vs-audio track-duration delta**, offset independently by the two `concat`s — **not** AAC priming, and it **accumulates per source** (~4.5 ms each here).
- **A 54-minute match is ~3,256 cues, ~194 KB of `.srt`.**
- **Plan time vs file time:** entry 2 starts at `ceil(1623.3955 × 30)/30 = 1623.400 s` against 1623.3955 s — 4.5 ms, bounded at one frame per source. A whole-second clock cannot show it.
- **GStreamer 1.24.2 here:** `mp4mux` has per-pad `trak-timescale`, `reserved-max-duration`, `reserved-bytes-per-sec` and **`reserved-duration-remaining`** (a readable property — that is what L4's log line reads). `concat` has `adjust-base`, default true.

**Checked in the code while writing this plan:**

- **`probe::Probe` returns `duration_seconds` and `display_aspect`, and nothing else** (`crates/video-coach-media/src/probe.rs:19-25`). It never returns caps. The compatibility gate therefore reads the copy graph's own pads; there is no second `Discoverer` pass.
- **`ExportJob` is at `crates/video-coach-media/src/composite/export.rs:68-102`**, `scoreboard: Option<ScoreboardContext>` at `:93`.
- **Media reads `job.scoreboard` in exactly two places, both the per-frame overlay state:** `composite/export.rs:381-384` and `composite/preview.rs:443`. Nothing else in media touches it — not the chapters, not the entry text.
- **Chapter titles and entry text come from core's plan, not the job:** `CompilationPlan::chapters` (`crates/video-coach-core/src/plan.rs:100`), built by `entry_chapters` (`plan.rs:116-118`) or `whole_match_chapters` (`crates/video-coach-core/src/whole_match.rs:64`).
- **Fewer than two entries means no chapters** (`plan.rs:117`), and `whole_match_chapters`'s one-per-source fallback has the same guard (`whole_match.rs:83`) — but **tagged** events are returned whatever their number (`whole_match.rs:76-82`). So a single-source copy gets its tags' chapters, and none at all when nothing is tagged.
- **`whole_match_entries` filters out a source with no usable duration** (`whole_match.rs:23-31`), which is why the copy iterates `plan.entries` and not `project.source_videos`.
- **`run` owns the `.part` contract**: `part_path` (`composite/export.rs:262-267`), export → rename → delete-on-error (`:270-287`), `chapters::splice` before the rename (`:437`), `ExportDone` built once (`:439`).
- **`chapters::splice(path, &[(f64, &str)])`** is at `crates/video-coach-media/src/chapters.rs:157` and needs no change.
- **`ExportDone`** (`composite/export.rs:132-143`) is constructed in exactly one place (`:439`). Its readers: the `bus: exported …` line (`crates/video-coach-app/src/bus/export.rs:258-269`) and `crates/video-coach-media/tests/export.rs` — the two helper signatures at `:83` and `:103` and the `done.chapters` assertion at `:1679`. **Those are the three test sites** a new field touches.
- **`Preferences` carries `#[serde(default)]` on the container** (`crates/video-coach-core/src/project.rs:73-75`) and fills from a hand-written `Default` impl (`:91-104`). `project.rs`'s module comment (`:6-18`) forbids the field-level form. **The new field takes no attribute.**
- **`CURRENT_FORMAT_VERSION = 10`** (`crates/video-coach-core/src/store.rs:21`), **`MIN_READABLE_FORMAT_VERSION = 7`** (`:26`). `read` refuses anything above current as `TooNew`, and `write` keeps a one-time `project.json.v<old>` backup.
- **The two export pickers are written back in `bus/export.rs:375-378`,** only when they changed. The third joins them there.
- **`ExportTargetRow` keeps `count`, `unit`, `seconds`** (`bus/export.rs:122-136`); `export_targets` is at `:145`, and the whole match's `"video"` unit at `:153`. **The UI builds the detail string** (`crates/video-coach-app/src/main.rs:646-655`). **Nothing about the rows changes in this plan**, so `crates/video-coach-harness/tests/reel.rs:149` and `:157` (`(count, unit)` assertions) stay as they are. The detail rework the simplify review proposed was dropped: `SourceRef` stores duration and display aspect, never width and height.
- **`file_name` replaces `/` and `:`** (`bus/export.rs:216-219`) and **`de_duplicate` suffixes `" (2)"`** (`:484`). `job.path.with_extension("srt")` inherits both.
- **The rate window is the run's, not the target's:** `Active.rate` (`bus/export.rs:235`), sampled at `:436`, and `RateWindow` keeps a 5-second window with a minimum sample count and span (`crates/video-coach-core/src/export.rs:65-92`). `Active::finish_target` is at `bus/export.rs:248`.
- **`ScoreboardContext::for_project` is built once per run** at `bus/export.rs:574`, and frozen (Phase 9).
- **`Command::Export { targets, resolution, quality }`** (`crates/video-coach-app/src/bus/mod.rs:251-255`, dispatched at `:846-850`).
- **The sheet's two ComboBoxes** are at `crates/video-coach-app/ui/app.slint:1365-1385`, their properties at `:1963-1964`, wired at `:3491-3492`; `main.rs:616-627` reads them when Export starts, and `main.rs:669-694` sets them from `Preferences` when the sheet opens.
- **`CounterKind::H264Mp4BFrames` has no audio track** (`crates/video-coach-media/src/fixtures.rs:297-305`, and the assert at `:423-424`). The copy's audio `concat` needs a fixture that has one; `counter_video_with` is at `:380`.
- **`packaging/smoke-test.sh:60-69`** is the element list. `concat` is not in it.

---

## Task 1: The cue list, the mode, and format v11

Pure core. Nothing in it needs GStreamer, and `video-coach-core` gains no dependency.

**Files:**
- `crates/video-coach-core/src/{cues.rs (new),lib.rs,project.rs,store.rs,plan.rs}`
- `crates/video-coach-core/tests/{cues.rs (new),project_format.rs}`

**What to build:**

1. **`cues.rs`: `Cue { start: f64, end: f64, text: String }`** and **`scoreboard_cues(&Compilation, &ScoreboardContext) -> Vec<Cue>`** (spec U1).
   - For each output frame `n`: `plan.entries[frames[n].entry].source_index` and `frames[n].source_time` into `ScoreboardContext::state_at`, then the **T2** line: `"{home} {home_score} - {away_score} {away} · {clock}"`, where `{clock}` is `format_clock`'s `main` plus its `trailing` when in stoppage, `HT`/`BREAK` on a break and `FT` after the last period.
   - **Run-length-encode** the rendered line. A run from frame `a` to `b` is a cue from `a / OUTPUT_FPS` to `(b + 1) / OUTPUT_FPS`.
   - **`state_at` is called per frame** — CLAUDE.md's Phase 9 rule, not an optimisation to skip. A frozen entry reads the same state either side of the pause, and the run-length encoding is what turns that into one long cue instead of a running clock.
   - `None` from `state_at` ends the current run and starts no new one: a gap, which is what "no match yet" means.
2. **`cues_to_srt(&[Cue]) -> String`** beside it (U3): `HH:MM:SS,mmm --> HH:MM:SS,mmm`, blank-line separated, `\n` endings, numbered from 1, empty input → empty string. Media never formats SRT.
3. **`ScoreboardMode { Burned, Track }`** and **`default_scoreboard_mode(&ExportTarget) -> ScoreboardMode`** (`Track` for `WholeMatch`, `Burned` for every other variant). Put them where `ExportTarget` lives (`plan.rs`), so the exhaustive match is next to the enum it matches.
4. **`Preferences::last_export_scoreboard: Option<ScoreboardMode>`**, **with no field attribute** — the container's `#[serde(default)]` and the hand-written `Default` impl (which gains `last_export_scoreboard: None`) are the mechanism, and `project.rs`'s module comment forbids the field-level form. `ScoreboardMode` derives `Serialize`/`Deserialize` in `camelCase` like its neighbours.
5. **`CURRENT_FORMAT_VERSION = 11`.** `MIN_READABLE_FORMAT_VERSION` stays 7.

**Test that must fail first:**

- **`a_v10_file_loads_under_the_current_version`** in `tests/project_format.rs`: a raw v10 `project.json` with no `lastExportScoreboard` key loads, with the field `None`, and `write` then stamps 11. It fails before step 5 for the `TooNew`/version reason, and it is the test every bump owes (CLAUDE.md's format rules).
- **One table test in `tests/cues.rs`** over `scoreboard_cues` and `cues_to_srt`, a row per case (spec Testing):
  - a two-source match (kick-off, goal, half-time, second-half start, full time): no cue before the first start; the score turns over on the goal's **own** frame; the break is **one** cue reading `HT`; `FT` after the last period; every cue in a run ends exactly where the next begins;
  - a frozen entry holds the clock — one long cue, not a running one (the invariant BACKLOG #27 is about);
  - stoppage appends `+M:SS`;
  - no scoreboard configured → an empty list;
  - `cues_to_srt`: hours, milliseconds, numbering from 1, and an empty list → `""`.
- **No test for `default_scoreboard_mode`.** A test that restates a two-arm `match` pins nothing the compiler doesn't.

**Verify:** the gate. `cargo test -p video-coach-core` must still pass on a runner with no GStreamer — this task adds no dependency, and that is the point of the crate.

**CLAUDE.md:** one line under the format paragraph — v11 adds `Preferences::last_export_scoreboard`, and a field added to `Preferences` takes **no** attribute because the container carries `#[serde(default)]`.

Commit: `feat(core): scoreboard cues and the export's scoreboard mode (format v11)`.

---

## Task 2: The copy graph

Media only, and it can be built and tested before anything writes a sidecar. At the end of this task a copy runs, is lossless, carries its chapters, refuses a mismatch and cancels cleanly.

**Files:**
- `crates/video-coach-media/src/composite/{mod.rs,copy.rs (new),export.rs}`
- `crates/video-coach-media/src/fixtures.rs`
- `crates/video-coach-media/tests/copy.rs` (new)
- `packaging/smoke-test.sh`

**What to build:**

1. **`ExportJob::render: Render { Encode, Copy }`** (spec M3) in `composite/export.rs`, documented as *which renderer, not which mode*. `run` branches on it once, at the top, into `copy::copy(job, &part, cancel, on_message)` or today's `export`. **`run` keeps the `.part`, the rename, the delete-on-failure and the chapter splice** — one implementation, not two. Every existing `ExportJob` literal gets `render: Render::Encode` in this task.
2. **`copy.rs`: the graph** (L1), built by iterating **`job.compilation.plan.entries` in order**, taking each entry's file as `job.sources[entry.source_index]` — never `project.source_videos` (L1b). Per entry: `filesrc ! qtdemux`, `queue ! h264parse` into the video `concat`, `queue ! aacparse` into the audio `concat`; the `concat`s into one `mp4mux ! filesink`.
   - **A `queue` after every demux pad and before every mux pad**, with the deadlock in a comment.
   - `trak-timescale=90000` on the video pad and the sample rate on the audio pad (L3).
   - `reserved-max-duration` by `export.rs`'s existing formula (L4), and **`reserved-duration-remaining` read at EOS** and returned for the log line (L4, E7).
   - No `Gl`, no display, no encoder.
3. **The compatibility gate, from the pads' own caps** (L6), as they are negotiated in this graph. Absolute: `video/x-h264` on every entry, `audio/mpeg, mpegversion=4` where there is audio, and a file `qtdemux` yields no usable video pad from is refused. Relative to the first entry: `codec_data` byte-identical for video and for audio, and audio present-or-absent the same. A failure is `ExportError::Failed` with the file, the field and the way out named — *"…; choose **Scoreboard: burned in** to export it re-encoded."*
   - **Wait for the gate with a bound**, never forever: a `no-more-pads` (or an error, or a timeout) decides each entry. CLAUDE.md's rule about never blocking a push or pull without a bound applies to this wait too.
4. **Progress** (X3): a pad probe on the video branch into the mux reads each buffer's running time and reports `round(seconds × OUTPUT_FPS)` clamped to `plan.total_frames()`, throttled to whole-percent changes as the encoder's is. EOS reports the total.
5. **Cancel** (X4): the same `AtomicBool`, polled on the bus watch; stop the pipeline and return `ExportError::Cancelled`. `run` deletes the `.part`.
6. **`ExportDone`** from a copy: `encoder: "copy"`, a default `Diagnostics`, `chapters` as usual, and the remaining reserve.
7. **A fixture with AAC audio in MP4:** `CounterKind::H264AacMp4` beside `H264Mp4BFrames` — the same `x264enc bframes=2 ! mp4mux` with a silent `audiotestsrc ! avenc_aac` track sized to the video, as `Vp8WebmWithAudio` sizes its Vorbis. A new kind rather than a flag on the old one, so no existing test's fixture changes shape.
8. **`packaging/smoke-test.sh`:** add `concat` to the element list, as every element the code names by hand is listed.

**Test that must fail first,** `crates/video-coach-media/tests/copy.rs`:

- **`a_copy_of_two_sources_is_lossless_and_chaptered`** — two `H264AacMp4` fixtures, a two-entry whole-match plan with chapters. In one run: `fixtures::decode_counters` reads `0..N` then `0..M` with nothing missing, repeated or out of order; `ffprobe` reports the inputs' `codec_name`, `profile`, `width` and `height`, and a video packet count equal to the sum of the inputs'; `ffprobe -show_chapters` reads the plan's chapters back at the expected times (which is what proves `reserved-max-duration` was set — without it the splice skips and this fails).
- **`a_mismatched_pair_refuses_and_leaves_nothing`** — two fixtures at different sizes: `ExportError::Failed` naming the second file, **no `.part` and no file at the target path**. **This is the test the 320×240/640×480 measurement demands:** without the gate the run *succeeds* and writes a wrong file, so watch it fail that way first.
- **`a_cancelled_copy_leaves_nothing`** — cancel mid-copy on a long-enough fixture: `ExportError::Cancelled`, no `.part`, no output.

A missing `ffprobe` **fails** these tests with a message naming the package. They never skip.

**Verify:** the gate. The copy tests need no GPU, so they run as CI runs them with no special recipe.

**CLAUDE.md:** a paragraph under the export rules — the whole match in track mode is a **stream copy** (`composite/copy.rs`), not an encode: `qtdemux ! h264parse/aacparse ! concat ! mp4mux`, a `queue` on every demux and mux pad or it deadlocks, `trak-timescale` pinned, the `moov` reserved so the chapter splice still works, and **the caps gate is the only thing that refuses mismatched sources** — `mp4mux` will happily write one `stsd` over two different videos.

Commit: `feat(export): copy the whole match instead of re-encoding it`.

---

## Task 3: The sidecar

Still media, and it touches **both** renderers: a burned export must remove a stale `.srt` too.

**Files:**
- `crates/video-coach-media/src/composite/export.rs`
- `crates/video-coach-media/src/lib.rs`
- `crates/video-coach-media/tests/{copy.rs,export.rs}`

**What to build:**

1. **`ExportJob::cues: Vec<Cue>`** — the payload, computed by the bus (Task 4). Empty means no sidecar. **`video-coach-media` already depends on `video-coach-core`**, so `Cue` crosses no new boundary.
2. **`ExportDone::sidecar: Option<PathBuf>`**, filled where `ExportDone` is built (`composite/export.rs:439`). Its three test sites (`tests/export.rs:83`, `:103`, `:1679`) and the `bus: exported …` line (Task 4) follow in the same task they belong to.
3. **The write, after the rename, in `run`, for every renderer** (T6):
   - the path is **`job.path.with_extension("srt")`** — which inherits `file_name`'s `/` and `:` cleaning and the run's `" (2)"` de-duplication, and is the matching basename a player auto-loads;
   - with cues: write `cues_to_srt(&job.cues)`;
   - **with no cues: remove any file already at that path.** One step, one place: *the file that belongs beside this output is this string, or nothing.* Without it a coach who deletes their scoreboard and re-exports gets the old score played over the new film.
   - **A failure is reported, never fatal:** a good `.mp4` must not be thrown away over a 200 KB text file. It is logged and `sidecar` is `None`.
   - **A cancel writes nothing and removes nothing** (X4): the sidecar step runs only after a successful rename, so a cancelled run cannot delete the last good export's `.srt`.

**Test that must fail first:**

- **`an_empty_cue_list_writes_no_sidecar_and_removes_a_stale_one`** in `tests/copy.rs`: put an `.srt` at the target's sidecar path first; run a copy with `cues: vec![]`; afterwards the `.mp4` is there, the `.srt` is gone, and the run did not hang.
- **Extend Task 2's `a_copy_of_two_sources_is_lossless_and_chaptered`** with a cue list: the `.srt` sits beside the `.mp4`, its bytes equal `cues_to_srt(&cues)`, and `ExportDone::sidecar` names it.
- **One assertion in `tests/export.rs`** on the encoded path: a burned export with no cues removes a stale `.srt` at its own sidecar path. That is the case the copy tests can't reach, and it is the one a coach hits by switching the picker back.

**Verify:** the gate.

**CLAUDE.md:** two lines under the same export paragraph — the scoreboard sidecar is `job.path.with_extension("srt")`, written after the rename, never fatal; **and a run that writes no sidecar removes a stale one, in every mode.**

Commit: `feat(export): the scoreboard as an .srt beside the file`.

---

## Task 4: The picker, and the run

The app side: one picker, one mapping, and the two corrections the correctness review found in the run.

**Files:**
- `crates/video-coach-app/ui/app.slint`
- `crates/video-coach-app/src/main.rs`
- `crates/video-coach-app/src/bus/{mod.rs,export.rs}`
- `crates/video-coach-harness/tests/whole_match.rs` (new)

**What to build:**

1. **`Command::Export` gains `scoreboard: Option<ScoreboardMode>`** — `None` is "Default".
2. **One mapping function in `bus/export.rs`,** used by the job builder and nothing else:
   `let mode = scoreboard.unwrap_or_else(|| default_scoreboard_mode(&target));`
   then, per job:
   - `Track` → **`job.scoreboard = None`** (the overlay's one read, so `None` *is* "don't draw the board"), and `job.cues = scoreboard_cues(&compilation, &context)` from the `ScoreboardContext` the bus already built at `bus/export.rs:574` — **computed before `scoreboard` is blanked**;
   - `Burned` → today's `job.scoreboard`, and `job.cues = vec![]`;
   - `job.render = Render::Copy` **only** for `ExportTarget::WholeMatch` in `Track` mode; `Render::Encode` otherwise.

   No `scoreboard_mode` field reaches media, and `overlay.rs` is not touched in this plan.
3. **The picker.** A third `ComboBox` beside Resolution and Quality (`app.slint:1365-1385`), labelled **"Scoreboard"**, model `["Default", "Burned into the picture", "Separate track"]`, with `export-scoreboard` alongside `export-resolution` and `export-quality` (`:1963-1964`, `:3491-3492`), read in `main.rs:616-627` and set from `Preferences` in `main.rs:669-694`.
   - **Under the picker, shown only for "Separate track":** *"Copied, not re-encoded; highlights and drawings can't ride a copy."* One line of text, the only place the trade is explained (spec M1, N). **The rows do not change and are not re-listed** (M5, X2).
4. **Remember it** in `Preferences::last_export_scoreboard`, in the same write-back that already stores the other two (`bus/export.rs:375-378`), on the same "only when it changed" condition — so opening the sheet still never dirties the project.
5. **Clear the rate window when a target finishes** (X3): one line in `Active::finish_target` (`bus/export.rs:248`), `self.rate = RateWindow::default()`. A copy runs at ~3,600 output frames a wall second against an encode's ~20, and without this the clips queued behind it inherit that rate and the sheet promises they will finish almost immediately. `RateWindow` needs a minimum span before it answers, so the gap is silent rather than wrong.
6. **The `bus: exported …` line** (`bus/export.rs:258-269`) gains the sidecar and the remaining `moov` reserve. Its shape is unchanged, so nothing that reads the log breaks.

**Test that must fail first,** `crates/video-coach-harness/tests/whole_match.rs`:

- **`the_whole_match_is_copied_with_a_sidecar`** — a two-source project (the `H264AacMp4` fixture twice), a scoreboard with a kick-off and a goal, exported with `scoreboard: Some(Track)`:
  - the run's progress reaches `plan.total_frames()`;
  - `Whole match - <project>.mp4` and `Whole match - <project>.srt` land in `exports/`;
  - the `.srt`'s first cue text carries the configured team names and a clock;
  - the video is a copy, not an encode — assert on `ExportDone`'s `encoder == "copy"` through the run's state, or on the packet count matching the sources'.
- **`the_default_mode_copies_the_whole_match_and_burns_a_clip`** — one run with `scoreboard: None` over both targets: the whole match gets a sidecar, the clip gets none, and neither file is missing.
- **`a_burned_whole_match_removes_a_stale_sidecar`** — export `Track`, then export `Burned` to the same path: the `.srt` is gone.

**Verify:** the gate, plus a screenshot pass over the export sheet with the picker on each of its three entries (the third shows the line under it).

**CLAUDE.md:** a short paragraph under the export rules — the export sheet's Scoreboard picker is `Default / Burned in / Separate track`, `Default` is per target (`default_scoreboard_mode`), the choice lives in `Preferences` (v11), **Track mode blanks `job.scoreboard` rather than carrying a mode flag into media**, and the run's rate window is cleared between targets because a copy and an encode are three orders of magnitude apart.

Commit: `feat(app): choose whether the scoreboard is burned in or a separate track`.

---

## Task 5: Closeout, and the build the user checks with

1. **Adversarial review** of the whole diff (the `adversarial-review` skill, CLAUDE.md's pattern). Apply, skip or defer.
2. **Backlog** what is deferred, with "Why deferred" and "When to revisit". At minimum the spec's Deferred 1 (the embedded `tx3g` track, **gated on the user's report**) and Deferred 5 (the per-source A/V drift).
3. **Check each task's CLAUDE.md addition** is there and still accurate.
4. **`docs/hands-on-checklist.md`:** a section for the copied whole match, in the checklist's own voice, from the user's steps below.
5. **The build.** Bump `[workspace.package] version` to `0.4.0`, build with `flock /tmp/claude-1000/cargo.lock nice -n 19 packaging/build-deb.sh`, run `packaging/smoke-test.sh` (the element list changed, so it is **not** optional this time), and copy the `.deb` to `~/Downloads`. Stage `Cargo.toml` and `Cargo.lock` explicitly. Tagging `v0.4.0` and pushing it is the user's call.

Commit: `chore: 0.4.0, the whole match copied with the scoreboard beside it` (with the review's fixes in their own commits).

---

## The user's own steps

1. **Install 0.4.0** (`sudo apt install ~/Downloads/coach-cuts_0.4.0_amd64.deb`).
   **This build writes format v11.** The first save upgrades a project, and 0.3.x then refuses it ("newer than this build supports"). **`project.json.v10` beside it is the way back:** put it back as `project.json`, losing what changed since.
2. **Export the match.** Open the export sheet on your tagged match, tick **Whole match**, leave Scoreboard on **Default**, and export. Expect **about half a minute and about 2 GB**, not an hour and 8 GB.
3. **Check it on the laptop, in VLC.**
   - The score and clock appear by themselves, bottom-centre, and track the play. (If they don't, check that the `.srt` sits beside the `.mp4` with the same name.)
   - Scrub across the join between the halves: no stutter, no flash, sound continuous.
   - `[` / `]` and Playback → Chapter land where they should.
   - Turn the subtitles off: the picture underneath is the camera's own, with no board burned into it.
4. **Then the one question this plan can't answer: play it where the parents actually watch.** The TV, a phone, a share, whatever you'd really send.
   - **Report which of them shows the line.** That verdict, and only that verdict, decides whether the embedded `tx3g` track (spec Deferred 1) is ever built. If the sidecar travels, it never is: the cue list is already there, and the track would only add a mux pad and a stall hazard for a track VLC lists and won't turn on.
   - If it does **not** travel, say so and the embedded track becomes the next task; the cue list it needs is already built and tested.
5. **Your call on two things:**
   - whether "2 videos · 54:16" still reads right on a row that now takes half a minute (the plan deliberately left the row alone);
   - whether the subtitle's position, in your player's own font, is ever in the way — if it is, the positioning tag (spec Deferred 9) is a one-line change for libass-based players, though not for VLC's own decoder.

---

## Deliberately not in this plan

Everything in the spec's Deferred list, in particular:

- **the embedded `tx3g` track** — gated on step 4 above, and, if built, with its `trak-timescale` pinned at 1000 for the same reason L3 pins the other two;
- **a styled track (ASS in Matroska)**, `avc3`, and re-encoding only the source that doesn't match;
- **correcting the per-source A/V drift** (~4.5 ms a source) — it needs a re-encode or a re-timestamp, which is what this spec exists to avoid;
- **a sidecar for clips and reels** — a 12-second clip already carries its own text bar;
- **a >4 GB fixture.** `mp4mux` switches to `co64` by itself, and a `moov` that outgrew its reserve is a muxer error, which fails the export and deletes the `.part`. The run logs `reserved-duration-remaining` instead (L4, E7) — a number in the log beats minutes of CI and gigabytes of scratch proving what the muxer guarantees;
- **greying Resolution and Quality** when only a copied target is ticked, and **ticking the whole match by default**.
