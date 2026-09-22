# Match Vision Plan (round trip, goals reel, chapters, highlights)

**Date:** 2026-09-22
**Spec:** `docs/superpowers/specs/2026-09-22-match-vision-design.md` (decisions F, C, R, H, D, T, J, B, L, G). It proceeds on the defaults for Q1, Q2, Q3, Q5 and Q6.
**Status:** Adversarial review applied, not yet executed.

**Scope.** P0, P1 and P2 are planned in full here: 3, 7 and 6 tasks. They need no ML and can be built now. P3–P7 are outlines only (see the end of this plan). Each depends on measurements from the user's tagged matches, and each gets its own detailed plan once its entry gate is met.

**Execution.** A fresh subagent per task (`superpowers:subagent-driven-development`), given this plan, the spec and `CLAUDE.md`.
- Tasks run one at a time, in the order written, in one tree. The cargo lock serialises every build anyway.
- The orchestrator runs the gate below and commits each task. It stages paths explicitly, never with `git add -A`.
- **At the end of every task the workspace builds and every test passes.** A task that changes a type fixes every user of it in the same task, in every crate.
- Each task writes its own `CLAUDE.md` addition, where it names one.

**The gate, for every task:** run the `verify` skill, with every cargo call under the machine-wide lock (`flock /tmp/claude-1000/cargo.lock nice -n 19 cargo …`), because other sessions build here too.

**Test-first.** Each task names the test that must fail first. Write it, run it and watch it fail for the stated reason, then build until it passes. A test that fails only because the code doesn't compile yet counts, but the plan names a behavioural failure wherever one exists.

**What tests may touch.** No test reaches the network, the real camera or mic, or the user's footage. The exceptions are the `#[ignore]`d `COACH_FOOTAGE` tests (and, in P3, `COACH_GROUND_TRUTH`). Real footage and anything that identifies a team or player is never committed: the repository is public and the footage shows children. That includes file names, team names and shirt numbers, in code, tests, commit messages and docs.

**Known facts. Don't re-derive these.** Each was checked in the code while writing this plan.
- **`store::read` refuses every version below `CURRENT_FORMAT_VERSION` as `LegacyProject`** (`store.rs:118`). `project_format.rs::newer_format_is_refused_as_too_new` hard-codes `8`, so it breaks the moment current becomes 8.
- **`MatchEventRecord` has no serde defaults** (`scoreboard.rs:150`). `Project.match_events` has a field-level `#[serde(default)]`: that is the pattern F2 means.
- **`compilation_schedule` zips `plan.entries` with `selected_clips`** and `debug_assert`s the ids (`export.rs:127-133`). A reel entry has no clip, so the pairing has to go.
- **`audio::Mixer::new` reads `job.entries[i].recording` for every commentary region** (`media/src/composite/audio.rs:108`), and its Game arm already falls back to silence with `unwrap_or_default`. A clip with `show_pip` off still needs its recording for the commentary, which is why a reel entry's media is `None` as a whole (Task 1.2, spec R4).
- **`ExportTarget` is matched exhaustively in two places:** core's `plan::selected_clips` and app's `bus/export.rs` `label()`. `main.rs:536`'s `matches!` is not exhaustive.
- **`Pip::open` reads the `Clip` only for `show_pip`** (`composite/export.rs:414`). `OverlayFrame` reads it only for `visible_strokes`, which takes `&Clip` at 25 call sites.
- **`OverlayFrame` carries no zoom.** Strokes live in the content rect and deliberately don't move with the zoom. A highlight lives in source space and must move with it, so core maps it (`highlight_shapes`, Task 2.1) and the overlay stays zoom-agnostic.
- **`Decoder` is `pub(super)`,** and `mailbox::Frame` drops the sample's segment, so neither a displayed frame's stream time nor `frame_at`'s choice is reachable from the harness today.
- **The harness has no accessor for the scan mailbox.** `BusHandle::mailbox()` exists.
- **`fixtures::counter_video_with(.., CounterKind::H264Mp4BFrames, ..)` writes an edit-listed MP4**, which is the stream-time trap.
- **The Export… button is enabled on `clip-count > 0`** (`app.slint:2590`). A project with tagged goals and no clips (every ground-truth project) could never export its reel.
- **`export_targets`' detail reads "N clips"** (`main.rs:530-533`), from `ExportTargetRow.clips`. The sheet's default ticks (`main.rs:536`) tick every row but a clip's unless the sheet was opened on that clip, so a reel row needs no code to follow them.
- **`store::write` stamps `CURRENT_FORMAT_VERSION`** (`store.rs:163`) over whatever version was read, and keeps no copy of the old file.
- **The arrow keys skip 3 s** (10 s with Shift). Nothing steps one frame.
- **The recording allow-list** is the `matches!` in `Bus::command` (`bus/mod.rs:666`).
- **`UndoController::purge_for_source_change`** purges `EditMatchEvents` from both stacks through one `stale_events` predicate (`undo.rs`).
- **`h`, `[`, `]`, `,` and `.` are free** in `handle-key`.
- **The UI draws scan frames in `video.rs`,** which takes each from `BusHandle::mailbox()` in the rendering notifier. That is the one place that knows which frame is on screen.
- **The live stroke layer** is `if !root.previewing: Rectangle` inside the content-clipped `Rectangle` in `app.slint`, around line 2248.
- **`mp4mux`'s `moov` reserve** is set in `Encoder::start` (`composite/export.rs:722-729`).
- **`ffprobe` is on the laptop** (`/usr/bin/ffprobe`) but not in `packaging/build-deps.txt`.
- **`scan_abs`** (`main.rs:1818`) is the one way the UI reads "where the game video is". Every caller-captured position in this plan comes from it, mapped through `Project::locate`, as `on_tag_match_event` does.

---

## P0: the round trip (BACKLOG #67, spec H6)

The gate before P2 and before the user starts tagging: a scrub lands within one frame of its target, and the scan player displays the frame export picks for the reported position. P0 also adds a one-frame step, which tagging and highlight placement both need, and ends with the build the user tags on.

### Task 0.1: Make the round trip measurable, and reproduce #67

**Files:**
- `crates/video-coach-media/src/mailbox.rs`
- `crates/video-coach-media/src/composite/{mod.rs,decode.rs}`
- `crates/video-coach-media/src/lib.rs`
- `crates/video-coach-harness/src/lib.rs`
- `crates/video-coach-harness/tests/{real_footage.rs,transport.rs}`

**What to build:**
1. **`Frame.stream_time: Option<f64>`**, set in `Frame::from_sample` from the sample's segment: `segment.to_stream_time(pts)`, which is the rule CLAUDE.md sets for export. It is `None` when the segment isn't in time format. It says which frame is on screen: this task's tests read it, and P2's highlight keys are placed at it (Task 2.5).
2. **`pub fn frame_times(source: &Path, targets: &[f64]) -> Result<Vec<f64>, CompositeError>`** in `composite/mod.rs`. It opens one `Decoder` on `Gl::shared()` and returns, for each target in the order given, the stream time of the frame `Decoder::frame_at(seconds_to_clock(target))` answers with. It is the export's own choice, reached without an export. Document it as a diagnostic seam, like `fixtures`.
3. **`Harness::take_frame() -> Option<Frame>`**, the scan mailbox's newest frame, as `take_self_view` does for the self-view.
4. **`round_trip(h, source, targets) -> Vec<Landing>`**, a helper in the harness library that both tests share. For each target it:
   - sends `ScrubRelease` while paused;
   - waits for the seek to settle and a new frame to arrive;
   - records the target, `position_secs()`, and the displayed frame's `stream_time`.

   After the loop it calls `frame_times` on all the reported positions, and returns one `Landing` per target. `Landing::check()` asserts two things: the displayed stream time equals the export's within 1 µs (decode.rs's `SLACK`), and `|reported − target| ≤ FRAME`. **`FRAME` is a fixed 1/30 s,** never `info.fps()`, which can read 0/1 on an HLS remux. Every landing is printed on `--nocapture`, whether it passes or fails.
5. **The CI guard,** `a_paused_scrub_shows_the_frame_export_picks` in `harness/tests/transport.rs`: `round_trip` over 20 targets spread across a 10 s `counter_video_with(H264Mp4BFrames)` fixture, with `Harness::new` (System sink). It should pass today. It pins the round trip on the edit-list class before anything changes.
6. **The reproduction,** `real_footage_scrubs_land_on_the_frame_export_picks` in `real_footage.rs`, `#[ignore]`d behind `COACH_FOOTAGE` like its neighbour. It runs the same helper over 20 targets spread across the whole file. It uses `Harness::new`, because only the System sink's frames can be read back. It then repeats the positions-only part under `Harness::production()` and **asserts `|reported − target| ≤ FRAME` there too**, printing both runs side by side. The app runs the production sinks, and the bug may depend on `autoaudiosink`'s clock.

**Test that must fail first:** the ignored test, on a Trace half, must fail. That is the point of this task: it is #67 reproduced in a test (a scrub to 812 reported 811.70). Run it:

```bash
COACH_FOOTAGE=/path/to/trace-half.mp4 flock /tmp/claude-1000/cargo.lock nice -n 19 \
  cargo test -p video-coach-harness --test real_footage real_footage_scrubs -- --ignored --nocapture --test-threads=1
```

The footage path comes from the user. It never goes into a commit. The CI guard passes. If it fails, that is a finding in its own right: stop and report it.

**Verify:** the gate. Record the failing run's numbers (target, reported, displayed and export stream times, with no file name) for Task 0.2. Don't commit them.

Commit: `test: measure the scan-to-export round trip (BACKLOG #67)`.

### Task 0.2: Fix #67

**Files:** decided by the root cause. Most likely `crates/video-coach-media/src/player/mod.rs`, `crates/video-coach-app/src/bus/transport.rs` and `crates/video-coach-media/src/fixtures.rs`. Also `BACKLOG.md`, and `CLAUDE.md` if the cause is a new class.

**Investigate before changing anything.** Use the numbers from Task 0.1, plus `ffprobe -show_packets -select_streams v` and `-select_streams a` around 568 s and 812 s. The symptoms are:
- the error is not monotonic (−0.30 at 812, +0.19 at 568);
- a 70 s stream-copy of the same footage was within 9 ms;
- the file is an HLS remux with irregular 29.997 fps timestamps.

Hypotheses to rule in or out, in this order:
1. **The position query answers in the audio's time, not the video's.** `playbin3`'s position can come from the audio sink. In an HLS remux, the AAC timestamps can drift from the video's per segment, or carry a different `elst`. Test it by comparing the reported position with the displayed frame's stream time on a release. Task 0.1's production and System runs already separate the audio sink's part in this.
2. **The ACCURATE seek lands on the wrong frame** on irregular timestamps (CLAUDE.md: "ACCURATE seeks drop frames in VFR or gapped files"). Test it by comparing the displayed frame's stream time with the target.
3. **Stream time versus raw PTS** somewhere in the scan path: an edit list per HLS segment, or a non-zero first PTS.
4. **The seek target is rewritten on the way to the player** (`transport.rs`'s `scrub`/`seek_abs`, the skip coordinator's anchoring).

**The fix follows the cause.** One constraint: the scan player's reported position, `Decoder::frame_at` and the stored tag times must agree on one timeline, which is stream time. Prefer fixing the player's report, or its seek, over teaching export a correction.

**Test that must fail first:**
- The ignored test from Task 0.1, which already fails. It must pass on all four halves on hand, in both its System and its production runs.
- **If the cause can be reproduced in a generated fixture**, also add a failing-first CI test on it before the fix. Grow `CounterQuirks` with the quirk: for example, per-segment timestamp discontinuities, or an audio track offset from the video. The test is `round_trip` on that fixture in `transport.rs`.
- **If it can't be reproduced**, the commit message says the ignored test is the only proof, as H6 requires.

**Verify:** the gate, plus the ignored test on each half. Mark BACKLOG #67 resolved with the cause, in aggregate numbers only. If the cause is a new class, add a line to CLAUDE.md's decode rules.

**Hands-on (batched):** scrub to a few places in a Trace half. The picture and the readout agree.

Commit: `fix(player): a scrub on a Trace file lands on its target (BACKLOG #67)`.

### Task 0.3: One-frame steps with `,` and `.`, a tenths readout, and the 0.1.1 build

The arrows skip 3 s, which is too coarse to find the frame the ball crosses the line, or to place a highlight key on the frame the coach means. This task adds a one-frame step while paused, then builds the app the user tags with.

**Files:**
- `crates/video-coach-app/ui/app.slint`
- `crates/video-coach-app/src/{main.rs,format.rs,bus/mod.rs,bus/transport.rs}`
- `crates/video-coach-media/src/player/mod.rs` (if the step needs the player)
- `crates/video-coach-harness/tests/transport.rs`
- `Cargo.toml`, `Cargo.lock`

**What to build:**
1. **`Command::StepFrame { forward: bool }`.** The bus acts on it only while the scan player is paused, with no seek outstanding and no preview open. It is **not** on the recording allow-list: a step during a paused take would move the source without a log entry that replay could follow.
2. **The step lands on the neighbouring frame, and the reported position is that frame's stream time.** How it steps is the implementer's call (a one-buffer `Step` event, or an ACCURATE seek to just past the displayed frame's end or just before its start), provided it holds on irregular timestamps. The test decides.
3. **`,` steps back and `.` steps forward,** in `handle-key` after the `text-editing` yield, gated `!recording && !previewing && can-play`. A held key repeats, as the skip keys do.
4. **The time readout shows tenths while paused** (`12:34.5 / 27:10`), and whole seconds while playing, so the digits don't flicker. Without it a frame step is invisible: the whole-second readout holds for ~30 steps. The total stays whole seconds. The tenths are floored, like `format_hms`, so a frame at 12:34.99 never reads 12:35.0. Add a `format_hms_tenths` beside `format_hms` in `format.rs`, and pick between them in the 30 Hz readout update in `main.rs` by the player's paused state.

**Test that must fail first:** `a_paused_step_moves_exactly_one_frame` in `harness/tests/transport.rs`, on the `H264Mp4BFrames` fixture, paused mid-file:
- five `.` steps each show a frame (`Frame.stream_time`) exactly one fixture frame later, and the reported position equals it;
- five `,` steps return to the starting frame exactly;
- `,` on the first frame does nothing;
- a step while playing does nothing.

Also a unit test in `format.rs`: `format_hms_tenths` floors (`754.99` → `12:34.9`), has an hours form (`1:02:03.4`), and reads `0:00.0` for non-finite or non-positive input, as `format_hms` does.

**Verify:** the gate.

**CLAUDE.md:** one line under the transport rules: `,` and `.` step one frame while paused, and the arrows skip; the readout shows tenths while paused.

Commit: `feat(app): step one frame with , and ., and show tenths while paused`.

**The 0.1.1 build.** The user's tagging (G1) waits for P0 and needs P0's fix and the frame step in the app they use. The spec names no release point, and nothing may block P1 or P2, so this plan chooses one here: a small build now, and 0.2.0 after P2.
1. Bump `[workspace.package] version` to `0.1.1`.
2. Build with `packaging/build-deb.sh` (it runs cargo itself; hold the lock around it: `flock /tmp/claude-1000/cargo.lock nice -n 19 packaging/build-deb.sh`). P0 changes no dependency, so the smoke test isn't needed.
3. Copy the `.deb` to `~/Downloads`.
4. Stage `Cargo.toml` and `Cargo.lock` explicitly.

Tagging and pushing a GitHub release is the user's call. **No format change ships in 0.1.1**, so the projects the user tags are v7, and every later build reads them (Task 1.1).

Commit: `chore: 0.1.1, the build the ground truth is tagged with`.

---

## The user's own steps (in order; they never block P1 or P2)

1. **Install 0.1.1** from `~/Downloads` (`sudo apt install ~/Downloads/coach-cuts_0.1.1_amd64.deb`). **Before tagging anything, confirm the scrub fix:** in a Trace half, scrub to a few places and check that the picture and the readout agree. Pause and press `.` a few times: the readout's tenths (`12:34.5`) move with each frame.
2. **One project per match** (G1). Start from a new, empty folder for each match.
   - Put that match's video files **inside the project folder** before adding them. Then step 5's copy is one folder, and its relative paths still resolve.
   - Add the halves in order.
   - Open **Set up teams…** and enter the teams and the real format (the number of periods and their minutes).
3. **Tag the whole match before any detector has seen it:**
   - `V` on the whistle that starts each period, and on the whistle that ends it;
   - `Z` or `X` on the frame the ball crosses the line, not the celebration. Pause, then step one frame at a time with `,` (back) and `.` (forward) to find it.

   Don't tag near misses. If a file starts after its half's kick-off, or ends before the final whistle, leave that tag out and add a `# missing` line to `kickoffs.txt` (next step). The first half's missing kick-off is covered by the setup sheet's "My video starts after kick-off".
4. **Write `kickoffs.txt`** beside `project.json`: one line per restart after a goal, at the moment the ball is played from the centre spot.
   - Each line is the source's number (1 for the first half, 2 for the second) and the time into that file as `mm:ss`, for example `2 14:05`.
   - Use `#` for comments.
   - Period-start kick-offs are already your `V` tags, so leave them out.
5. **Copy each tagged project folder, videos included, to local disk,** for example `~/coach-truth/match-a`. The scoring runs decode every half in full, and a network mount is too slow for that. These copies stay on your disk. They never go into the repository.
6. **Repeat for the second match** (the four halves on hand are two matches). Add a third if one gets recorded.
7. **P3 starts** once one match has gone through steps 3–5. Its verdicts wait for the second.
8. **After P2's release, install 0.2.0 and run the new checklist section** (Task 2.6). 0.2.0 upgrades a project the first time it saves it, to format v9, which 0.1.1 refuses to open ("newer than this build supports"). **`project.json.v7` beside it is the backup:** to go back to 0.1.1, put it back as `project.json`, losing what was changed since.

---

## P1: the goals reel and chapters (format v8)

Order: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, then 1.7 closes the phase. 1.3 needs 1.1 and 1.2. 1.5 needs 1.3 and 1.4, and 1.6 needs 1.5.

### Task 1.1: Read v7 onward, and store reel trims (v8)

**Files:**
- `crates/video-coach-core/src/{store.rs,scoreboard.rs,project.rs}`
- `crates/video-coach-core/tests/{project_format.rs,scoreboard.rs}`
- every `MatchEventRecord { … }` literal: `grep -rn "MatchEventRecord {" crates`

**What to build, in this order:**
1. **The bump: `CURRENT_FORMAT_VERSION = 8`.** Then run the new `a_v7_file_loads_under_the_current_version` (below) and watch it fail with `LegacyProject`. That failure is F1's reason.
2. **F1.** `MIN_READABLE_FORMAT_VERSION = 7`. `read` accepts `MIN_READABLE..=CURRENT`, `LegacyProject { minimum }` reports `MIN_READABLE`, and `TooNew` stays for anything above current. Fix `CURRENT_FORMAT_VERSION`'s doc ("and is the minimum it reads" stops being true).
3. **The upgrade backup.** When `store::write` is about to raise a file's version (the project it was given was read at an older `format_version`, and `project.json` exists), it first copies `project.json` to `project.json.v<old>`, **only if that file doesn't exist yet**. It is never overwritten, so it stays the file as the older build last wrote it, and going back to that build is possible.
4. **`MatchEventRecord.reel_lead_in` and `reel_tail`: `Option<f64>`, each with a field-level `#[serde(default)]`.**
   - They are positive magnitudes, in seconds: the lead-in is the time from the reel's start to the goal, and the tail the time from the goal to its end. `None` means the default (Task 1.3).
   - Serialize them always, with no `skip_serializing_if`, so there is one shape on disk.
   - Amend `project.rs`'s header comment: a field-level default on an `Option` or a `Vec` is exactly what an older file means (F2). The hazard it warns of is `f64` and `bool`.
5. **`Project::set_reel_trim(goal: Uuid, end: ReelEnd, at: Option<(usize, f64)>) -> Result<(), ReelTrimError>`**, with `ReelEnd { Start, End }`. `None` resets that side to the default. It refuses:
   - an id that isn't a goal;
   - a position on another source from the goal's;
   - a start that isn't before the goal, or an end that isn't after it.

   It stores `goal − at` for a start and `at − goal` for an end.

**Test that must fail first:**
- **`a_v7_file_loads_under_the_current_version`:** a raw v7 JSON with a goal and no trim keys loads, with both trims `None`. It fails after step 1 with `LegacyProject`. `write` then stamps 8.
- **`an_upgrade_keeps_the_old_file_once`:** writing a project read at v7 leaves `project.json.v7` byte-identical to the v7 file. A second write leaves it untouched. Writing a project read at the current version makes no backup.
- `swift_era_v6_is_refused` now expects `minimum == MIN_READABLE_FORMAT_VERSION`.
- `newer_format_is_refused_as_too_new` uses `CURRENT_FORMAT_VERSION + 1`.
- The trims round-trip.
- Each of `set_reel_trim`'s refusals, and a reset leaving the other side alone.

**Verify:** the gate.

**CLAUDE.md:** a format paragraph under "Build + test conventions":
- the readable range is `MIN_READABLE..=CURRENT`;
- every phase that stores a new field bumps once (F3);
- the new fields of an existing struct are `Option` or `Vec` with a field-level default, while a new struct's fields have none (F2);
- every bump comes with a test that the oldest readable version still loads;
- the first save after an upgrade keeps `project.json.v<old>`, once, never overwritten.

Commit: `feat(core): read v7 onward; reel trims on goals (format v8)`.

### Task 1.2: Plan entries without a clip

This is plumbing only. A plan can now hold an entry that has game video and nothing else. Every existing test keeps passing unchanged, except `core/tests/plan.rs:70` and `:187`, which compare `clip_id` with a clip's id and now need `Some(id)`.

**Files:**
- core: `plan.rs`, `export.rs`, `audio.rs`, and `tests/{plan,export,audio}.rs`
- media: `overlay.rs`, `composite/{export.rs,audio.rs,preview.rs}`, `fixtures.rs` (if it builds entries), `tests/export.rs`
- app: `bus/export.rs`

**What to build:**
1. **`PlanEntry.clip_id: Option<Uuid>`.**
2. **`compilation_schedule`** walks each entry with the events of its own clip, looked up by `clip_id`, or `&[]`. It no longer zips with `selected_clips`. `selected_clips` stays as the plan's selection.
3. **`audio_regions`** adds the commentary region only for an entry with a `clip_id`.
4. **`ExportJob.entries: Vec<Option<EntryMedia>>`,** with `EntryMedia` unchanged (`{ recording, clip }`). An entry is `None` only when it has no clip.
   - `Pip::open` takes the entry's `Option<&EntryMedia>`. `None` gets the GL filler, and `Some` decides on `clip.show_pip` exactly as today.
   - `Mixer::new`'s Commentary arm falls back to silence for an entry with no media, as the Game arm does with `unwrap_or_default`. No `expect`.
   - **`OverlayFrame.clip: Option<&Clip>`,** and `None` draws no strokes. `visible_strokes` keeps its `&Clip` argument: this is the smaller reshape (spec R4).
   - The bus fills `Some(EntryMedia)` for every clip entry, exactly as today.
5. Preview passes `Some(&job.clip)`. `PreviewJob` keeps its `Clip`, since a preview is always of a clip.

**Test that must fail first:**
- Core, `an_entry_without_a_clip_is_game_audio_only`: a hand-built compilation with a `clip_id: None` entry yields game regions and no commentary region.
- Media, `an_entry_with_no_media_exports_game_audio_only_with_a_filler_pip` in `media/tests/export.rs`:
  - use a `tone_video` source and one entry whose media is `None`;
  - the file decodes with the tone at its time;
  - the PiP rect shows the source's pixels, not an inset;
  - the frame count equals `plan.total_frames()`.

**Verify:** the gate. `media/tests/export.rs` runs on llvmpipe as in CI. If the GPU path needs checking, use CLAUDE.md's `bwrap` recipe.

Commit: `refactor: plan entries that carry no clip`.

### Task 1.3: The reel's plan (R2–R4)

**Files:**
- `crates/video-coach-core/src/{plan.rs,reel.rs (new),lib.rs}`
- `crates/video-coach-core/tests/reel.rs` (new)
- `crates/video-coach-app/src/bus/export.rs` (the `label()` arm)

**What to build:**
1. **`ExportTarget::Reel`.** `compilation_plan` builds its entries from the goals, not from `selected_clips`. Every exhaustive match gains its arm in this task, so the workspace still builds:
   - core's `selected_clips` selects no clip for `Reel`;
   - app's `bus/export.rs` `label()` returns `REEL_LABEL = "All goals"`, which gives the file name `All goals - <project>.mp4` (E6).
2. **`reel.rs`:**
   - **Defaults:** `REEL_LEAD_IN = 30.0` and `REEL_TAIL = 6.0` (Q3's default).
   - **Which goals:** both goal kinds, in match order (`abs_seconds`, a stable sort).
   - **Each goal's span:** `start = max(0, goal − lead, prev_end on the same source)` and `end = min(duration, goal + tail)`. The lead and tail are the goal's own trims, or the defaults. `duration` is `SourceRef::duration_seconds`.
   - **A goal at or before the previous entry's end on the same source makes no entry.** Its moment is already in that entry, so the entry's end extends to `max(prev_end, min(duration, goal + tail))` instead, and the goal's own lead-in is ignored.
   - **Each entry:** one `Play` segment from `start` to `end`, and `frames = frame_count(end − start)`.
   - **The numbering** (`<n> / <total>`) counts entries, not goals.
3. **The entry text** is `"<n> / <total> | <team> goal | <home>-<away>"`, for the entry's first goal.
   - The score is `ScoreboardContext::for_project(project)?.state_at(goal.source_index, goal.source_seconds)`, which counts the goal itself.
   - With no state, the score part is dropped.
   - With no scoreboard, the team is "Home" or "Away".
4. `compilation_schedule` on `Reel` needs nothing new after Task 1.2: identity zoom, and the source time runs from `start` to `end`.

**Test that must fail first:**
- `a_goal_gets_thirty_seconds_before_and_six_after`.
- The clamps: at 0, at the source's end, and at the previous goal's end on the same source but not across sources.
- **A goal inside the previous entry** (3 s after a goal, on the same source) makes no entry, extends the previous entry's end to its own tail, and the numbering reads `1 / 1`.
- One side of a trim overrides only that side.
- Match order across two sources.
- The entry text with a state, without one, and with no scoreboard.
- A goal the scoreboard doesn't count still has an entry, and shows the unchanged score.
- A project with no goals yields an empty plan.

**Verify:** the gate.

**CLAUDE.md:** a "The goals reel" paragraph:
- it is an `ExportTarget`, never a clip;
- it holds confirmed goals only;
- each entry is one `Play` segment, and a goal inside the previous entry extends it rather than making its own;
- the defaults are 30 s and 6 s, and are never replaced by a shorter guess;
- its PiP is the filler, and its audio is the game's alone.

Commit: `feat(core): the goals reel's plan`.

### Task 1.4: Chapters in exported files (C2, C3)

**Files:**
- `crates/video-coach-core/src/plan.rs`, `crates/video-coach-core/tests/plan.rs`
- `crates/video-coach-media/src/{chapters.rs (new),lib.rs,composite/export.rs}`
- `crates/video-coach-media/tests/export.rs`
- `crates/video-coach-app/src/bus/export.rs` (the log line)
- `packaging/build-deps.txt`
- `BACKLOG.md` (#23)

**What to build:**
1. **Core, `CompilationPlan::chapters(&self) -> Vec<(f64, &str)>`,** in `plan.rs`: one `(start_frame / OUTPUT_FPS, &entry.text)` per entry, and empty for a plan of fewer than two entries. No new core module.
2. **Media, `chapters::splice(path, &[(f64, &str)]) -> io::Result<ChapterOutcome>`, about 60 lines, as C3 describes.** It owns the format's limits:
   - at most 255 chapters, logging how many it dropped;
   - titles of at most 255 bytes, truncated on a `char` boundary.

   The splice:
   - Walk the top-level boxes, handling 64-bit `largesize` and size 0 ("to the end").
   - Find `moov` and the `free` box after it. Append a `chpl` (version 1, 4 reserved bytes, a `u8` count, and per entry a `u64` start in 100 ns units, a `u8` length and the UTF-8 title) to `moov/udta`, creating `udta` if there is none.
   - Grow `udta` and `moov` by the box's size and shrink `free` by the same. The `free` must be consumed exactly, or keep at least 8 bytes.
   - Write `[moov.start, free.end)` in place with one positioned write.
   - **`ChapterOutcome` is `Written(n)` or `Skipped(reason)`,** where the reason says why nothing was written: no chapters, no room, or `moov` after `mdat`.
3. **Wiring.** `run()` splices the `.part` with `job.compilation.plan.chapters()` after `export()` succeeds and before the rename.
   - `ExportDone.chapters: ChapterOutcome` joins the `bus: exported …` log line.
   - **An I/O error fails the export** and deletes the `.part`: a half-written `moov` is a corrupt file.
   - `Skipped` keeps the file without chapters, and the log line says why.
4. **`packaging/build-deps.txt`:** add `ffmpeg`, commented as test-only. It is the chapter test's independent reader (`ffprobe`), and nothing links or ships it. The README points at that file and needs no change. Add a dated note under BACKLOG #23 saying that ffmpeg is now a test-only build dependency, and that the `.deb` still doesn't depend on it.

**Test that must fail first:**
- **Media integration, `a_compilation_gets_a_chapter_per_entry`:** a real `Exporter` run with the encoder CI selects and `avenc_aac` audio, and three entries. Read it back with `ffprobe -v error -show_chapters -of json`: three chapters, with the plan's titles and `start_frame / 30` times to within 1 ms. Every frame still decodes (`fixtures::decode_counters`, or an `ffprobe -count_frames` count equal to `total_frames`). A missing `ffprobe` **fails** the test with a message naming the package. It never skips.
- **Unit tests on synthetic box bytes:** no `udta`, which the splice creates; a `free` too small, giving `Skipped` with the file byte-identical; a 1–7 byte remainder counting as no room; `moov` after `mdat`, where nothing is written; a 64-bit `mdat` header; the 255 cap; truncation on a `char` boundary (a title of multi-byte characters).
- **Core:** times from `start_frame`; fewer than two entries gives none.

**Verify:** the gate.

**CLAUDE.md:** under "Export burns in the overlay…":
- chapters are a hand-written `chpl` spliced into the reserved `moov` before the rename;
- a chapter's time is `start_frame / OUTPUT_FPS`, never a duration sum;
- `ffprobe` is the test's reader, and a test-only build dependency.

Commit: `feat(export): a chapter per entry in every exported file`.

### Task 1.5: The reel and its trims through the bus

**Files:**
- `crates/video-coach-app/src/{main.rs,bus/mod.rs,bus/export.rs,bus/scoreboard.rs}`
- `crates/video-coach-harness/tests/reel.rs` (new)

**What to build:**
1. **`export_targets`** adds an **"All goals"** row after the tag rows when the reel's plan has entries. **`ExportTargetRow.clips` becomes `entries`,** and `main.rs`'s detail line follows the rename in this task (Task 1.6 words it for the reel).
2. **`job()` for a clip-less entry** gives it `None` media. The missing-video refusal names the goal ("goal 3's game video is missing; relink it first"). There is no recording to check.
3. **`Command::SetReelTrim { goal: Uuid, end: ReelEnd, at: Option<(usize, f64)> }`,** with the position captured by the caller at the click (the bus contract).
   - It goes through `edit_match_events`, so it is an `EditMatchEvents` undo step, purged on a source move as Phase 9's are.
   - A refusal is a `UserError::Scoreboard` notice.
   - It is **not** on the recording allow-list.

**Test that must fail first (harness), wiring only** (core owns the plan's and the trims' rules):
- **`the_reel_exports_through_the_bus`:** a 3 s fixture video, two goals and no clips. The run writes `All goals - Game.mp4`, and its frame count is the plan's.
- The "All goals" row is absent with no goals, and present with one.
- A trim is set and undone, and a refused trim is a notice.
- An undo after a source move doesn't restore a stale trim.

**Verify:** the gate.

Commit: `feat(app): export the goals reel; trim a goal's span`.

### Task 1.6: The Match panel and the scrubber (R3, C1)

**Files:**
- `crates/video-coach-app/ui/{app.slint,scrubber.slint}`
- `crates/video-coach-app/src/{main.rs,match_panel.rs}`
- `crates/video-coach-app/tests/scrubber.rs`

**What to build:**
1. **`MatchRowText` gains `abs: f64` and `kind: MatchEventKind`.** The goal rows, the scrubber's marks and the chapter jumps are all built from `match_rows`, so there is no separate chapter type.
2. **The goal rows (R3).** Each goal row in the Match panel gets **"Reel starts here"** and **"Reel ends here"**, each with a reset, and shows its span (`−30 s / +6 s`).
   - `MatchRowText.reel_span: Option<String>` is computed in `match_panel.rs`, so it is tested headless.
   - The span sits on a second line under the goal row, because the 280 px column has no room beside it.
   - The buttons follow the row's existing `can-edit` gate.
   - **The capture.** The click's position is captured through `scan_abs` and `locate` in the callback, as `on_tag_match_event` does, then sent as `SetReelTrim`.
3. **The export sheet.**
   - **The Export… button** is enabled on `can-export`, set from `!export_targets(project, None).is_empty()` wherever `clip-count` is set, so it is enabled exactly when the sheet would have a row.
   - The reel row's detail reads "N goals · m:ss".
   - **The default ticks need no code:** the reel row is ticked unless the sheet was opened on a single clip, by `main.rs:536`'s existing rule.
4. **The scrubber's marks.** `Scrubber` gains `in property <[Mark]> marks`. Each mark is a `{ at: float, color: color }`, drawn as a thin tick over the track, built from `match_rows`:
   - a goal is in its team's `primary_color`, or the palette's accent colour with no scoreboard;
   - a start/stop is white;
   - pending suggestions join in P4, as hollow marks.

   Set the marks on `ProjectChanged`, not per tick.
5. **`[` and `]`.** `previous_chapter(abs, &rows)` and `next_chapter(abs, &rows)` in `match_panel.rs` **use one tolerance, `CHAPTER_TOLERANCE = 0.5` s:** `]` goes to the first row more than 0.5 s after the playhead, and `[` to the last row more than 0.5 s before it. So a press while sitting on a chapter, or a hair either side of it, moves on to the next one. The keys go in `handle-key` after the `text-editing` yield, take the same gate as a Match row's seek (`!recording && !previewing && can-play`), and send `ScrubRelease` to the row's `abs`.
6. **A screenshot pass** with a scratch project driven through callbacks: the panel with trimmed goals, the sheet with the reel row, and the scrubber with marks.

**Test that must fail first,** in `match_panel.rs`:
- `a_goal_row_shows_its_reel_span`: the defaults, one side trimmed, and no span on a start/stop row.
- `previous_and_next_chapter_skip_the_one_under_the_playhead`: with the two ends (nothing before the first, nothing after the last), and **a position a hair before a mark (811.999 for a mark at 812) steps past it** in both directions.

**Verify:** the gate, plus the screenshot pass.

**Hands-on (batched):** the reel, trim and mark items in Task 2.6's checklist section.

Commit: `feat(app): the reel row, goal trims, chapter marks and [ / ]`.

### Task 1.7: P1 closeout

1. **Adversarial review** of the P1 diff (the `adversarial-review` skill, CLAUDE.md's pattern). Apply, skip or defer.
2. **Backlog** what is deferred.
3. **Check that each task's CLAUDE.md addition is there** and still accurate.

Commit: `docs: close out P1 of match vision` (along with the review's fixes, in their own commits).

---

## P2: hand-placed highlights (format v9)

Order: 2.1, 2.2, 2.3, 2.4, 2.5, then 2.6 closes the phase and releases. 2.3 needs 2.2 for its export test, and 2.5 needs 2.3 and 2.4.

### Task 2.1: The highlight model (H1, H2), v9

**Files:**
- `crates/video-coach-core/src/{highlight.rs (new),lib.rs,project.rs,store.rs,undo.rs}`
- `crates/video-coach-core/tests/{highlight.rs (new),project_format.rs,sources.rs,undo.rs}`

**What to build:**
1. **The types** follow H2 exactly: `NormRect { x, y, w, h }`, `PlayerHighlight` and `HighlightKey` (with `tracked`), in `camelCase` and with **no serde defaults** (F2).
   - `Project.player_highlights: Vec<PlayerHighlight>` gets a field-level `#[serde(default)]`.
   - **Keys are not sorted on read.** The mutators keep them sorted, and `highlights_at` relies on it. Document both.
2. **The bump: `CURRENT_FORMAT_VERSION = 9`.**
3. **The mutators on `Project`, in `highlight.rs`, as `scoreboard.rs` does:**
   - **`set_highlight_key(id, source_index, color, key) -> Result<(), HighlightError>`.**
     - It creates highlight `id`, with `color` and an empty label, if there is none. The caller generates the id, so the UI can select a new highlight at once.
     - It refuses a key on a source other than the highlight's own.
     - **A key at exactly the same `source_seconds` as an existing one replaces it.** Keys are placed at the displayed frame's stream time (Task 2.5), so the same frame gives the same number, and no tolerance is needed.
     - Keys stay sorted.
   - **`delete_highlight_key(id, source_seconds)`**, by exact equality. Deleting the last key deletes the highlight.
   - **`delete_highlight(id)`.**
   - **`edit_highlight(id, HighlightEdit::{Label(String), Color(Rgba)})`.** A label goes through `normalize_label`: trimmed, and a label of digits only gets a leading `#` ("7" becomes "#7").
4. **`highlights_at(&[PlayerHighlight], source_index, t) -> Vec<VisibleHighlight<'_> { color, label, rect }>`:**
   - with two or more keys, visible over `[first, last]`, with the rect interpolated linearly between the keys either side;
   - with one key, visible over `[k − SINGLE_KEY_SPAN/2, k + SINGLE_KEY_SPAN/2]`, with `SINGLE_KEY_SPAN = 1.0`.
5. **`highlight_shapes(highlights, source_index, t, zoom: Zoom, picture_w, picture_h) -> Vec<HighlightShape>`,** the one piece of drawing geometry, used by both the media overlay (2.2) and the live layer (2.4). It runs `highlights_at`, maps each rect to picture pixels through `zoom.transform(pw, ph, pw, ph)` (the picture *is* the fitted source, so the source's own size is never needed), and returns `HighlightShape { color, label, rect, ellipse: (cx, cy, rx, ry), width }` in picture pixels, relative to the picture's origin (H4):
   - the ring is an ellipse 1.4 × the box's width wide and 0.35 × that tall, centred on the box's bottom edge;
   - the stroke width scales with the picture's height, like a pen;
   - the ring geometry is a private `highlight_ring`, with its ratios private.
6. **F4.**
   - `move_source` and `remove_source` remap `player_highlights`.
   - `source_is_referenced` counts highlights. (The message and the tooltip that name highlights are Tasks 2.3 and 2.5.)
7. **Undo.** `UndoAction::EditHighlights { before, after }` joins `purge_for_source_change`'s stale predicate, so it is purged from both stacks.

**Test that must fail first:**
- **`v7_and_v8_files_load_under_v9`** (the migration test: both versions, one with trims).
- The round trip.
- Interpolation at the ends and midway.
- The single-key span, including its edges.
- A second key at the same time replaces the first.
- Deleting the last key deletes the highlight.
- A key on another source is refused.
- Label normalization.
- Move and remove remap highlights, and a highlight blocks its source's removal.
- `EditHighlights` is purged from both stacks.
- **Holds through a freeze:** over a `compilation_schedule` with a commentary pause, every frame of the pause gets the same shape.
- **The zoom round trip:** a box placed at picture pixels through `Zoom::source_point` under a 2.5× zoom comes back from `highlight_shapes` at the same pixels.
- `highlight_shapes`: a centred box at identity maps to the picture's centre, and under a 2× zoom it maps per `transform`.

**Verify:** the gate.

**CLAUDE.md:** a "Player highlights" paragraph:
- a highlight belongs to the footage, keyed by `source_index` and the displayed frame's stream time, never by record time;
- it is drawn from `highlight_shapes`, which maps through `Zoom::transform` with no new mapping, in both the overlay and the live layer;
- it may be placed outside a recording, while pen drawings stay recording-only.

Commit: `feat(core): player highlights (format v9)`.

### Task 2.2: Drawing highlights in preview and export (H4, H5)

**Files:**
- `crates/video-coach-media/src/{overlay.rs,composite/export.rs,composite/preview.rs}`
- `crates/video-coach-media/tests/preview.rs`
- `crates/video-coach-app/src/bus/{export.rs,preview.rs}`

**What to build:**
1. **`OverlayFrame` gains `highlights: &[HighlightShape]`.** It stays zoom-agnostic: the shapes are already in picture pixels.
2. **`draw_highlights` runs first**, under the bar's tint, the strokes, the glyphs and the scoreboard.
   - **The ring:** the dark edge first, then the colour, as strokes are drawn, offset by the picture rect's origin.
   - **The label** is a pill of the highlight's colour above the box, drawn through `draw_label` with no memo slot, because it moves every frame. **`draw_label` bypasses the mask, so the pill is placed inside the picture rect:** below the box when there is no room above it, and shifted sideways at the left and right edges.
   - **The ring is clipped to the picture rect** with a `tiny_skia::Mask`.
3. **The jobs.** `ExportJob.highlights` and `PreviewJob.highlights` are `Vec<PlayerHighlight>`, a snapshot. Each driver calls `highlight_shapes(&job.highlights, entry.source_index, frame.source_time, frame.zoom, picture_w, picture_h)`. The bus fills both from `open.project.player_highlights`.

**Test that must fail first (overlay unit tests):**
- a ring's pixels are in its colour, inside the picture rect, near the box's bottom edge;
- a shape reaching past the picture's edge leaves nothing outside the picture rect;
- a stroke crossing a ring shows the stroke's colour where they cross;
- the label pill sits above the box;
- **a box at the picture's top edge puts its pill below the box, and a box at a side edge keeps its pill inside the picture rect**;
- with no highlights, the output is unchanged.

**Verify:** the gate. Also check that `render` still costs about the measured 3.6 ms a frame at 1080p, and note the figure in the commit.

Commit: `feat(media): draw player highlights in preview and export`.

### Task 2.3: Highlight commands and undo

**Files:**
- `crates/video-coach-app/src/bus/{mod.rs,highlights.rs (new),clips.rs,sources.rs}`
- `crates/video-coach-harness/tests/highlights.rs` (new)

**What to build:**
1. **The commands:**
   - `SetHighlightKey { id, source_index, source_seconds, rect, color }`, with `source_seconds` the displayed frame's stream time, captured by the caller at pen-down;
   - `EditHighlight { id, edit: HighlightEdit }`;
   - `DeleteHighlightKey { id, source_seconds }`;
   - `DeleteHighlight(Uuid)`.
2. **Undo.** Each is one `EditHighlights` step, through an `edit_highlights` helper shaped like `edit_match_events`. Undo and redo apply the whole list in `clips.rs`'s replay.
3. **While recording,** `SetHighlightKey` joins the allow-list (H3). The rest wait, as every other edit does.
4. **`UserError::SourceReferenced`'s message** names highlights along with clips and match events.

**Test that must fail first (harness):**
- A key creates a highlight, and a second key at the same time replaces it.
- "Delete key" on the last key deletes the highlight.
- A label "7" is stored as "#7".
- Undo and redo.
- **While recording,** `SetHighlightKey` lands and `EditHighlight` is refused.
- A source with a highlight can't be removed, and the notice names highlights.
- An undo after a source move doesn't restore a stale index.
- **A highlight reaches an export:** the ring's colour is at the expected pixels of the exported frame at its key time.

**Verify:** the gate.

Commit: `feat(app): highlight commands and their undo`.

### Task 2.4: Rings on the scan and recording picture (H5)

**Files:**
- `crates/video-coach-app/src/{highlight_view.rs (new),lib.rs,main.rs}`
- `crates/video-coach-app/ui/app.slint`

**What to build:**
1. **`highlight_view::live_highlights(project, source_index, source_secs, zoom, content_w, content_h) -> Vec<LiveHighlight { commands, ink, label, label_x, label_y }>`**, from `highlight_shapes` with the content rect as the picture.
   - The ellipse is two SVG arcs in content-rect pixels.
   - This is pure code, so it is tested headless.
2. **The tick.** `tick` computes the rings from `scan_abs` → `locate` and the UI's zoom. It sets the model **only when it changed**, because Slint re-parses every path.
3. **The Slint layer.** Path elements plus a label pill go on the live layer, inside the content clip and **under** the strokes. They are hidden while previewing, whose picture already carries the rings.

**Test that must fail first:** unit tests in `highlight_view.rs`:
- a centred box at identity gives arcs around the content's centre;
- the same inputs give byte-equal commands (which the change check relies on).

**Verify:** the gate, plus a screenshot with a hand-written highlight in a scratch `project.json`, at 1× and zoomed.

Commit: `feat(app): show highlights on the live picture`.

### Task 2.5: The H tool and the highlight inspector (H3)

**Files:**
- `crates/video-coach-app/ui/app.slint`
- `crates/video-coach-app/src/{main.rs,video.rs,highlight_view.rs,zoom_input.rs}`

**What to build:**
1. **The displayed frame's time.** `video.rs` records the `stream_time` of each scan frame it draws, so the UI knows which frame is on screen. A key is placed at that time, on `ui.source_index`, which makes it exactly the frame `Decoder::frame_at` picks for it (Task 0.1). A frame with no stream time falls back to `scan_abs` → `locate`.
2. **Entering and leaving the tool.**
   - **`H` toggles the tool.** It is gated as the tag keys are (`!event.repeat && !previewing && can-play && recording-phase != starting`) and yields to `text-editing`.
   - **Esc:** in `handle-key`, the tool's Esc goes **before** the recording's. The first Esc deselects a highlight, and the next leaves the tool. Esc in the tool during a paused recording never stops the take.
3. **Its own `TouchArea`** over the content rect, enabled only in the tool and placed after the draw area so it takes the press. A rubber-band `Rectangle` follows the drag.
4. **A drag.** On press, the displayed frame's time is captured. If the picture is playing, the drag places nothing and shows **"Pause to place a highlight (Space)"** in the drawing hint's slot. Otherwise the release does the following:
   1. **The rect.** Both corners go to content fractions, then through `Zoom::source_point`, normalized to min/max and clamped to `[0, 1]`: `highlight_view::drag_rect`.
   2. **The target.** It is the highlight whose ring (or box) the press started on, if it shows at this frame. Otherwise it is the selected highlight, **if it is on this source and the paused frame is within 10 s of its range.** Otherwise it is a new id: `highlight_view::target_for_drag`, over `hit_test`.
   3. **The command.** `SetHighlightKey` goes out with the current pen's colour, and the target becomes selected.

   A drag under 6 px is a click: it selects the ring under it, or does nothing. `zoom_input::drawing_hint` never shows in the tool.
5. **A Highlights panel** under the Match panel.
   - Each row shows a colour dot, the label and a delete.
   - **The selected row** adds a label field, which folds into `text-editing` and commits on focus loss, like a clip's name. It also adds **"Delete key here"**, enabled when the displayed frame's stream time equals one of its keys'.
   - **Clicking a swatch** with a highlight selected in the tool also sends `EditHighlight::Color`, except while recording.
   - The panel's edits and the swatch recolour are disabled while recording: only placing keys is allowed then.
6. **The source row's tooltip** names highlights among what keeps a source from being removed.

**Test that must fail first:** unit tests in `highlight_view.rs`:
- `drag_rect` under zoom (it matches Task 2.1's round trip), and with the corners reversed;
- `hit_test` on a ring and off it;
- `target_for_drag`: the ring under the press, the selected highlight within 10 s of its range, a selected highlight more than 10 s from its range (a new one), a selected highlight on another source (a new one), and nothing selected (a new one).

**Verify:** the gate, plus a screenshot pass: the tool with a selected highlight and the inspector open.

Commit: `feat(app): the highlight tool and inspector`.

### Task 2.6: P2 closeout and the release

1. **Adversarial review** of the P2 diff. Apply, skip or defer, and backlog what is deferred.
2. **Version `0.2.0`.** Build with `flock /tmp/claude-1000/cargo.lock nice -n 19 packaging/build-deb.sh`, run `packaging/smoke-test.sh`, and copy the `.deb` to `~/Downloads`. Stage `Cargo.toml` and `Cargo.lock` explicitly. Tagging `v0.2.0` and pushing it is the user's call.
3. **`docs/hands-on-checklist.md`:**
   - update §1's install line to `0.2.0`;
   - add **§11, "Goals reel, chapters and highlights"**, in the checklist's own voice. The items are below.

   **§11 items:**
   - **[must work] Old projects open.** A project from 0.1.x opens and plays. *Expected:* once 0.2.0 has saved it, `project.json.v7` sits beside it, and 0.1.x refuses the new `project.json`.
   - **[must work] Scrub lands.** On a Trace half, scrub to a few places. The readout and the picture agree (P0).
   - **Frame steps.** Paused, `.` and `,` move one frame forward and back, and the readout follows. They do nothing while playing.
   - **Reel row.** With goals tagged, the export sheet has **All goals** ("N goals · m:ss"). It is absent with none. Export… works in a project with goals and no clips.
   - **Reel file.** `All goals - <project>.mp4` has one piece per goal, each about 36 s. Check that:
     - the caption reads `n / total | Team goal | h-a`;
     - the board's score turns over on the goal's frame;
     - there is no webcam inset;
     - there is game sound, with no commentary;
     - a highlight placed during a goal's build-up shows in that piece.
   - **Trims.** On a goal row, **Reel starts here** and **Reel ends here** set from where you are. Check that:
     - the span text updates;
     - reset restores the defaults;
     - a start after the goal is refused;
     - Ctrl+Z undoes.
   - **[must work] Chapters.** Open the reel, and a multi-clip export, in VLC (Playback → Chapter): one chapter per goal (or per clip), with the caption's text. **Tell us** where the coach's audience watches (Q5).
   - **Marks and jumps.** The scrubber shows a tick per goal (team colour) and per period (white). `]` and `[` jump between them, and do nothing while recording.
   - **[must work] A highlight.** Press **H**, pause, and drag a box around a player: a ring at the feet with a label pill. Then:
     - type `7` in its label, which reads `#7`;
     - play a second on, pause, drag again, and play from before the first key: the ring glides between the keys;
     - zoom in: the ring stays on the player;
     - check it in a preview and in an export;
     - a drag while playing only shows the hint.
   - **While recording** [cam+mic]: pause, press H and ring a player. It lands, and Esc leaves the tool without stopping the take.
   - **Edit and undo.** Delete key here, delete, recolour from a swatch, and Ctrl+Z each. A source with a highlight on it can't be removed.
   - **Your call:** the ring's size and thickness, the pill's legibility at full screen, and whether hand keys a second apart are tolerable on a panning shot (that is P6's reason to exist).
4. **CLAUDE.md:** confirm the P1 and P2 additions.

Commit: `chore: 0.2.0, with the goals reel, chapters and highlights`.

---

## P3–P7: outlines only

Each of these gets its own detailed plan, written once its entry gate is met, with its own spec review where the spec left a choice to the measurements. The G4 bars below are on the **held-out** matches (G2).

### P3: Measure (spec G5)

**Entry gate:** P0 has shipped, and one match is fully tagged, with `kickoffs.txt` and a local copy (user steps 2–5). The signal passes, the tuning and V-2 to V-8 need only that. V-1's held-out numbers and every G4 verdict wait for the second match.

**Main tasks:**
1. **The one-`Finished` job helper (B1).** It turns a panic into `Failed`, and `Transcriber` moves onto it, which closes BACKLOG #64. It depends on nothing measured, and can be pulled forward to any point after P2.
2. **Media:** the audio pass (the existing `Reader`) and the motion pass (`decodebin3` → `videorate` at 5 fps before `glupload` → GL scale to 160×90 → `gldownload`, on `Gl::shared()`), and `Analyzer` with its per-frame cancel.
3. **Core:** `whistles` (the Goertzel bank), `cheers`, `still_intervals`, the kick-off pattern, the confirmation rule, and the 10 s de-duplication. All are tested on synthetic input.
4. **Harness:** the `kickoffs.txt` reader, `src/score.rs`, and `tests/ground_truth.rs` (`#[ignore]`d):
   ```bash
   COACH_GROUND_TRUTH=/local/match-a:/local/match-b flock /tmp/claude-1000/cargo.lock nice -n 19 \
     cargo test -p video-coach-harness --test ground_truth -- --ignored --nocapture --test-threads=1
   ```
5. **The runtime and detector spike (L3, V-2, V-7):**
   - `rten` against `ort` (`load-dynamic`) on D-FINE-N and RTMDet-tiny, at 640 and 960, on 4 threads, on AC, over minutes;
   - far-side recall;
   - a cold snap;
   - the ONNX export script under `tools/`.
6. **The spike doc,** `docs/superpowers/spikes/<date>-match-vision-measurements.md`, with aggregate numbers only.

**Exit:**
- V-1 to V-8 are answered.
- The runtime is chosen by L3's rule.
- The G4 goal, seek and period numbers from sound and motion alone are recorded.
- **P5 is decided,** built or skipped.

### P4: Suggestions from sound and motion (format v10)

**Entry gate:** P3's held-out numbers clear the G4 bars for periods and goals on sound and motion alone. If the quiet tier fails its bar, P4 ships the high tier and the periods only, and P5 (if built) brings the rest.

**Main tasks:**
- **The format:** `MatchSuggestion` (v10, with a v7–v9 migration test). F4's remap applies, and removing a source deletes its suggestions.
- **Resolution, derived:** an event resolves the earliest suggestion it lands in, and undo un-resolves it.
- **The scheduler (B2):** one slot per kind, run in the order tracking, transcription, analysis. Recording, export and preview block every job. Preemption follows B2's rules.
- **Commands:** `Analyze` and `SetSuggestionDismissed`.
- **UI:**
  - the Match panel's "Find goals and kick-offs", with its progress, and its suggestion rows (Seek, Dismiss/Restore);
  - the scrubber's hollow marks;
  - the export sheet's "N suggested goals not confirmed".
- **Harness:** preemption by recording, a panicking job, and a re-run's de-duplication.

**Exit (G4):**
- **Goals:** all tiers reach recall ≥ 90% with no held-out match missing more than one, and precision ≥ 70%. The high tier reaches precision ≥ 90%. The quiet tier reaches ≥ 40%, or is hidden.
- **Seek:** 90% of matched high-tier goals lie within 20 s of their Seek point. Otherwise Seek goes to the window start.
- **Periods:** recall ≥ 90% and precision ≥ 80%.
- **Throughput and UI:** analysis takes ≤ 5 min per half on AC, a cancel takes ≤ 0.5 s, and scanning holds full rate during an analysis.

### P5: The formation check (D5; built only if P3 says so)

**Entry gate:**
- P3 shows the goal precision bars fail on sound and motion alone.
- V-2 shows the virtual camera frames kick-offs often enough for the test to see both kits.
- The detector and runtime are pinned, with each licence and hash re-verified at pin, including the backbone's ancestry (Risk 7).

**Main tasks:**
- **The model:** hosted as a release asset (L4) and downloaded through the existing downloader with `fetch` as the permission, under a "Download N MB and find goals" prompt. The README says that it downloads, keeping BACKLOG #23's "nothing leaves the machine after that" claim true.
- **Media:** detector inference, and torso-colour sampling.
- **Core:** k-means (k = 2) with outlier rejection, and the separation test (`MIN_PER_KIT` from V-2).
- **The analysis:** drops failing kick-offs, with no reduced pass without the model.
- **A test detector kind** for CI.

**Exit (G4):** the goal precision bars are met, and analysis still takes ≤ 5 min per half.

### P6: Click-to-track

**Entry gate:**
- The detector and runtime are in place (from P5, or brought in here).
- V-7's cold snap is ≤ 1 s, or the warm-session fallback is planned.
- **At least 10 hand-keyed 10 s ranges** exist in the coach's projects, as truth from P2.

**Main tasks:**
- **The snap-to-player click (T1).**
- **`Tracker` (T2):**
  - one frame in flight, cropped by `gltransformation`;
  - the SORT-lite Kalman filter and gate in core;
  - `TRACK_KEY_TOLERANCE` thinning;
  - a stale result is dropped.
- **B2's preemption:** a track preempts an analysis, but never whisper.
- **UI:** Track, "Track to here", "lost at", and the "Track" undo step.

**Exit (G4):**
- **Tracking:** the centre lies inside the coach's box on ≥ 90% of interior hand keys, over ≥ 10 ranges, with at most one switch per 30 s.
- **Throughput:** tracking runs at ≥ 0.5× realtime, a cancel takes ≤ 0.5 s, and a cold snap takes ≤ 1 s.

### P7: Jersey numbers (research-gated)

**Entry gate:**
- P6 has shipped.
- About 40 labelled tracks exist from the coach's own P2 and P6 highlights.
- Each candidate's training-data question (L2) is checked.

**Main tasks:**
- A spike scoring PARSeq, a digit model we train, and `ocrs`.
- If one clears the bar: OCR over the tracked crops, with tracklet voting (≥ 5 readings, ≥ 70% agreeing), filling empty labels only.

**Exit (G4):** a filled label is right ≥ 95% of the time, and ≥ 30% of tracks are filled. Otherwise P7 ships nothing and the label stays typed.

## Deliberately not in this plan

Everything in the spec's Deferred list, including Q6's whole-match export and QuickTime `chap` tracks (Q5), until the user says otherwise.
