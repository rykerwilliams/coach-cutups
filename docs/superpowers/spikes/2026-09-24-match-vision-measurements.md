# Can sound and motion find the goals? The measurement, and the verdict

**Date:** 2026-09-24
**Spec:** `docs/superpowers/specs/2026-09-22-match-vision-design.md`, decisions **D** and **G**
**Plan:** `docs/superpowers/plans/2026-09-24-match-detection-measure.md` (P3, the measurement phase)
**How to reproduce:** `cargo test --release -p video-coach-harness --test ground_truth -- --ignored --nocapture --test-threads=1`, with `COACH_GROUND_TRUTH` naming the three tagged folders, tuning match first.

Aggregate numbers only. The three matches are **A**, **B** and **C**; **B** is the tuning match and **A** and **C** are held out. Nothing here names a club, an opponent, a player or a file.

---

## In plain words

**Finding the goals automatically does not work, and it is not close.** On the two matches the thresholds had never seen, the finished rule points at 36 places and 7 of them are goals. It misses 2 of the 9. Worse: those 36 suggestions cover **57% of the match between them**, and a rule that highlights 57% of a game at random would find about 5.7 goals of the 9 on its own. The whole apparatus — the crowd noise, the walk-back, the restart — is worth about **one extra goal in nine** over highlighting half the match and calling it a day. That is not something to put in front of you.

**Finding the periods does not work either.** The half's opening and closing whistles are audible and the detector hears them — at seven of the twelve tags there is a whistle within about five seconds. But there is no way to tell *which* whistle: 40–85 whistles are detected in a half, the period ones are not the longest (0.16–0.69 s, the same range as every other whistle), and they are not the loudest either. The best rule tried finds **half** the period tags and is wrong half the time it fires.

**One thing genuinely works, and it is the one you suggested first.** The crowd noise is real signal: on the held-out matches, a cheer lands within 5 s of **8 of 9 goals**, against 1 in 9 expected by chance. The trouble is that the same setting fires about 24 times a half. So the sound knows where the goals are; nothing we built knows how to throw away the other 23.

**What that means for the feature.** Showing you suggested goals or periods is **not justified** on this evidence, at any tier. The next step is not another threshold. It is one of two things: (1) **write down the restarts** — the moment the ball is played from the centre spot after each of the sixteen goals — which is the only measurement the whole design rests on that has never been taken; or (2) accept that this needs the **player-detection model** (the spec's P5), which can ask the question none of these cues can: *are twenty children standing in two halves of the pitch right now?* Until one of those happens, the honest answer is that the analysis backend is built, measured and shelved.

---

## What was measured

| | |
|---|---|
| Matches | 3, hand-tagged in the app before any detector ran |
| Halves | 6 (four ~27-minute, two ~33-minute files) |
| Goals | 16 (7 tuning, 9 held out) |
| Period tags | 12 (4 tuning, 8 held out) |
| Restarts after goals | **0 — never written down** (`kickoffs.txt` is the blank template in all three folders) |
| Analysis | one `Analyzer` run per source: the same job the app would queue |

Every rule below is a pure function of what that one analysis produced, so a 540-point sweep costs one decode (spec G2). Thresholds are chosen on **B** and read off **A** and **C** exactly once.

---

## The verdict against the spec's bars (G4)

| Detector | Bar | Measured (held out) | |
|---|---|---|---|
| Goals, all tiers | recall ≥ 90%, no match missing > 1, precision ≥ 70% | **r = 0.78** (7/9), match C missed **2**, **p = 0.19** | **fail** |
| Goals, high tier | precision ≥ 90% | **p = 0.19** | **fail** |
| Goals, quiet tier | precision ≥ 40%, or hidden | **0.11** on the tuning match; gated off | **fail → hidden** |
| Seek | 90% of matched high-tier goals within 20 s after the seek point | **6/7 held out** (0.86); 7/14 over all three | **fail** |
| Periods | recall ≥ 90%, precision ≥ 80% | start **r = 0.50, p = 0.50**; end **r = 0.00** | **fail** |
| Throughput | ≤ 5 min per 27-minute half | **67–84 s per file, ~24× realtime** | **pass** |

**And the bar that is not in the table.** A goal rule's windows claim a share of the match, and a truth goal falls inside them by luck at that rate. On the held-out matches the chosen rule claims **57%** and so would be expected to "find" **5.7 of 9 goals knowing nothing**. It found 7. **Lift: +0.15.** Every number in the first row has to be read against that one.

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

The cue is real and venue-dependent: on the tuning match, whose crowd is nearly inaudible, the same thresholds find 2 or 3 of 7. Cheers per half at the shipped constants: 4–9 in two matches, 17–30 in the third.

**Applause texture — the coach's own idea, that clapping is quiet but textured — was built and measured separately** (`core::signals::clap_texture`, commit `053be67`). At matched firings a half on the held-out matches it loses to the level cue everywhere (3/9 against 4/9 at 6 firings, 4/9 against 8/9 at 19, 6/9 against 9/9 at 67) and the union of the two is worse than the level cue alone at the same total rate. It wins only on the tuning match, whose crowd is inaudible — which is exactly the win a held-out split exists to distrust.

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

### 3. Stillness — the threshold is now relative, and it changed the picture

The spec's absolute θ cannot port: the median motion of a half is 16–19 on two matches and 4–8 on the third, and at θ = 3 the cue finds 1 of 6 tagged kick-offs. The threshold is now a **quantile of each half's own motion distribution** (`core::motion::still_theta`), which ports by construction.

It made the cue work at all, and it did not make it a detector. Over all six halves, holds of ≥ 10 s that end in play, against the 6 tagged kick-offs at ±10 s:

| quantile | firings a half | kick-offs found | by chance | lift |
|---|---|---|---|---|
| 0.05 | 0.0 | 0/6 | — | — |
| 0.20 | 0.3 | 0/6 | 0.00 | 0.00 |
| 0.30 | 3.5 | 1/6 | 0.04 | +0.13 |
| 0.40 | 10.8 | 4/6 | 0.12 | +0.55 |
| **0.50** | 23.7 | 5/6 | 0.24 | **+0.59** |

Two things worth keeping:

- **The threshold has to land near the half's median**, not near its fifth percentile. The virtual camera's motion is bimodal — play in the high teens, everything else near zero — so "the stillest fifth" falls *inside* the still cluster and no run of frames is continuously below it. That is why a fifth fires 0.3 times a half and a half fires 24.
- **The spec's "then motion above θ for 3 s" cannot be read literally.** Requiring every frame of those 3 s over θ produced **0 candidates in 6 halves** at every threshold. It is now the *median* of the 3 s, which is the same question asked of a noisy picture.

### 4. The kick-off picture — reproducible, not rare, does not port

A 32×18 normalised thumbnail correlated against known kick-off frames (a template never contains the match it scores).

| template | mean best score at the tags | worst |
|---|---|---|
| the same match's other half | 0.70 (held out), 0.88 (tuning) | 0.43 |
| **another match entirely** | **0.24** (held out) | −0.07 |

So a fixed shipped template is out, and even the same-match template fires 17–36 times a half at every threshold that catches the kick-off: the wide halfway framing *is* the camera's resting state. Cross-match, the peaks land nowhere near the tags at all (0/4 at every threshold, lift negative).

### 5. The combination — the kick-off pattern and the confirmation rule

Built as the spec describes (`core::kickoff`): a hold, then play again, `K` from a whistle near the end of the hold when there is one; the first kick-off of a source is a period start, the last long whistle a period end, every other kick-off a goal whose window is `[max(K − W, K_prev), K]`, **high** tier when a cheer stands in `[window start, K − 15 s]`.

Swept over **540 combinations** and chosen on the tuning match — by **lift over chance**, not by F1, because F1 picks the point whose windows cover half the match (it picked `W = 240 s` and 47% coverage on the first pass, and that point scored +0.05 over chance held out). Chosen: quantile 0.50, hold ≥ 15 s, cheer ≥ 10 dB for ≥ 0.5 s, `W = 150 s`, cheer gate **on**.

| | tuning (B) | held out (A + C) | A | C |
|---|---|---|---|---|
| goals found | 7/7 | **7/9** | 6/6 | 1/3 |
| false suggestions | 7 | **29** | 10 | 19 |
| precision | 0.50 | **0.19** | 0.38 | 0.05 |
| share of the match claimed | 0.50 | **0.57** | | |
| expected by chance | 0.48 | **0.63** | | |
| **lift** | **+0.52** | **+0.15** | | |

Also measured: 94 cheers were reported as near misses (a cheer with no restart behind it) — 8 on the tuning match, 10 on A, **76 on C**. The two missed goals are both on C, and both have no cheer and no hold anywhere near them.

**`CHEER_GATES_CANDIDATES` is `true`, and it is measured, not argued.** Turning the gate off on the tuning match adds 18 quiet-tier rows carrying 2 goals — precision **0.11** against the 40% bar — and takes the share of the match claimed from 50% to **80%**.

### 6. Throughput (V-6) — the one bar that passes comfortably

Whole `Analyzer` (sound then picture, one decode each), release, on AC: **67–84 s per file**, ~24× realtime, against a bar of 5 minutes per 27-minute half. The sound is about a third of it. Cancelling lands within a frame.

### 7. The rest of the questions

- **V-4 (do the files hold the kick-off and the final whistle?):** yes, all six. Lead-in 52–117 s, tail 44–101 s. `auto_back_anchor_p1` is not needed on this footage.
- **V-8 (does the raw thumbnail difference separate a walk-back from play?):** yes, within a half — the distribution is cleanly bimodal — and **no** between venues without the relative threshold above. Global motion removal was not needed to get this far and is not what is limiting anything.
- **V-3 (walk-back durations, which set `W`):** **unmeasured**. No restart has ever been written down, so `W = 150 s` is still the spec's guess, and it is the single most influential number in the goal rule: it sets how much of the match a suggestion claims.
- **V-2 and V-7 (the detector runtime and whether the camera frames a kick-off):** not run. Task 3.6's entry condition is met by these numbers — the precision bars failed — but see below.

---

## What would change the answer

1. **The restarts.** Sixteen lines in `kickoffs.txt`: the source's index and the `mm:ss` at which the ball is played from the centre spot after each goal. This is the only measurement the design rests on that has never been taken. It sets `W` from data instead of from a guess, and a `W` of 45 s instead of 150 s would cut the share of the match claimed by two thirds — which is most of the gap between the rule and chance. **Nothing else here is worth doing first.**
2. **The player-detection model (spec P5).** Every cue measured here answers "did something happen?" and none of them answers "is this a kick-off?". A person detector can: two clusters of children either side of a near-vertical line is what a kick-off looks like and nothing else in a match does. The honest reading of this measurement is that **the formation check is not an optional precision boost, it is the only cue with a chance of clearing the bars** — and whether it can work at all is still unmeasured (V-2: does the virtual camera frame both halves of the pitch at a kick-off?).
3. **A learned audio tagger** (the spec defers YAMNet). The cheer cue's problem is not recall, it is that 24 firings a half include every shout, whistle and passing car. A model that labels "crowd cheer" rather than "band got louder" could move precision without touching recall. It is a different spec.
4. **More matches.** Three is thin: one held-out match carries 6 goals and the other 3, so a single goal moves recall by 11 points. The fourth, untagged match is worth more as a final check than as a fourth tuning set — it should stay untagged until something passes.

**What would not change it:** another threshold sweep. 540 points were tried, the best of them on data it had seen is worth +0.52 over chance and +0.15 on data it had not, and the shape of that gap is what over-fitting looks like.

---

## What P3 leaves in the code

- `core::signals` — whistles, cheers, applause texture. Measured, kept, unused by the app.
- `core::motion` — stillness at a **quantile of the half's own motion**, the thumbnail correlation and its template.
- `core::kickoff` — the kick-off pattern, the confirmation rule, the cheer gate (`true`) and D7's 10 s de-duplication.
- `media::analyze` — the audio pass, the motion pass and `Analyzer`, one job per source, ~80 s a half.
- `video-coach-harness` — `truth.rs`, `score.rs` and the one `#[ignore]`d `ground_truth` run that produced everything above.

**Nothing is wired to the bus, the project format or the UI**, which is what P3 said it would do. P4 (showing suggestions) is **not justified** by these numbers and is not started. Task 3.6 (the detector runtime spike) is deferred with P5 rather than run on its own: measuring `rten` against `ort` is only worth doing when there is a detector the answer would be used for, and that decision is the user's.
