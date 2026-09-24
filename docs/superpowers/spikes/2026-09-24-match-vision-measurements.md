# Can sound and motion find the goals? The measurement, and the verdict

**Date:** 2026-09-24 (re-measured the same day, with the restarts)
**Spec:** `docs/superpowers/specs/2026-09-22-match-vision-design.md`, decisions **D** and **G**
**Plan:** `docs/superpowers/plans/2026-09-24-match-detection-measure.md` (P3, the measurement phase)
**How to reproduce:** `cargo test --release -p video-coach-harness --test ground_truth -- --ignored --nocapture --test-threads=1`, with `COACH_GROUND_TRUTH` naming the three tagged folders, tuning match first.

Aggregate numbers only. The three matches are **A**, **B** and **C**; **B** is the tuning match and **A** and **C** are held out. Nothing here names a club, an opponent, a player or a file.

> **What changed in this pass.** The first pass ended by naming one unmeasured number — `W`, the window between a goal and the restart that confirms it — as the only thing that could move the answer. The coach has since timed the restarts for **A** and **C**. `W` is now measured, and every number below is the re-run at the real value. The first pass's numbers are kept where they are the comparison.

---

## In plain words

**Finding the goals automatically still does not work — but it is no longer hopeless, and the reason is now precise.** At the real `W` the rule points at 37 places on the two held-out matches and 7 of them are goals. It still misses 2 of the 9. What changed is the denominator: those 37 suggestions cover **29% of the match** instead of 57%, and a rule highlighting 29% of a game at random would find about **2.7** of the 9 on its own. So the apparatus is now worth about **four extra goals in nine** over chance, against **one** at the guessed `W`. That is a real signal. It is still a list the coach would dismiss four rows out of five, so it is still not something to put in front of you.

**The restarts also said which half of the rule is broken, and it is not the sound.** Given the nine restarts the coach timed — the picture's job done perfectly — the confirmation rule finds **9 of 9 goals with no false ones**. Every goal's cheer stands inside `[K − 60 s, K − 15 s]` of its own real restart, and every one of the nine cheers starts within **2.4 s** of the frame the coach tagged. The rule the spec describes is correct. What fails is the stage before it: the picture offers about **21 candidate restarts a half** where a half holds three or four.

**Finding the periods still does not work.** Unchanged by the restarts, because no restart is a period event: 41–85 whistles in a half, the period ones neither the longest (0.16–0.78 s, the same range as the rest) nor the loudest (the loudest whistle in a file's first or last five minutes is the tagged one **1 time in 12**). The best rule tried finds **half** the period tags and is wrong half the time it fires.

**One bar moved from fail to pass.** The Seek bar — does the coach land within 20 s before the goal — is **7 of 7** on the held-out matches, because a narrower window puts `at` on the cheer rather than three minutes upstream of it. And "no held-out match missing more than one goal" now passes too: A misses one and C misses one, where C used to miss two.

**What that means for the feature.** Showing you suggested goals or periods is **still not justified**, because precision is 0.19 against a 70% bar. But the next step has changed. It is no longer "write down the restarts" — that is done, and it moved the answer as far as it could. It is now one specific thing: **something that can tell a real restart from a camera holding still**, which is exactly what the spec's P5 formation check is. The sound half of this design is measured and works.

---

## What was measured

| | |
|---|---|
| Matches | 3, hand-tagged in the app before any detector ran |
| Halves | 6 (four ~27-minute, two ~33-minute files) |
| Goals | 16 (7 tuning, 9 held out) |
| Period tags | 12 (4 tuning, 8 held out) |
| Restarts after goals | **9, timed by hand** — every goal on both held-out matches; the tuning match has none |
| Analysis | one `Analyzer` run per source: the same job the app would queue |

Every rule below is a pure function of what that one analysis produced, so a whole grid costs one decode (spec G2). Thresholds are chosen on **B** and read off **A** and **C** exactly once.

---

## V-3: how long a walk-back takes — the measurement everything rests on

This is the number the whole goal rule is built from, and until this pass nobody had taken it. The coach timed the moment the ball is played from the centre spot after each goal. The two matches are **one 7-a-side pitch and one 9-a-side**, so the spread below is how long children take to walk back, not a quirk of one ground.

| set | timed | min | max | mean | median |
|---|---|---|---|---|---|
| A | 6 of 6 goals | 20.4 | 29.3 | 24.5 | 25.3 |
| C | 3 of 3 goals | 34.2 | 46.1 | 39.1 | 37.0 |
| **pooled** | **9** | **20.4** | **46.1** | **29.4** | **25.7** |

Three things fall straight out of it:

1. **`W` is 60 s, not 150.** The rule is the longest walk-back ever timed, rounded up to the next quarter-minute: every observed restart plus up to 15 s of margin. It covers **9 of 9**, with 13.9 s to spare. The spec guessed 30–90 s and the code shipped 150 — three times the longest walk-back that has ever happened on this footage, and that alone was most of why a suggestion used to claim over half the game.
2. **The two matches do not overlap.** A's walk-backs are 20–29 s and C's are 34–46 s; there is no gap either band shares with the other. A single `W` has to cover both, which is why the headline is the pooled maximum and not a mean. With only two matches it is also why the margin is not trimmed finer.
3. **The 15 s cheer clamp has less room than it looks.** `CHEER_CLAMP_SECONDS` throws away cheers within 15 s of `K` so the restart's own applause is not read as evidence. The shortest walk-back measured is 20.4 s, so on that goal the cheer cleared the clamp by **5.4 s**. A clamp of 20 s — which would have looked harmless — would have thrown two of the nine goals away.

**How `W` was chosen, and the leak it would have been.** `W` is taken from the physical spread and **not** from any score. It is no longer in the tuning grid at all: a grid free to widen the window always contains a point that claims half the match and reads the coverage back as recall, which is what happened on the first pass. The restarts exist **only for the two held-out matches**, so choosing `W` by what it scores on them would be reading the answer off the paper it is meant to be marked against — and the curve below shows exactly what that would have bought: the best held-out lift is at `W = 45 s`, and 45 is not what was picked.

---

## The `W` curve

Every other constant is the one the tuning match chose; only the window moves. Goals, all tiers.

| `W` | | tp | fp | p | r | share of match | chance r | **lift** |
|---|---|---|---|---|---|---|---|---|
| 30 s | tuning | 1 | 10 | 0.09 | 0.14 | 0.10 | 0.10 | +0.04 |
| | held out | 6 | 19 | 0.24 | 0.67 | 0.10 | 0.12 | **+0.55** |
| 45 s | tuning | 2 | 14 | 0.12 | 0.29 | 0.21 | 0.21 | +0.07 |
| | held out | 7 | 27 | 0.21 | 0.78 | 0.21 | 0.22 | **+0.56** |
| **60 s** | tuning | 4 | 14 | 0.22 | 0.57 | 0.31 | 0.31 | +0.26 |
| **(headline)** | **held out** | **7** | **30** | **0.19** | **0.78** | **0.29** | **0.30** | **+0.48** |
| 90 s | tuning | 4 | 14 | 0.22 | 0.57 | 0.43 | 0.43 | +0.14 |
| | held out | 8 | 38 | 0.17 | 0.89 | 0.49 | 0.51 | +0.38 |
| 150 s | tuning | 7 | 12 | 0.37 | 1.00 | 0.60 | 0.60 | +0.40 |
| | held out | 8 | 38 | 0.17 | 0.89 | 0.64 | 0.68 | +0.21 |

Read it this way:

- **Recall and share climb together, and that is the whole trap.** Recall goes 0.67 → 0.78 → 0.89 as `W` triples; share goes 0.10 → 0.29 → 0.64. Chance recall tracks share almost exactly at every point — 0.12 against 0.10, 0.30 against 0.29, 0.68 against 0.64 — which is what it means to say the window is doing the work.
- **Lift is flat from 30 to 60 s and falls after it.** +0.55, +0.56, +0.48, then +0.38 and +0.21. The physically-chosen 60 s sits on the shoulder rather than the peak, and costs about 0.08 of lift against the held-out optimum. Paying that is the price of not tuning on held-out data, and it is cheap.
- **The tuning match reads the opposite way** — it *prefers* 150 s, at a share of 0.60 — which is precisely the over-fit the first pass fell into and the reason `W` is no longer a tunable at all.

---

## The verdict against the spec's bars (G4)

| Detector | Bar | First pass (`W` = 150) | **This pass (`W` = 60)** | |
|---|---|---|---|---|
| Goals, all tiers | recall ≥ 90%, no match missing > 1, precision ≥ 70% | r = 0.78, C missed 2, p = 0.19 | **r = 0.78** (7/9), **no match missing > 1**, **p = 0.19** | **fail** (precision, recall) |
| Goals, high tier | precision ≥ 90% | 0.19 | **0.19** | **fail** |
| Goals, quiet tier | precision ≥ 40%, or hidden | 0.11, gated off | **0.08**, gated off | **fail → hidden** |
| Seek | 90% of matched high-tier goals within 20 s after the seek point | 6/7 (0.86) | **7/7 (1.00)** held out | **pass** |
| Periods | recall ≥ 90%, precision ≥ 80% | start 0.50/0.50, end 0.00 | **unchanged** | **fail** |
| Throughput | ≤ 5 min per 27-minute half | 67–84 s | **71–88 s per file, ~23× realtime** | **pass** |

**And the bar that is not in the table.** On the held-out matches the chosen rule claims **29%** of the match and so would be expected to "find" **2.7 of 9 goals knowing nothing**. It found 7. **Lift: +0.48**, against +0.15 at the guessed `W`. Every number in the first row still has to be read against that one — but it is now a number that says the rule knows something.

Per match, held out: **A 5 of 6** with 10 false, **C 2 of 3** with 20 false. C is the loud venue and produces 82 of the 97 discarded cheers.

---

## The confirmation rule, scored for real

This is the measurement that was impossible before the restarts existed. Feed `core::kickoff::suggest` the nine restarts the coach timed instead of the picture's guesses — the picture's half of the job done perfectly — and the only thing left that can be wrong is D4's own question: **does a cheer stand in `[K − W, K − 15 s]` of a real restart?**

| | |
|---|---|
| Goals found | **9 of 9** |
| False suggestions | **0** |
| Tier | **high on all nine** |
| Gate on vs gate off | **identical** — every real restart had a cheer behind it |
| Cheer onset vs the coach's tag | **−2.4 s to +1.7 s**, seven of the nine within 1 s |
| Share of the match claimed | 0.07 |

Two honest caveats. The precision of 1.00 is partly constructed: the candidate list *is* the restart list, so there are exactly as many rows as goals. And every window contains its goal by arithmetic, since `W` was set to cover the longest gap. What is **not** constructed is the cheer test, and that is the part that was in doubt: the gate kept all nine.

**And this is where the physical `W` pays for itself.** At `W = 45 s` — the value the held-out curve would have chosen — the goal with the 46.1 s walk-back falls outside its own window and the oracle drops to 8 of 9. The window has to cover the longest walk-back that has actually happened, which is the rule `W` was set by and not a score.

**`near_misses` now means something.** A near miss is a cheer with no restart behind it, and judged against restarts that really happened rather than a stillness rule's guesses, the held-out matches give:

| match | cheers detected | explained by a real restart | **near misses** |
|---|---|---|---|
| A | 42 | 14 | **28** |
| C | 138 | 12 | **126** |

So the crowd makes a noise 180 times across two matches and 9 of those are goals. That is the shape of the problem in one line: **the sound knows where the goals are and cannot possibly know which noise is one** — the restart is the only thing that can say so, which is why the restart detector is now the whole question.

**And the goals it does find, it times almost exactly.** For the seven held-out goals the rule found, the detected `K` sits 20.3–35.9 s after the goal — inside the measured walk-back band of 20.4–46.1 s. The picture's `K` really is landing on the restart when it lands at all.

**Both missed goals are the picture's fault, not the sound's.** Each of the two — one on A, one on C — has a cheer within two seconds of the tag and a restart written down in `kickoffs.txt`, and the picture produced no hold that ended near it. On the first pass those misses were reported as "no cheer and no hold"; with the restarts in hand the diagnosis is sharper and different.

---

## Cue by cue

### 1. The cheer — the only cue with real signal

Held out, 9 goals, a cheer onset within ±5 s of the tag:

| threshold | firings a half | goals found | by chance | lift |
|---|---|---|---|---|
| ≥ 6 dB for ≥ 1.0 s | 24.0 | 8/9 (0.89) | 0.11 | **+0.78** |
| ≥ 8 dB for ≥ 1.0 s | 16.0 | 6/9 (0.67) | 0.07 | +0.60 |
| ≥ 10 dB for ≥ 0.5 s | 45.0 | 9/9 (1.00) | 0.18 | +0.82 |
| ≥ 8 dB for ≥ 0.5 s | 67.2 | 9/9 (1.00) | 0.25 | +0.75 |

The cue is real and venue-dependent: on the tuning match, whose crowd is nearly inaudible, the same thresholds find 2 or 3 of 7. **The short floor is what matters, not the level:** three held-out goals have no cheer within 100 s at ≥ 8 dB for ≥ 1.0 s, and all three have one within 2.4 s of the tag at ≥ 10 dB for ≥ 0.5 s.

**Applause texture — the coach's own idea, that clapping is quiet but textured — was built and measured separately** (`core::signals::clap_texture`, commit `053be67`). At matched firings a half on the held-out matches it loses to the level cue everywhere, and the union of the two is worse than the level cue alone at the same total rate. It wins only on the tuning match, whose crowd is inaudible — which is exactly the win a held-out split exists to distrust.

### 2. The whistle, and why periods failed

Every period tag's distance to its **nearest detected whistle**, over the twelve tags: −0.5, −0.6, −1.2, +2.4, −4.5, −5.3, +5.0, −11.8, +12.7, −31.0, −55.1, +190.2 s. So the whistles are there and seven of the twelve tags sit within about five seconds of one — the coach's reaction time, not a detector error.

**Selecting the right whistle is what has no signal.** A half holds 41–85 detected whistles.

- **Duration does not mark them.** At the tags the whistles run 0.16–0.69 s; the longest whistle in any half is 0.78 s, so the spec's "a period ends on the last **long** (≥ 0.8 s) whistle" finds nothing at all. Sweeping the floor down (first long whistle = start, last = end), held out:

  | floor | long whistles a half | period start r / p | period end r / p |
  |---|---|---|---|
  | 0.15 s | 64.5 | 0.50 / 0.50 | 0.25 / 0.25 |
  | 0.25 s | 13.0 | 0.50 / 0.50 | 0.50 / 0.50 |
  | 0.35 s | 5.0 | **0.50 / 0.50** | **0.50 / 0.50** |
  | 0.50 s | 2.2 | 0.25 / 0.25 | 0.50 / 0.50 |
  | 0.80 s | 0.0 | 0.00 / n/a | 0.00 / n/a |

  A shorter floor does help — from nothing to half — and half is a long way under the 90%/80% bar. The ceiling is the same at 0.25 and 0.35 s, which says the floor is not what is limiting it.
- **Loudness does not mark them either.** The loudest whistle in a file's first (or last) five minutes is the tagged one **1 time in 12**.

### 3. Stillness — now scored against restarts, and the picture is worse than it looked

The spec's absolute θ cannot port: the median motion of a half is 16–19 on two matches and 4–8 on the third, and at θ = 3 the cue finds 1 of the 6 tagged period starts. The threshold is a **quantile of each half's own motion distribution** (`core::motion::still_theta`), which ports by construction.

This pass scores it against **15 tagged kick-offs** — six period starts plus the nine timed restarts — instead of six, which is the first time this cue has had a real denominator. Held out (13 tags), holds that end in play:

| quantile | hold ≥ | candidates a half | worst half | kick-offs found | by chance | lift |
|---|---|---|---|---|---|---|
| 0.30 | 10 s | 4.0 | 8 | 4/13 (0.31) | 0.06 | +0.25 |
| 0.40 | 10 s | 11.5 | 19 | 8/13 (0.62) | 0.15 | +0.47 |
| **0.50** | **10 s** | **21.2** | **29** | **9/13 (0.69)** | 0.22 | **+0.47** |
| 0.50 | 15 s | 13.8 | 17 | 7/13 (0.54) | 0.15 | +0.39 |
| 0.50 | 6 s | 38.5 | 50 | 11/13 (0.85) | 0.37 | +0.48 |

**This is the bottleneck, stated as a number.** At the chosen point the picture offers **21 candidate restarts a half** and a half contains three or four. Its precision floor is 0.11. Nothing downstream can recover from that: the cheer gate throws away four fifths of them and the survivors are still 30 false rows against 7 true ones. Lift is flat at about +0.47 from quantile 0.40 to a 6 s floor, across a fivefold change in firing rate — a cue whose lift does not care how often it fires is a cue that is finding the right things by covering the pitch.

Two things worth keeping from the first pass:

- **The threshold has to land near the half's median**, not near its fifth percentile. The virtual camera's motion is bimodal — play in the high teens, everything else near zero — so "the stillest fifth" falls *inside* the still cluster and no run of frames is continuously below it. That is why a fifth fires 0.0–0.3 times a half and a half fires 21.
- **The spec's "then motion above θ for 3 s" cannot be read literally.** Requiring every frame of those 3 s over θ produced **0 candidates in 6 halves** at every threshold. It is now the *median* of the 3 s, which is the same question asked of a noisy picture.

### 4. The kick-off picture — reproducible, not rare, does not port

A 32×18 normalised thumbnail correlated against known kick-off frames (a template never contains the match it scores). Scored against the same 15 tags:

| template | mean best score at the tags | worst |
|---|---|---|
| the same match's other half | 0.75 (held out), 0.88 (tuning) | 0.43 |
| **another match entirely** | **0.35** (held out) | −0.07 |

So a fixed shipped template is out. The same-match template does find 9 of the 13 held-out tags — but at 26–36 firings a half at every threshold that catches them, because the wide halfway framing *is* the camera's resting state. Cross-match, lift is **negative** at every threshold.

### 5. The combination — the kick-off pattern and the confirmation rule

Built as the spec describes (`core::kickoff`): a hold, then play again, `K` from a whistle near the end of the hold when there is one; the first kick-off of a source is a period start, the last long whistle a period end, every other kick-off a goal whose window is `[max(K − W, K_prev), K]`, **high** tier when a cheer stands in `[window start, K − 15 s]`.

Swept over **180 combinations** and chosen on the tuning match by **lift over chance**, not by F1. `W` is not in the grid — it is V-3's measurement. Chosen: quantile 0.50, hold ≥ 10 s, cheer ≥ 10 dB for ≥ 0.5 s, cheer gate **on**.

| | tuning (B) | held out (A + C) | A | C |
|---|---|---|---|---|
| goals found | 4/7 | **7/9** | 5/6 | 2/3 |
| false suggestions | 14 | **30** | 10 | 20 |
| precision | 0.22 | **0.19** | 0.33 | 0.09 |
| share of the match claimed | 0.31 | **0.29** | | |
| expected by chance | 0.31 | **0.30** | | |
| **lift** | **+0.26** | **+0.48** | | |
| Seek within 20 s | 3/4 | **7/7** | 5/5 | 2/2 |

The tuning match scores *worse* than the held-out ones now, which is what an honestly-set constant looks like: B's crowd is nearly inaudible, so its goals lose the cue that carries the other two.

**`CHEER_GATES_CANDIDATES` is `true`, and it is measured, not argued.** Turning the gate off on the tuning match adds 38 quiet-tier rows carrying 3 goals — precision **0.08** against the 40% bar — and takes the share of the match claimed from 31% to **75%**. It buys the last three goals by highlighting three quarters of the football, and lift goes *down*, 0.26 to 0.25. Scored against the real restarts, gating on and off give the identical 9 of 9, so the gate has never thrown a goal away.

### 6. Throughput (V-6) — the one bar that passes comfortably

Whole `Analyzer` (sound then picture, one decode each), release, on AC: **71–88 s per file**, ~23× realtime, against a bar of 5 minutes per 27-minute half. The sound is about a third of it. Cancelling lands within a frame.

### 7. The rest of the questions

- **V-3 (walk-back durations, which set `W`):** **measured**, above. Nine restarts, 20.4–46.1 s, `W = 60 s`.
- **V-4 (do the files hold the kick-off and the final whistle?):** yes, all six. Lead-in 52–117 s, tail 44–101 s. `auto_back_anchor_p1` is not needed on this footage.
- **V-8 (does the raw thumbnail difference separate a walk-back from play?):** yes, within a half — the distribution is cleanly bimodal — and **no** between venues without the relative threshold above. Global motion removal was not needed to get this far and is not what is limiting anything.
- **V-2 and V-7 (the detector runtime and whether the camera frames a kick-off):** not run. Task 3.6's entry condition is met — the precision bars failed — but see below.

---

## What would change the answer

1. **Something that can tell a real restart from a camera holding still.** This is now the only thing standing between the measured numbers and a usable feature, and the size of the gap is exact: the sound confirms 9 of 9 goals when the restarts are known, and the picture offers 21 candidates a half where a half has three. A detector that cut the candidate list to five a half at the same recall would take precision from 0.19 to about 0.5 with nothing else changed. **The spec's P5 formation check is that detector** — two clusters of children either side of a near-vertical line is what a kick-off looks like and nothing else in a match does — and whether it can work at all is still unmeasured (V-2: does the virtual camera frame both halves of the pitch at a kick-off?). **Nothing else here is worth doing first.**
2. **A learned audio tagger** (the spec defers YAMNet) is now clearly *second*. The cheer cue's recall is 9 of 9; its problem is 180 firings, and the restart is what disambiguates them. A better cheer detector helps the tuning match's inaudible crowd and does nothing about the 21-candidates-a-half problem.
3. **More matches.** Three is thin: one held-out match carries 6 goals and the other 3, so a single goal moves recall by 11 points. The fourth, untagged match is worth more as a final check than as a fourth tuning set — it should stay untagged until something passes. **And restarts for the tuning match** would let `W` be re-derived from three venues instead of two, which is the one thing that would make the leak note below unnecessary.

**What would not change it:** another threshold sweep. Lift is flat at about +0.47 across a fivefold change in the stillness cue's firing rate, which is the signature of a cue that is not discriminating.

**The leak the reader has to be told about.** The restarts exist for **A** and **C** only — the two held-out matches. `W` is derived from them, so the split is no longer perfectly clean. It was chosen from the *physical spread* (the longest walk-back, rounded up) and never from a score, and the `W` curve above shows what optimising would have picked instead: 45 s, for +0.08 of lift. That is the whole size of the contamination, and it points away from the value used rather than towards it.

---

## What P3 leaves in the code

- `core::signals` — whistles, cheers, applause texture. Measured, kept, unused by the app.
- `core::motion` — stillness at a **quantile of the half's own motion** (0.50, hold ≥ 10 s), the thumbnail correlation and its template.
- `core::kickoff` — the kick-off pattern, the confirmation rule, the cheer gate (`true`), D7's 10 s de-duplication and `GOAL_WINDOW_SECONDS = 60`, which is V-3's measurement and no longer a guess.
- `media::analyze` — the audio pass, the motion pass and `Analyzer`, one job per source, ~80 s a half.
- `video-coach-harness` — `truth.rs` (including `walk_backs`, V-3's pairing), `score.rs` and the one `#[ignore]`d `ground_truth` run that produced everything above.

**Nothing is wired to the bus, the project format or the UI**, which is what P3 said it would do. P4 (showing suggestions) is **not justified** by these numbers and is not started. Task 3.6 (the detector runtime spike) is now the obvious next measurement rather than a deferred one: the whole remaining gap is a restart detector, and V-2 decides whether P5 can supply it. That decision is the user's.
