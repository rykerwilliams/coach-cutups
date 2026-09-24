# P3 Plan — Measure: can sound and motion find the goals?

**Date:** 2026-09-24
**Spec:** `docs/superpowers/specs/2026-09-22-match-vision-design.md`, decisions **D** (suggested kick-offs, goals and periods), **B** (jobs, threads and cancelling), **L** (models, runtimes and licences) and **G** (ground truth, scoring and acceptance, V-1…V-8 and the G4 bars). This plan is the P3 row of that spec's phase table and of `docs/superpowers/plans/2026-09-22-match-vision.md`.
**Status:** the entry gate is **met**. Three matches are hand-tagged and on local disk; a fourth is held back untagged.

**What P3 is.** The analysis backend with **no user-facing suggestions**: the audio pass, the motion pass, an `Analyzer`, core's signal types, the kick-off pattern, the confirmation rule, and a scoring tool that grades detections against the coach's own tags. It stores nothing, adds no command, draws nothing and touches no `.slint` file. Its output is numbers, and the numbers decide whether P4 (showing suggestions) is worth building and which cue carries the weight.

**What P3 is not.** No `MatchSuggestion` in the project format, no bus command, no scheduler, no Match-panel row, no scrubber mark. All of that is P4's, and P4's format bump is now **v12** (`CURRENT_FORMAT_VERSION` is already 11; the spec's D6 says v10 and is stale on that one number).

---

## Execution conventions

A fresh subagent per task (`superpowers:subagent-driven-development`), given this plan, the spec and `CLAUDE.md`.

- Tasks run one at a time, in the order written, in one tree. The cargo lock serialises every build anyway.
- The orchestrator runs the gate and commits each task, staging paths explicitly, never `git add -A`.
- **At the end of every task the workspace builds and every test passes.**
- **The gate, for every task:** the `verify` skill, with every cargo call under the machine-wide lock (`flock /tmp/claude-1000/cargo.lock nice -n 19 cargo …`), because other sessions build here too.
- **Test-first.** Each task names the test that must fail first. Write it, run it, watch it fail for the stated reason, then build until it passes.
- **What tests may touch.** No test reaches the network, the real camera or mic, or the coach's footage. The one exception is the `#[ignore]`d `COACH_GROUND_TRUTH` test this plan builds. CI never sees the footage: the ground-truth test is `#[ignore]`d, and every unit test in core and media runs on synthetic input.
- **Nothing identifying is ever committed.** The repository is public and the footage shows children. No team name, no folder name, no file name, no shirt number — in code, tests, commit messages, the spike doc or this plan. The matches are **A**, **B** and **C** throughout, and the spike doc carries aggregate numbers only.
- **The coach's folders are read-only.** The ground-truth tool opens each project with `store::read` and nothing else. It never constructs a `Bus`, never calls `store::write`, and writes no file of its own inside a project folder. A task that needs scratch output writes it under `$TMPDIR`.

---

## Known facts, measured while writing this plan. Don't re-derive these.

Everything in this section was measured on the three tagged matches on 2026-09-24, with `ffprobe`, `ffmpeg` band filters and a 5 fps 160×90 luma-difference probe. The band filters are crude stand-ins for what core will compute (four-pole Butterworth sections and a block median, not a Goertzel bank and a rolling median), so treat the **directions** as facts and the **exact thresholds** as things the tasks below re-measure properly.

### The three matches, by their properties

| Match | Files | Half length (played) | Goals | Period tags | Whistle-band noise floor |
|---|---|---|---|---|---|
| **A** | 2 | 1516 s, 1522 s | 3 + 3 | 4 | quiet (band median ≈ −79 dB) |
| **B** | 2 | 1510 s, 1506 s | 3 + 4 | 4 | very quiet (≈ −83 dB) |
| **C** | 2 | 1817 s, 1812 s | 2 + 1 | 4 | loud (≈ −52 dB) |

**Sixteen goals and twelve period events in total.** All six files are 1920×1080 H.264 at ~5.0 Mbit/s with AAC-LC 48 kHz stereo, as the spec describes.

Three assumptions in the spec need correcting against this:

1. **Not every half is 27 minutes.** Match C's halves are **30-minute** halves in ~33-minute files (`regulationPeriodSeconds` is 1800, not 1500). Every per-half budget in the spec — the 5-minute throughput bar, the motion pass's 48,500 frames — is a per-27-minute figure and must be stated per *file length*, not per "half".
2. **Match C's audio is ~27 dB louder in the whistle band than A's and B's.** No absolute level threshold can exist. Everything is relative to a rolling median, as D2 already says — but the *contrast* also differs between venues, so a threshold tuned on one match is not obviously portable, which is exactly what G2's tuning/held-out split is there to expose.
3. **`kickoffs.txt` does not exist yet** in any of the three folders. V-3 (walk-back durations) and the per-goal restart diagnostic depend on it. The tool must run **without** it, scoring everything the G4 bars need and printing "restart unknown" in the diagnostics.

### What the tags alone say (no detector involved)

| Question | Measured |
|---|---|
| Does each file contain its half's opening kick-off and final whistle? | **Yes, all six.** Lead-in before the first tag: **52–117 s**. Tail after the last tag: **44–101 s**. This answers **V-4**: `auto_back_anchor_p1` is not needed on this footage, and a period detector has real signal at both ends of every file. |
| Closest two goals | **192 s** apart (one file in match B). Every other pair is ≥ 285 s. So D4's `W = 150 s` cannot make two goal windows overlap on this data, and the `K_prev` clamp is belt-and-braces rather than load-bearing — but it stays, because it is exact and free. |
| Shortest gap from a period start to the first goal | **65 s.** So a goal's window *can* be clipped by `K_prev`, and the clamp earns its place at least once. |
| Shortest gap from a goal to the end of its period | **113 s**, and only one other is under 200 s. Every goal in the set has room for a restart before the whistle, so **no truth goal is structurally undetectable by a kick-off-anchored rule.** |
| Goals per file | 1, 2, 3, 3, 3, 4 |

**Which match tunes.** G2 says the first folder in `COACH_GROUND_TRUTH` is the tuning match. Use **match B** (7 goals, the most). That leaves **9 goals across two held-out matches**, and keeps the 3-goal match held out, where G4's "no held-out match missing more than one goal" is the binding constraint rather than the 90% rate.

**Read the bars with the counts in hand.** A held-out set of 9 goals means one miss is 89% recall — already under the bar. The counts matter as much as the rates (G2 says so); the report prints both, always.

### What the signals look like (one half, probed end to end)

Measured on one whole 1516 s half of match A, which has 3 goals at 412 s, 697 s and 1122 s and period tags at 52 s and 1568 s.

- **The cheer is the strong cue.** In the 0.3–3 kHz band, over a 60 s block median, an excursion of **≥ 8 dB held for ≥ 1.0 s** fires **6 times in the half** and covers **all three goals** (each within 1.4 s of its tag) plus one restart. The spec's initial constants (≥ 8 dB for **≥ 1.5 s**) fire only 3 times and **miss the third goal**, whose burst lasts 1.4 s. **The 1.5 s floor is the wrong initial value; start at 1.0 s.** Loosening to ≥ 15 dB / 0.5 s still covers all three goals but fires 12 times.
- **A goal's cheer is enormous and nearly simultaneous with the tag:** +25 to +46 dB over the block median, onset 0.4–1.4 s *after* the coach's frame. D4's `at = cheer onset − 1 s` therefore lands within about a second of the ball crossing the line, which is a good sign for the Seek bar.
- **The restart also makes a cheer** (one measured at +44 dB for 2.6 s, 60 s after its goal). D4's `K − 15 s` clamp on which cheers count is what keeps that out of the `at` estimate. That clamp is now measured, not assumed.
- **The whistle band is useless as a level detector.** In the same half, the two loudest 2–4.5 kHz events are the two loudest *cheers* — a shout is broadband and leaks straight into the whistle band. The half's final-whistle tag has no level excursion near it at any threshold tried. **D2's pitch-hold is not a refinement, it is the whole detector,** and it needs a companion: the peak bin must dominate the rest of the band (tonality), or every shout passes.
- **Stillness alone is weak.** The ≥ 10 s still-interval rule yields **19–21 intervals per half at every threshold from θ = 2 to θ = 6** — one per 75–80 s of match — against 5 truth events in that half. That is a **precision ceiling near 20–25%** for motion alone. The spec's D3 says "throw-ins and free kicks … most never reach the 10 s floor"; measured, that is **false on this footage**, and it is the single most important thing P3 has to confirm or refute across all six halves.
- **The long stillness sits after the goal, not before the restart.** For the one goal traced second by second, the picture went still for 15 s starting 10 s *after* the goal (the hold on the celebration), then moved again 35 s before the actual restart, which was preceded by only ~5 s of calm. So `K` taken as "the end of the still interval" is **not** the restart; it is roughly *goal + 25 s*. That still puts the goal inside `[K − W, K]` — the window rule survives — but it means **`K` is not a kick-off time**, and anything that reads `K` as one (the period-start rule, the `K − 15 s` cheer clamp, the coach's Seek point) is measuring something else.
- **One opening kick-off was missed by the pattern entirely:** no still interval of ≥ 10 s ended anywhere near that half's period-start tag, because the virtual camera keeps moving while the teams line up.

**The prediction these add up to, which P3 must confirm or refute:** the high tier (cheer, then a kick-off) is viable; the quiet tier (kick-off, no cheer) will be roughly 17 false candidates per half against 0–1 true ones and will fail its 40% bar by a wide margin. D3 already leaves it to P3 whether the cheer **gates** candidates (V-1, V-5). This measurement says it must, and Task 3.5 decides it with the numbers in hand rather than by argument.

### Facts checked in the code

- `composite::audio::Reader::start(path, rate, channels, cancel)` already decodes any file to F32 at an arbitrary rate and channel count, and `transcribe.rs` already calls it at 16 kHz mono. The audio pass adds **no new decode path**; it is a third caller.
- `Reader::start` has a 10 s `START_TIMEOUT` and `Reader::read` takes the `&AtomicBool` cancel. Both are what the analysis needs.
- `scoreboard::interpret(events, config)` takes **absolute** events, filters to `StartStop`, sorts, optionally back-anchors, truncates to `expected_start_stop_events()` and assigns `PeriodRole::Start(p)` / `End(p)` by position. It is the only correct way to learn which tag is a start and which is a stop. The tool maps each interpreted event back to `(source_index, seconds)` with `Project::locate`, so a match whose halves share one file still works.
- `Project::abs_seconds` / `Project::locate` are the source↔absolute mapping.
- `probe::probe` gives `duration_seconds` per source; the census needs it for the tail figure.
- `video-coach-harness` already depends on `video-coach-media` (with `fixtures`) and `video-coach-core`, so the ground-truth test needs no new dependency.
- `CURRENT_FORMAT_VERSION` is **11**, `MIN_READABLE_FORMAT_VERSION` is 7. The three tagged projects are v11 and must stay readable and unwritten.
- `Gl::shared()` is the process-wide surfaceless EGL display the export uses. The motion pass runs there (B1), never on Slint's context.
- `video-coach-core` declares **no media dependency** and the audit expects exactly `serde`, `serde_json`, `thiserror`, `uuid`. The Goertzel bank is hand-written for that reason; no FFT crate.

---

## Where the work lives

| Crate | What P3 adds |
|---|---|
| `video-coach-core` | `signals.rs`: `whistles`, `cheers`, `still_intervals` and their constants. `kickoff.rs`: the kick-off pattern, the confirmation rule and the 10 s de-duplication. Pure functions on `&[f32]` and `&[f32]` number series. No new dependency. |
| `video-coach-media` | `job.rs`: the one-`Finished` job helper (B1). `analyze/`: the audio pass (a third `Reader` caller), the motion pass (`decodebin3` → `videorate` 5 fps → GL scale to 160×90 → `gldownload` → appsink, on `Gl::shared()`), and `Analyzer`. |
| `video-coach-harness` | `src/truth.rs` (read a tagged project and its optional notes), `src/score.rs` (pure scoring), `tests/ground_truth.rs` (`#[ignore]`d). |
| `video-coach-app` | **Nothing.** |

**How a developer runs it:**

```bash
COACH_GROUND_TRUTH=/local/match-b:/local/match-a:/local/match-c \
  flock /tmp/claude-1000/cargo.lock nice -n 19 \
  cargo test -p video-coach-harness --test ground_truth -- --ignored --nocapture --test-threads=1
```

`:`-separated project folders, **first is the tuning match**, the rest held out. The folders are already on local disk, so no copying step is needed — but they are the coach's only copy, so the read-only rule above is not a formality. `--test-threads=1` because each run decodes six whole halves and two analyses at once would fight over the decoder and ruin the timing lines.

---

## Tasks

Order: 3.1, 3.2, 3.3, 3.4, 3.5, then 3.6 — and **3.6 runs only if 3.5's numbers say it must** (see its entry condition). 3.1 comes first so every later task reports against it.

### Task 3.1: The ground-truth reader, the scorer, and the tag census

The piece everything else is judged by, so it is built and pinned before any detector exists. At the end of this task the ground-truth test runs end to end against the three matches and scores an **empty** detection set — precision undefined, recall 0 — and prints the census.

**Files:**
- `crates/video-coach-harness/src/{lib.rs,truth.rs,score.rs}` (two new)
- `crates/video-coach-harness/tests/{score.rs,ground_truth.rs}` (both new)

**What to build:**

1. **`truth.rs` — reading a tagged match.**
   - `Truth { name: String, sources: Vec<PathBuf>, durations: Vec<f64>, events: Vec<TruthEvent> }`, and `TruthEvent { source_index: usize, seconds: f64, kind: TruthKind }` with `TruthKind { Goal, PeriodStart, PeriodEnd, Restart }`.
   - `Truth::read(folder) -> Result<Truth, TruthError>`: `store::read` the project; take every `MatchEventKind::{HomeGoal, AwayGoal}` as a `Goal`, with the side deliberately thrown away (Q9: the side is not detected and not scored); run `scoreboard::interpret` over the **absolute** start/stops with the project's own `ScoreboardConfig`, and map each `PeriodRole::Start/End` back through `Project::locate` to a `(source_index, seconds)` `PeriodStart` / `PeriodEnd`.
   - `name` is **not** the folder name. It is `"A"`, `"B"`, `"C"` … assigned by position in `COACH_GROUND_TRUTH`, so nothing identifying can reach the terminal or a pasted report. The folder path is never printed.
   - **`kickoffs.txt`, if present** beside `project.json`: one restart per line, `<1-based source index> <mm:ss>`, `#` starts a comment, blank lines ignored, a `# missing` line tolerated. Each becomes a `Restart`. **An absent file is not an error**; it costs the restart diagnostics and V-3, nothing else.
   - A malformed line is an error naming the line number, not a silent skip — a mistyped restart would quietly distort V-3.

2. **`score.rs` — pure scoring, no I/O.** `score(truth: &[TruthEvent], found: &[Detection]) -> ScoreReport`, with `Detection { source_index, seconds, kind: DetectionKind }` mirroring the spec's `SuggestionKind` (`Goal { tier, window, at }`, `PeriodStart`, `PeriodEnd`) but owned by the harness, so P3 needs no format change.
   - **Matching, per the spec's G3 table:**

     | Kind | Matches a truth when |
     |---|---|
     | Goal | the truth goal lies inside the detection's `window`, on the same source |
     | Period start / end | same source and `\|seconds − truth\| ≤ PERIOD_TOLERANCE` (10 s) |

   - **Pairing is one-to-one.** Goals: one run's windows are disjoint (D4), so a truth goal matches at most one window; a window holding two truth goals takes the earlier and the later counts as a false negative. Periods: greedy by nearest time, each truth and each detection used once, ties broken by earlier detection.
   - **Counting:** true positives, false positives, false negatives, precision (`tp / (tp + fp)`, `None` when the denominator is 0 — never 0.0, which reads as a failure), recall, **per kind, per tier, per match, and aggregated over the held-out matches**.
   - **`PERIOD_TOLERANCE` is a named constant in `score.rs` with a comment saying it is a bar on the *coach's* tag accuracy as much as the detector's**, and that Task 3.3 measures the coach's own offset before it is trusted.
   - Also computed, because they retune the constants without another five-minute run: for each matched goal, the truth's distance from its `window.start` and from `K`; for each matched period event, the signed error.
   - **The Seek bar:** for each matched high-tier goal, whether the truth lies in `[seek, seek + 20 s]`, with `seek` from D6.

3. **The report, printed by `ScoreReport::print`.** One fact per line, stable `key=value`, grep-able prefixes, so two runs diff with `diff`:

   ```text
   TAGS   match=A files=2 goals=6 periods=4
   TAGS   match=A src=0 dur=1623 start=52 stop=1568 lead_in=52 tail=55 goals=3 headroom_min=446
   GAPS   goal_to_goal_min=192 goal_to_stop_min=113 start_to_first_goal_min=65
   RUN    tuning=B held_out=A,C  cheer_snr=8.0dB cheer_min=1.0s still_theta=3.0 still_min=10.0s W=150.0s …
   SCORE  set=held_out kind=goal   tier=all  tp=13 fp=5 fn=3 p=0.72 r=0.81
   SCORE  set=held_out kind=goal   tier=high tp=9  fp=1 fn=7 p=0.90 r=0.56
   SCORE  set=held_out kind=period_start     tp=5  fp=1 fn=1 p=0.83 r=0.83
   SCORE  set=per_match match=C kind=goal tier=all tp=2 fp=1 fn=1 p=0.67 r=0.67
   MISS   match=C src=0 goal=778.3 still=none whistle=none cheer=none restart=unknown
   NEAR   match=A src=0 cheer=274.0 peak=+17dB reason=no_kickoff_within_W
   ```

   - **`RUN` prints every constant the run used**, so a pasted report is reproducible without the commit hash.
   - **`SCORE set=per_match` rows for every match**, because "no held-out match missing more than one goal" is a per-match bar.
   - The `MISS` and `NEAR` lines are placeholders in this task (no detector yet) and get their content in 3.3–3.5.

4. **`tests/ground_truth.rs`**, `#[ignore]`d, module doc naming the env var and the command. It reads every folder in `COACH_GROUND_TRUTH`, prints `TAGS` and `GAPS`, and calls `score` with an empty detection list. It **panics with a clear message** if the variable is unset. It asserts nothing about rates yet; it asserts that every project read, that every source file named by a project exists, and that the census numbers are finite.

**Test that must fail first:** `crates/video-coach-harness/tests/score.rs`, on hand-built truth and detection sets, running on CI with no footage:
- a goal inside a window is a true positive and one outside is a false negative plus a false positive;
- **two truth goals in one window score 1 tp + 1 fn**, not 2 tp — the pairing rule, and the one that silently inflates recall if it is wrong;
- a period event at exactly 10.0 s error matches and one at 10.1 s does not;
- greedy period pairing with two detections near one truth uses the nearer and calls the other a false positive;
- precision with no detections at all is `None`, and the printed line says `p=n/a`, not `p=0.00`;
- a `kickoffs.txt` with a comment, a blank line and a `# missing` line parses to the right restarts, and a malformed line is an error naming its line number.

**Verify:** the gate. Then run the ignored test against all three folders and paste the `TAGS`/`GAPS` block into the task's report. Confirm `git status` shows nothing changed under any project folder.

Commit: `feat(harness): ground-truth reader and the detection scorer`

---

### Task 3.2: One `Finished` per job (spec B1), closing BACKLOG #64

Small, independent of every measurement, and it has to exist before `Analyzer` does — a vision job indexes slices on data, which is #64's own trigger.

**Files:**
- `crates/video-coach-media/src/job.rs` (new), `src/lib.rs`
- `crates/video-coach-media/src/transcribe.rs`
- `crates/video-coach-media/tests/` (a new test file, or the existing transcribe test file)
- `BACKLOG.md`

**What to build:**
- One helper that spawns a job thread and guarantees **exactly one terminal message**: the job's own `Finished`, or a `Failed` carrying the panic's message when the body unwinds. Generic over the message type through a small trait or a closure pair — whichever is the smaller reshape of `Transcriber`; do not invent a job framework.
- `Transcriber` moves onto it, with no behaviour change.
- Remove BACKLOG #64 and note the closure in its place.

**Test that must fail first:** a media test with a job body that panics: the caller receives exactly one terminal message, it is `Failed` with the panic's text, and the channel then closes. Written against `Transcriber`'s existing test kind so it needs no model and no footage.

**Verify:** the gate. The whole transcription suite still passes.

Commit: `fix(media): every job thread sends exactly one Finished (closes #64)`

---

### Task 3.3: The audio pass, and the signals it feeds

**Files:**
- `crates/video-coach-core/src/{signals.rs,lib.rs}`, `crates/video-coach-core/tests/signals.rs` (new)
- `crates/video-coach-media/src/analyze/{mod.rs,audio.rs}` (new), `src/lib.rs`
- `crates/video-coach-harness/{src/score.rs,tests/ground_truth.rs}`

**What to build:**

1. **Media's audio pass**, `analyze::audio::samples(path, cancel) -> Result<Vec<f32>, …>`: `Reader::start(path, 16_000, 1, cancel)` read to the end, polling the cancel flag every block. **No new decode path, no seeking, one pass.** Store the whole half as `f32`: 16 kHz × 1700 s is **27 MB**, which is cheaper than any streaming arrangement and lets core stay a pure function of a slice.
2. **Core's `whistles(samples: &[f32]) -> Vec<Whistle>`**, `Whistle { start: f64, duration: f64, freq: f32, snr_db: f32, tonality_db: f32 }`:
   - a hand-written **Goertzel bank over 2–5 kHz in 75 Hz bins**, on **32 ms windows with a 16 ms hop** (512 and 256 samples at 16 kHz);
   - per window, the strongest bin's magnitude in dB, the band's rolling median (60 s), and the **tonality**: the peak bin over the median of the *other* bins in the same window;
   - a whistle is a run where the peak is ≥ `WHISTLE_SNR_DB` (15 dB, initial) over the band's rolling median **and** ≥ `WHISTLE_TONALITY_DB` (initial 10 dB) over its own window's other bins, holding pitch within ±150 Hz, for ≥ 150 ms;
   - `duration ≥ 0.8 s` is "long", exposed as `Whistle::is_long`.
   - **The tonality term is new, and it is why this detector can work at all:** measured, the loudest 2–4.5 kHz events in a half are the two loudest *cheers*, because a shout is broadband. Without a tonality term a level-and-pitch rule fires on every shout.
3. **Core's `cheers(samples: &[f32]) -> Vec<Cheer>`**, `Cheer { onset: f64, duration: f64, peak_db: f32 }`: broadband level in 0.3–3 kHz, ≥ `CHEER_SNR_DB` (8 dB) over a 60 s rolling median, for ≥ `CHEER_MIN_SECONDS`. **`CHEER_MIN_SECONDS` starts at 1.0 s, not the spec's 1.5 s** — measured, a real goal's burst ran 1.4 s and the 1.5 s floor dropped it. The band filter is a pair of one-pole sections in core on the sample slice; no filter crate.
4. **Every threshold is a named `const` in core with a doc comment saying it is an initial value P3 replaces**, and each is re-exported so the harness can print it on the `RUN` line and sweep it.
5. **Diagnostics into the report**, from this task on: for each truth period event, the **signed offset to the nearest long whistle** and that whistle's SNR and tonality; for each truth goal, the nearest cheer's offset, duration and peak. Printed as `DIAG` lines.

**Test that must fail first** (core, synthetic, on CI):
- a 3.2 kHz tone at −20 dBFS for 300 ms inside pink-ish noise is one whistle, with `freq` within one bin and `duration` within one hop;
- the same tone for **140 ms** is not a whistle (the 150 ms floor);
- a tone that **slides** from 3.0 to 3.6 kHz across 400 ms is not one whistle (the ±150 Hz pitch hold), and is either two or none;
- **a broadband burst at the same total level as the tone is not a whistle** (the tonality term) — this is the test that must fail first and is the reason the term exists;
- a 1.2 s band-limited noise burst 10 dB over its background is one cheer, and a 0.6 s one is not;
- a burst that rides a slow 20 dB level rise across the file is still one cheer (the rolling median tracks it), which is what makes match C's loud venue and match B's quiet one comparable.

**Verify:** the gate, then the ignored ground-truth run. Record in the task report:
- **the signed offset from every one of the twelve period tags to its nearest long whistle** — this is the number that says whether G4's ±10 s period bar is measuring the detector or the coach's reaction, and the probe that motivated this plan saw offsets of 10–30 s;
- cheer recall at the sixteen truth goals and cheer count per half, at the initial constants and at ±1 step either side.

Commit: `feat: whistle and cheer signals, and the analysis audio pass`

---

### Task 3.4: The motion pass, still intervals, and `Analyzer`

**Files:**
- `crates/video-coach-media/src/analyze/{motion.rs,mod.rs}`
- `crates/video-coach-core/src/signals.rs`, `crates/video-coach-core/tests/signals.rs`
- `crates/video-coach-harness/tests/ground_truth.rs`

**What to build:**

1. **`analyze::motion::series(path, cancel) -> Result<Vec<f32>, …>`:** `decodebin3` with the video stream selected by caps → **`videorate` to 5 fps before `glupload`**, so a dropped frame is never uploaded → GL scale to **160×90** → `gldownload` → appsink, on **`Gl::shared()`**. One pass, **no seeking**. Media computes the **mean absolute luma difference** between consecutive thumbnails on the 14,400-pixel buffer and hands core only the number series; that is analysis of a thumbnail GL already made, not full-frame work, so the pixel rule holds.
   - The cancel flag is polled **per frame**, so a cancel lands within ~100 ms (B1) rather than whisper's ~12 s.
   - The series is 5 values a second: **8,500 f32 for a 28-minute file, 34 KB.** Nothing is stored beyond the job.
2. **`Analyzer`**, shaped like `Transcriber` and built on Task 3.2's helper: a worker thread that runs the audio pass and then the motion pass for one source, reports `Progress`, and sends exactly one terminal message. Two passes, not one pipeline with two sinks — a pipeline whose second sink isn't drained stalls, and the audio pass costs seconds anyway.
3. **Core's `still_intervals(motion: &[f32], hz: f64) -> Vec<Range<f64>>`:** motion below `STILL_THETA` for at least `STILL_MIN_SECONDS` (initial 3.0 and 10.0). Pure, on the number series.
4. **`Analyzer` returns a `Signals { whistles, cheers, motion, still }` for the harness to score.** In P3 there is no bus and no command: the ground-truth test calls `Analyzer` directly and waits on its channel.

**Test that must fail first:**
- **Media:** on a generated fixture, the motion series has the expected length for the file's duration at 5 fps (±1), a still fixture yields values near zero and a fixture that changes every frame yields large ones, and **`Analyzer` cancelled mid-file stops within 0.5 s** and sends exactly one terminal message.
- **Core:** a synthetic series with two low stretches of 12 s and 6 s yields one interval; a single sample spike inside a low stretch does not split it if the rule is meant to tolerate it, or does if it isn't — pin whichever the implementation chooses, and say which in the doc comment.

**Verify:** the gate, then the ignored run. Record:
- **V-6: the motion pass's wall time per file**, and the whole `Analyzer`'s, on AC, over the six halves — against G4's ≤ 5 min bar, stated per file length, not per "half";
- **the still-interval count per half at θ ∈ {2, 3, 4, 5, 6}** — the probe behind this plan saw **19–21 per half at every one of them**, so if that holds across six halves, the motion cue's precision ceiling is ~20–25% and the plan says so out loud;
- **V-8:** whether the raw thumbnail difference separates walk-backs from play at all, or needs global motion removed first. Removing global motion is **not** built in this task; it is built only if this measurement demands it, and then as a shift-and-zoom estimate in core on the number series.

Commit: `feat(media): the analysis motion pass and Analyzer`

---

### Task 3.5: The kick-off pattern, the confirmation rule, and the verdict

The task the phase exists for. Everything before it produced inputs; this one produces the numbers that decide P4 and P5.

**Files:**
- `crates/video-coach-core/src/{kickoff.rs,lib.rs}`, `crates/video-coach-core/tests/kickoff.rs` (new)
- `crates/video-coach-media/src/analyze/mod.rs`
- `crates/video-coach-harness/tests/ground_truth.rs`
- `docs/superpowers/spikes/2026-09-24-match-vision-measurements.md` (new)

**What to build:**

1. **`kickoffs(signals) -> Vec<KickOff>`** (core, D3): a still interval, then motion above `STILL_THETA` for ≥ 3 s. `K` is the whistle in `[end − 5 s, end + 2 s]` when there is one, otherwise the end of the still interval. `KickOff` carries which of the two it was and the evidence found, because the report has to trace a miss to a stage.
2. **`suggest(kickoffs, cheers, previous) -> Vec<Detection>`** (core, D4): the first kick-off in a source is a `PeriodStart`; the last long whistle is a `PeriodEnd`; every other kick-off is a `Goal`, **high** tier when a cheer onset lies in `[window start, K − 15 s]` and **quiet** otherwise; the window is `[max(K − W, K_prev), K]`; `at` is the last qualifying cheer's onset minus 1 s, high tier only. Plus the **10 s same-kind de-duplication** D7 needs on a re-run, tested here even though nothing re-runs yet.
3. **`CHEER_GATES_CANDIDATES`, a core `const bool` that D3 leaves to P3.** When true, a kick-off with no cheer in its window is dropped rather than suggested as a quiet-tier goal. Build the flag, then **decide it from the numbers in this task's sweep** — not by argument. The probe behind this plan predicts the quiet tier at roughly 17 false candidates per half against 0–1 true ones, which is far under its 40% bar; if the six halves confirm that, the flag is `true` and the quiet tier never reaches P4.
4. **The sweep.** The ground-truth test runs `Analyzer` once per source and scores the same signals repeatedly over a small grid of constants — the analysis is the expensive part, and the rules are pure, so a sweep costs nothing extra. Sweep `STILL_THETA`, `STILL_MIN_SECONDS`, `CHEER_SNR_DB`, `CHEER_MIN_SECONDS` and `W`, choosing on **match B alone** and reporting every chosen value on the `RUN` line. Then score the held-out matches **once**, with those values, and never touch them again. A tool that lets you pick after seeing the held-out numbers is a tool that passes anything (G2).
5. **The spike doc**, `docs/superpowers/spikes/2026-09-24-match-vision-measurements.md`: V-1, V-3 (if `kickoffs.txt` exists), V-4, V-5, V-6 and V-8 answered with aggregate numbers; every G4 bar with its measured value and verdict; the chosen constants; and one paragraph per bar that failed, saying what the failure implies. **Aggregate numbers only** — no match is described in a way that identifies it beyond "A/B/C", and no file name, team name or shirt number appears.

**Test that must fail first** (core, synthetic, on CI — all three of the spec's confirmation cases):
- **cheer then kick-off** → a high-tier goal whose `at` is the cheer onset − 1 s and whose window holds the cheer;
- **cheer, no kick-off within `W`** → nothing suggested, and the cheer is reported as a near miss;
- **kick-off, no cheer** → a quiet-tier goal with no `at` when `CHEER_GATES_CANDIDATES` is false, and **nothing** when it is true;
- a cheer **inside** `[K − 15 s, K]` does not make the tier high (the restart's own cheer);
- the first kick-off in a source is a period start and not a goal;
- the window clamps to `K_prev`, with a 65 s start-to-goal case taken straight from the measured data;
- two goal suggestions from one run have disjoint windows;
- the 10 s de-duplication drops a new suggestion near a resolved or dismissed one of the same kind.

**Verify:** the gate, then the full ignored run over all three matches, with the report pasted into the task's report and the aggregates into the spike doc.

Commit: `feat(core): the kick-off pattern and the goal confirmation rule`

---

### Task 3.6: The runtime and detector spike (L3, V-2, V-7) — conditional

**Entry condition, decided by 3.5's numbers.** Run this task when **either**:
- the held-out **goal precision** bars fail on sound and motion alone (so P5's formation check is the candidate fix, and V-2 decides whether it can work at all); **or**
- P6 (click-to-track) is the next phase the user wants, since it needs the same runtime and the same V-7 latency figure.

If neither holds — the bars pass and the user is not asking for tracking — **skip this task**, record in the spike doc that P5 is skipped and the runtime is unchosen, and close P3. A detector built for a check nothing needs has not earned its place.

**Files:**
- `tools/export-onnx.md` or `tools/export_onnx.py` (documented, outside the Rust build)
- a scratch spike crate or an `#[ignore]`d test — **not** a dependency added to `video-coach-media` until the runtime is chosen
- `docs/superpowers/spikes/2026-09-24-match-vision-measurements.md`

**What to measure:**
- **L3's rule:** `rten` against `ort` (built `load-dynamic`, never `download-binaries`) on **D-FINE-N** and **RTMDet-tiny**, at **640 and 960** input, on **4 threads**, **on AC**, **over minutes rather than seconds** — a 15 W chip throttles, and a 10-second number is a lie (Phase 10 S0's discipline). Pick `rten` if it is within 1.5× of `ort` and meets the throughput bars; otherwise `ort`, and then its 20 MB bundled `.so`, its pin, its sha256, its `packaging/copyright` stanza and its smoke-test check are all part of its cost.
- **V-2, the spike's biggest open question:** at each truth kick-off, the detector's person boxes on the frame 2 s before `K` — how many fall each side of the frame's vertical centre, and whether the halfway line is in shot. **This needs no kit clustering.** It answers whether the virtual camera frames kick-offs at all, which is the precondition for P5 existing, and it sets `MIN_PER_KIT`.
- **V-7's remainder:** far-side person recall on 20 hand-checked kick-off frames, how often the stitch-seam ghost appears, and a cold snap end to end against G4's 1 s bar.
- **Licences re-verified at pin** (L2, Risk 7): each checkpoint still Apache-2.0, and its backbone descended only from COCO and ImageNet. Anything with Objects365 or other unreviewed ancestry goes back to the user.

**Verify:** the numbers land in the spike doc; no dependency is added to any shipped crate by this task.

Commit: `docs: the detector runtime and formation-feasibility spike`

---

## What would make P3 stop early

P3's job is to produce a verdict, including a negative one. Each of these is a number, measured by the task named, and each one ends or re-plans the phase rather than pressing on.

| Signal | Measured in | What it means |
|---|---|---|
| **Cheer recall at truth goals < 80%** on the held-out matches | 3.3 | The one strong cue does not cover the goals. Sound and motion cannot do this, and P4 ships **periods only**. The next idea is a learned audio tagger (YAMNet is deferred in L2), which is a different spec. |
| **Still intervals ≥ 15 per half** at every workable θ, **and** cheer gating does not pull goal precision to ≥ 70% | 3.4, 3.5 | The kick-off pattern is a ~20%-precision cue, exactly as this plan's probe suggests. P5's formation check becomes the only route to the precision bar, so Task 3.6 runs and V-2 decides whether P5 is even possible. |
| **The still-then-motion pattern misses more than one truth kick-off per half** | 3.5 | Recall fails at the first stage and no downstream rule recovers it. The pattern itself needs replacing — most likely by anchoring on the cheer and searching forward for the restart, which inverts the spec's "kick-off is the primary detector" and goes back to the user as a scope question, not a plan change. |
| **Period tags sit > 10 s from their nearest long whistle, consistently** | 3.3 | G4's ±10 s period bar is measuring the coach's reaction, not the detector. Propose a wider tolerance **to the user** with the measured histogram; do not widen it unilaterally, because the tolerance is also what makes a suggestion resolvable (D6). |
| **`Analyzer` takes > 5 min on the longest file** | 3.4 | Fails G4's throughput bar. The motion pass's 5 fps and 160×90 are the knobs; below those the signal gets worse, so a failure here is a real one. |
| **V-2 shows the camera rarely frames both halves at a kick-off** | 3.6 | P5 cannot work. If the goal precision bars also failed, sound and motion alone are the whole answer and P4 ships what clears its bar — the high tier and periods — with the quiet tier hidden. |

**The likeliest outcome, stated in advance so the numbers are read honestly:** the high tier clears its precision bar, the quiet tier fails its 40% bar and is hidden, periods clear on recall but need their tolerance re-examined, and P5 is built only if the *high tier's* precision turns out to need it. Writing that down before the run is what stops the report being read as a confirmation.

---

## The user's own steps

This is a measurement phase, so there are few, and none of them block a task.

1. **Write `kickoffs.txt` beside each of the three `project.json` files** — one line per post-goal restart: the source's 1-based index and the restart time as `mm:ss`, the moment the ball is played from the centre spot. Sixteen lines in total across the three matches; a `#` comment is allowed and a `# missing` line covers a goal whose restart is not in the file. **This is the only step that unlocks anything**: V-3 (which sets `W`) and the per-goal restart diagnostics. Everything else scores without it. The file stays on the user's disk and is never committed.
2. **Leave the fourth, untagged match untagged.** It is the one piece of footage no threshold has ever seen, and it is worth more as a final check than as a fourth tuning set.
3. **Nothing else.** No app build to run, no UI to try. The first thing the user will have to react to is P4's suggestion rows, and they only exist if the numbers say so.

---

## Deferred out of P3

- **Every UI-facing piece** of D6 and D7: `MatchSuggestion` in the format (P4, and it is **v12**, not the spec's v10), the `Analyze` and `SetSuggestionDismissed` commands, the heavy-job scheduler and its preemption rules (B2), the Match panel's rows, the scrubber's hollow marks and the export sheet's "N suggested goals not confirmed".
- **The formation check itself** (D5, P5) — Task 3.6 measures only whether it *could* work.
- **Global motion removal** from the motion series, built only if 3.4's V-8 demands it.
- **Removing full-frame work from the motion pass** — it is already a thumbnail GL made; nothing here moves resampling into Rust.
- **A learned audio tagger** (YAMNet, L2) — reconsidered only if the cheer detector fails its recall number, and then as a spec change.
- **Which side scored** (Q9), **jersey numbers** (P7), **click-to-track** (P6), and everything in the spec's own Deferred list.
- **Tuning for handheld or broadcast footage.** Nothing here may break on it; nothing here is measured on it.
