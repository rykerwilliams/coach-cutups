//! The measurement tool: grade the detector against the coach's own tagged
//! matches (spec G3), sound and picture in **one run**.
//!
//! `#[ignore]`d, because it needs whole matches of real footage that CI has
//! no copy of and never will — the repository is public and the footage shows
//! children. Run it on the reference laptop with
//!
//! ```text
//! COACH_GROUND_TRUTH=B=/local/b:A=/local/a:C=/local/c \
//!   flock /tmp/claude-1000/cargo.lock nice -n 19 \
//!   cargo test --release -p video-coach-harness --test ground_truth \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `:`-separated project folders, **first is the tuning match** (G2) and the
//! rest are held out. An entry may carry a one- or two-character label
//! (`B=/local/b`); without one a match is named by its position, `A`, `B`,
//! `C` … Either way nothing identifying — no folder name, no team name — ever
//! reaches the terminal.
//!
//! **One run, one analysis per source.** Every cue is read off the same
//! [`Analyzer`] output the app would produce, which is what makes the sound's
//! numbers and the picture's comparable at all: the sweeps are pure functions
//! of the series, so a whole grid of constants costs one decode (G2).
//!
//! **`--release`, and it is not optional.** The whistle bank is forty Goertzel
//! evaluations over every 32 ms window of a half, and an unoptimised build
//! spends about forty times as long on it as the one the coach would run.
//!
//! **The folders are read-only.** This test opens each project with
//! `store::read`, reads `kickoffs.txt` and decodes the source videos; it never
//! builds a `Bus`, never calls `store::write`, and writes no file inside a
//! project folder.
//!
//! `--test-threads=1` because the analysis decodes whole halves, and two at
//! once would fight over the decoder and ruin every timing line.
//!
//! # What this run answers
//!
//! | Prefix | The question |
//! |---|---|
//! | `TAGS`, `GAPS` | what the coach's tags alone say, with no detector |
//! | `SIGNAL`, `MDIST` | what one analysis found and what it cost (V-6, V-8) |
//! | `DIAG` | per tag, the nearest signal that should have found it |
//! | `SWEEP` | the cheer cue's coverage against a chance baseline |
//! | `STILL`, `SSWEEP` | stillness, at quantiles of each half's **own** distribution |
//! | `WHIST`, `WSWEEP`, `PSEL` | periods from whistles alone: the long floor swept, and the loudest-whistle rule |
//! | `PICTURE`, `PSWEEP`, `PORT` | the kick-off picture, against templates held out by match |
//! | `RGRID`, `CHOSE` | the whole rule, swept on the tuning match and chosen there |
//! | `SCORE`, `CHANCE`, `SEEK` | the chosen rule on the held-out matches, against chance |
//! | `HIT`, `MISS`, `NEAR` | every goal it found, missed, and every cheer it threw away |
//!
//! The verdict is
//! `docs/superpowers/spikes/2026-09-24-match-vision-measurements.md`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use video_coach_core::kickoff::{
    kickoffs, near_misses, suggest, KickOff, Rule, SuggestionKind, CHEER_CLAMP_SECONDS,
    GOAL_WINDOW_SECONDS, RESUME_SECONDS,
};
use video_coach_core::motion::{
    at_quantile, peaks, still_intervals_at, still_theta, Template, Thumbnail,
    KICKOFF_MIN_GAP_SECONDS, KICKOFF_SIMILARITY, MOTION_HZ, STILL_MIN_SECONDS, STILL_QUANTILE,
    THUMBNAIL_HEIGHT, THUMBNAIL_HZ, THUMBNAIL_WIDTH,
};
use video_coach_core::signals::{
    cheers_from, Cheer, Whistle, CHEER_MIN_SECONDS, CHEER_SNR_DB, MEDIAN_SECONDS,
    WHISTLE_LONG_SECONDS, WHISTLE_MIN_SECONDS, WHISTLE_PITCH_HZ, WHISTLE_SNR_DB,
    WHISTLE_TONALITY_DB,
};
use video_coach_harness::score::{
    onset_coverage, print_audio_diagnostics, score, show_rate, Detection, ScoreReport, Tally,
    CHEER_TOLERANCE, PERIOD_TOLERANCE, SEEK_LEAD, SEEK_WINDOW,
};
use video_coach_harness::truth::{folders, Truth, TruthEvent, TruthKind};
use video_coach_media::analyze::{AnalyzeMessage, Analyzer, Signals};

/// The cheer thresholds the cue is reported at: the initial value and one step
/// either side of it.
const CHEER_SNR_SWEEP: [f32; 3] = [CHEER_SNR_DB - 2.0, CHEER_SNR_DB, CHEER_SNR_DB + 2.0];
const CHEER_MIN_SWEEP: [f64; 2] = [0.5, CHEER_MIN_SECONDS];

/// The quantiles of a half's own motion the stillness cue is read at.
///
/// **A quantile has to sit over the share of the half that is genuinely
/// still** — under it and no run of frames is continuously below the threshold
/// at all — and **under the share that is play**, or every hold runs into the
/// next. Measured, the window between those two is not the same width in every
/// venue, which is the finding this sweep exists to expose.
const QUANTILE_SWEEP: [f64; 5] = [0.05, 0.20, 0.30, 0.40, 0.50];

/// The stillness floors. A shorter floor finds a kick-off whose walk-back was
/// brief and costs candidates everywhere else, which is the whole trade.
const STILL_MIN_SWEEP: [f64; 3] = [6.0, 10.0, STILL_MIN_SECONDS];

/// How long a whistle must hold to end a period. **The one constant the
/// footage flatly refuted**: no half holds one longer than 0.78 s, so the
/// spec's 0.8 s finds none at all. The sweep runs down to a peep.
const LONG_WHISTLE_SWEEP: [f64; 5] = [WHISTLE_MIN_SECONDS, 0.25, 0.35, 0.50, WHISTLE_LONG_SECONDS];

/// D4's `W`, swept. **Unmeasured from the footage**: V-3 sets it from the
/// walk-back durations in `kickoffs.txt`, and no restart has been written down
/// yet.
const WINDOW_SWEEP: [f64; 3] = [90.0, GOAL_WINDOW_SECONDS, 240.0];

/// The similarity thresholds for the picture cue. Nothing external has ever
/// measured it, so the sweep is wide: 0.5 is "vaguely the same scene" and 0.9
/// is "very nearly the same picture".
const SIMILARITY_SWEEP: [f32; 5] = [0.5, 0.6, 0.7, KICKOFF_SIMILARITY, 0.9];

/// How much of a tagged kick-off goes into a picture template: the tag's own
/// second and this many either side, so a tag a second or two early still
/// carries the framing.
const TEMPLATE_RADIUS: f64 = 2.0;

/// How far a picture peak or a still interval's end may sit from a kick-off tag
/// and still be that kick-off. The same tolerance the scorer gives a period
/// event, for the same reason: it is a bar on the coach's tagging as much as on
/// the detector.
const KICKOFF_TOLERANCE: f64 = 10.0;

/// How far either side of a tag the best picture score is looked for, when the
/// question is how distinctive the framing is rather than whether a rule fired.
const TAG_SEARCH: f64 = 5.0;

/// The sets every sweep is totalled over. The split is G2's: constants are
/// chosen on the tuning match and read off the held-out ones, and `all` exists
/// only so a number can be compared with one measured over everything.
const SETS: [&str; 3] = ["tuning", "held_out", "all"];

#[test]
#[ignore = "needs the coach's tagged matches: see this file's module docs"]
fn ground_truth() {
    let var = std::env::var("COACH_GROUND_TRUTH").expect(
        "COACH_GROUND_TRUTH names the tagged project folders, `:`-separated, \
         tuning match first — see this test's module docs",
    );
    gstreamer::init().expect("GStreamer starts");
    let truths: Vec<Truth> = folders(&var)
        .unwrap_or_else(|why| panic!("COACH_GROUND_TRUTH: {why}"))
        .iter()
        .map(|(name, folder)| {
            Truth::read(name.as_str(), folder).unwrap_or_else(|e| panic!("match {name}: {e}"))
        })
        .collect();

    for truth in &truths {
        check(truth);
        truth.print_census();
    }

    let tuning = truths.first().expect("at least one match").name.clone();
    println!(
        "RUN    tuning={tuning} held_out={} period_tolerance={PERIOD_TOLERANCE:.1}s \
         kickoff_tolerance={KICKOFF_TOLERANCE:.1}s seek_lead={SEEK_LEAD:.1}s \
         seek_window={SEEK_WINDOW:.1}s cheer_tolerance={CHEER_TOLERANCE:.1}s \
         median={MEDIAN_SECONDS:.0}s whistle_snr={WHISTLE_SNR_DB:.1}dB \
         whistle_tonality={WHISTLE_TONALITY_DB:.1}dB whistle_pitch={WHISTLE_PITCH_HZ:.0}Hz \
         whistle_min={WHISTLE_MIN_SECONDS:.2}s whistle_long={WHISTLE_LONG_SECONDS:.1}s \
         cheer_snr={CHEER_SNR_DB:.1}dB cheer_min={CHEER_MIN_SECONDS:.1}s \
         motion_hz={MOTION_HZ:.1} thumbnail_hz={THUMBNAIL_HZ:.1} \
         thumbnail={THUMBNAIL_WIDTH}x{THUMBNAIL_HEIGHT} still_quantile={STILL_QUANTILE:.2} \
         still_min={STILL_MIN_SECONDS:.1}s resume={RESUME_SECONDS:.1}s \
         cheer_clamp={CHEER_CLAMP_SECONDS:.1}s W={GOAL_WINDOW_SECONDS:.0}s \
         similarity={KICKOFF_SIMILARITY:.2} peak_gap={KICKOFF_MIN_GAP_SECONDS:.0}s",
        truths
            .iter()
            .skip(1)
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );

    // One analysis per source; every cue below reads what it produced.
    let halves: Vec<Half> = truths
        .iter()
        .flat_map(|truth| {
            let tuning = truth.name == tuning;
            truth
                .sources
                .iter()
                .enumerate()
                .map(move |(src, path)| Half::analyse(truth, src, path, tuning))
        })
        .collect();

    cheer_cue(&halves);
    stillness(&halves);
    periods_from_whistles(&truths, &halves);
    picture(&halves);
    rule(&truths, &halves);
}

// --------------------------------------------------------------- one half

/// One half, its tags and everything one analysis found in it.
struct Half {
    /// `A`, `B`, `C` … — never the folder name.
    match_name: String,
    source_index: usize,
    /// From the tuning match, the first folder in `COACH_GROUND_TRUTH` (G2).
    tuning: bool,
    /// This source's tags, copied so nothing borrows the match.
    events: Vec<TruthEvent>,
    /// Every kick-off tagged on this half: the period start, and the restarts
    /// `kickoffs.txt` names if it has any.
    kickoff_tags: Vec<f64>,
    /// The tagged period start, which is a kick-off frame and the only one a
    /// picture template is ever built from.
    period_start: Option<f64>,
    signals: Signals,
    cost: Duration,
}

impl Half {
    fn analyse(truth: &Truth, source_index: usize, path: &Path, tuning: bool) -> Half {
        let started = Instant::now();
        let signals = analyse(path);
        let half = Half {
            match_name: truth.name.clone(),
            source_index,
            tuning,
            events: truth
                .events
                .iter()
                .filter(|e| e.source_index == source_index)
                .copied()
                .collect(),
            kickoff_tags: truth
                .on(source_index, TruthKind::PeriodStart)
                .chain(truth.on(source_index, TruthKind::Restart))
                .map(|e| e.seconds)
                .collect(),
            period_start: truth
                .on(source_index, TruthKind::PeriodStart)
                .next()
                .map(|e| e.seconds),
            signals,
            cost: started.elapsed(),
        };
        half.print();
        half
    }

    /// What the analysis found and what it cost (V-6), and how the picture's
    /// motion is distributed (V-8).
    fn print(&self) {
        let s = &self.signals;
        println!(
            "SIGNAL match={} src={} audio_s={:.0} video_s={:.0} pass_s={:.1} realtime={:.0}x \
             whistles={} longest={:.2} long={} cheers={} kickoff_tags={}",
            self.match_name,
            self.source_index,
            s.audio_seconds,
            s.video_seconds,
            self.cost.as_secs_f64(),
            s.video_seconds / self.cost.as_secs_f64(),
            s.whistles.len(),
            s.whistles.iter().map(|w| w.duration).fold(0.0, f64::max),
            s.whistles.iter().filter(|w| w.is_long()).count(),
            self.cheers(CHEER_SNR_DB, CHEER_MIN_SECONDS).len(),
            self.kickoff_tags.len(),
        );
        // V-8: whether the raw thumbnail difference separates a walk-back from
        // play at all. Global motion is **not** removed, so the virtual
        // camera's pan is in these numbers on purpose.
        println!(
            "MDIST  match={} src={} p02={:.2} p05={:.2} p10={:.2} p20={:.2} p50={:.2} \
             p95={:.2} max={:.2}",
            self.match_name,
            self.source_index,
            at_quantile(&s.motion, 0.02),
            at_quantile(&s.motion, 0.05),
            at_quantile(&s.motion, 0.10),
            at_quantile(&s.motion, 0.20),
            at_quantile(&s.motion, 0.50),
            at_quantile(&s.motion, 0.95),
            at_quantile(&s.motion, 1.0),
        );
    }

    /// Which totals this half counts towards.
    fn sets(&self) -> Vec<&'static str> {
        set_names(self.tuning)
    }

    fn id(&self) -> String {
        format!("match={} src={}", self.match_name, self.source_index)
    }

    fn cheers(&self, snr_db: f32, min_seconds: f64) -> Vec<Cheer> {
        cheers_from(&self.signals.cheer_excess, snr_db, min_seconds)
    }

    /// The kick-off candidates at one picture threshold, read off this half's
    /// **own** motion distribution.
    fn kickoffs(&self, quantile: f64, min_seconds: f64) -> Vec<KickOff> {
        kickoffs(
            &self.signals.motion,
            MOTION_HZ,
            still_theta(&self.signals.motion, quantile),
            min_seconds,
            &self.signals.whistles,
        )
    }

    fn long_whistles(&self, seconds: f64) -> Vec<&Whistle> {
        self.signals
            .whistles
            .iter()
            .filter(|w| w.is_longer_than(seconds))
            .collect()
    }

    /// How many of this half's kick-off tags have one of `times` within
    /// [`KICKOFF_TOLERANCE`], and how many tags there are.
    fn kickoff_hits(&self, times: &[f64]) -> (usize, usize) {
        let hits = self
            .kickoff_tags
            .iter()
            .filter(|&&tag| nearest(times, tag).is_some_and(|d| d <= KICKOFF_TOLERANCE))
            .count();
        (hits, self.kickoff_tags.len())
    }

    /// The thumbnails within [`TEMPLATE_RADIUS`] of `seconds`.
    fn around(&self, seconds: f64) -> Vec<Thumbnail> {
        let first = ((seconds - TEMPLATE_RADIUS) * THUMBNAIL_HZ)
            .round()
            .max(0.0) as usize;
        let last = (((seconds + TEMPLATE_RADIUS) * THUMBNAIL_HZ).round() as usize)
            .min(self.signals.thumbnails.len().saturating_sub(1));
        self.signals
            .thumbnails
            .get(first..=last)
            .unwrap_or_default()
            .to_vec()
    }
}

/// Run the app's own [`Analyzer`] over one source and wait for it.
///
/// The same job the Match panel will queue in P4, so the numbers below are the
/// ones the coach's machine would produce (G3). Nothing cancels a measurement
/// run, so the only message that matters is the last one.
fn analyse(path: &Path) -> Signals {
    let (send, receive) = mpsc::channel();
    // Held until `Finished` arrives: dropping an `Analyzer` cancels it.
    let _analyzer = Analyzer::start(path.to_path_buf(), move |message| {
        let _ = send.send(message);
    });
    loop {
        match receive
            .recv()
            .expect("the analysis thread sends exactly one Finished")
        {
            AnalyzeMessage::Progress(_) => {}
            AnalyzeMessage::Finished(result) => {
                return result.expect("the source analyses");
            }
        }
    }
}

// ------------------------------------------------------------- the cheer cue

/// The one strong cue in the sound, at a small grid, with a chance baseline.
fn cheer_cue(halves: &[Half]) {
    let mut totals: BTreeMap<(&str, String), Firings> = BTreeMap::new();
    for half in halves {
        print_audio_diagnostics(
            &half.match_name,
            half.source_index,
            &half.events,
            &half.signals.whistles,
            &half.cheers(CHEER_SNR_DB, CHEER_MIN_SECONDS),
        );
        for snr in CHEER_SNR_SWEEP {
            for min in CHEER_MIN_SWEEP {
                let onsets: Vec<f64> = half.cheers(snr, min).iter().map(|c| c.onset).collect();
                let (covered, goals) = onset_coverage(&half.events, half.source_index, &onsets);
                for set in half.sets() {
                    let entry = totals
                        .entry((set, format!("snr={snr:.1} min={min:.1}")))
                        .or_default();
                    entry.add(onsets.len(), covered, goals);
                    // A goal is "covered" when a burst lands within the
                    // tolerance of it, so `n` onsets scattered at random over a
                    // half cover it with probability `1 − (1 − 2·tol/T)^n`.
                    // Without this column a rule that fires every fourteen
                    // seconds reads as a detector.
                    entry.chance_from(onsets.len(), goals, 2.0 * CHEER_TOLERANCE, half);
                }
            }
        }
    }
    for ((set, point), firings) in &totals {
        println!("SWEEP  set={set} feature=cheer {point} {}", firings.show());
    }
}

// -------------------------------------------------------------- the picture

/// The stillness cue, at quantiles of each half's own motion (spec D3).
///
/// A still interval's **end** is what the kick-off pattern reads as the
/// restart, so that is what a tag is matched against.
fn stillness(halves: &[Half]) {
    let mut totals: BTreeMap<(&str, String), Firings> = BTreeMap::new();
    for half in halves {
        for quantile in QUANTILE_SWEEP {
            let theta = still_theta(&half.signals.motion, quantile);
            for min_seconds in STILL_MIN_SWEEP {
                let intervals =
                    still_intervals_at(&half.signals.motion, MOTION_HZ, theta, min_seconds);
                let ends: Vec<f64> = intervals.iter().map(|i| i.end).collect();
                let (hits, tags) = half.kickoff_hits(&ends);
                // The same rule once the picture has to move again, which is
                // the kick-off pattern's own stage and the one the goal rule
                // reads.
                let restarts: Vec<f64> = half
                    .kickoffs(quantile, min_seconds)
                    .iter()
                    .map(|k| k.seconds)
                    .collect();
                let (resumed_hits, _) = half.kickoff_hits(&restarts);
                println!(
                    "STILL  {} quantile={quantile:.2} theta={theta:.2} min={min_seconds:.1} \
                     holds={} resumed={} tags_by_hold={hits}/{tags} \
                     tags_by_kickoff={resumed_hits}/{tags}",
                    half.id(),
                    intervals.len(),
                    restarts.len(),
                );
                for set in half.sets() {
                    let key = format!("quantile={quantile:.2} min={min_seconds:.1}");
                    let entry = totals.entry((set, key)).or_default();
                    entry.add(restarts.len(), resumed_hits, tags);
                    entry.chance_from(restarts.len(), tags, 2.0 * KICKOFF_TOLERANCE, half);
                }
            }
        }
    }
    for ((set, point), firings) in &totals {
        println!("SSWEEP set={set} {point} {}", firings.show());
    }
}

/// The picture cue, against templates that never contain the match they score.
///
/// `cross` is built from the **other matches'** period starts, `other_half`
/// from the same match's other half. The gap between the two is the price of
/// changing grounds, and so the answer to whether a shipped template could
/// exist at all.
fn picture(halves: &[Half]) {
    let mut known: BTreeMap<String, Vec<Thumbnail>> = BTreeMap::new();
    for half in halves {
        if let Some(start) = half.period_start {
            known
                .entry(half.match_name.clone())
                .or_default()
                .extend(half.around(start));
        }
    }

    let mut totals: BTreeMap<(&str, String, String), Firings> = BTreeMap::new();
    let mut distinctive: BTreeMap<(&str, String), Vec<f32>> = BTreeMap::new();
    for half in halves {
        for kind in ["cross", "other_half"] {
            let frames: Vec<Thumbnail> = match kind {
                "cross" => known
                    .iter()
                    .filter(|(name, _)| name.as_str() != half.match_name)
                    .flat_map(|(_, frames)| frames.clone())
                    .collect(),
                _ => halves
                    .iter()
                    .filter(|other| {
                        other.match_name == half.match_name
                            && other.source_index != half.source_index
                    })
                    .filter_map(|other| other.period_start.map(|start| other.around(start)))
                    .flatten()
                    .collect(),
            };
            let template = Template::new(frames);
            if template.is_empty() {
                println!("PICTURE {} template={kind} frames=0 skipped", half.id());
                continue;
            }
            let scores = template.scores(&half.signals.thumbnails);

            // How distinctive a kick-off is, before any threshold: its best
            // score near the tag, and how many seconds of the half score
            // higher. Rank 0 means the kick-off is the single most
            // template-like second of the whole half.
            for &tag in &half.kickoff_tags {
                let (best, rank) = tag_score(&scores, tag);
                println!(
                    "PICTURE {} template={kind} frames={} tag={tag:.0} best={best:.3} \
                     rank={rank} of={}",
                    half.id(),
                    template.len(),
                    scores.len(),
                );
                for set in half.sets() {
                    distinctive
                        .entry((set, kind.to_owned()))
                        .or_default()
                        .push(best);
                }
            }

            for threshold in SIMILARITY_SWEEP {
                let found = peaks(&scores, THUMBNAIL_HZ, threshold, KICKOFF_MIN_GAP_SECONDS);
                let times: Vec<f64> = found.iter().map(|p| p.seconds).collect();
                let (hits, tags) = half.kickoff_hits(&times);
                for set in half.sets() {
                    let entry = totals
                        .entry((set, kind.to_owned(), format!("{threshold:.2}")))
                        .or_default();
                    entry.add(found.len(), hits, tags);
                    entry.chance_from(found.len(), tags, 2.0 * KICKOFF_TOLERANCE, half);
                }
            }
        }
    }

    for ((set, kind, threshold), firings) in &totals {
        println!(
            "PSWEEP set={set} template={kind} threshold={threshold} {}",
            firings.show()
        );
    }
    // The one line that answers "does a template port across matches".
    for ((set, kind), best) in &distinctive {
        let mean = best.iter().map(|&b| f64::from(b)).sum::<f64>() / best.len().max(1) as f64;
        let worst = best.iter().copied().fold(f32::MAX, f32::min);
        println!(
            "PORT   set={set} template={kind} tags={} mean_best={mean:.3} worst_best={worst:.3}",
            best.len(),
        );
    }
}

// --------------------------------------------------------------- the periods

/// Periods from the whistles alone, as the long floor comes down (D4).
///
/// The rule is the spec's: the **first** long whistle of a source starts its
/// period and the **last** ends it. At the spec's own 0.8 s floor no half has
/// one, so this is the sweep that says whether a shorter floor is a period
/// detector or just more whistles.
fn periods_from_whistles(truths: &[Truth], halves: &[Half]) {
    let mut totals: BTreeMap<(&str, String), (Tally, usize, usize)> = BTreeMap::new();
    for truth in truths {
        let mine: Vec<&Half> = halves
            .iter()
            .filter(|h| h.match_name == truth.name)
            .collect();
        for seconds in LONG_WHISTLE_SWEEP {
            let mut found = Vec::new();
            let mut long = 0;
            for half in &mine {
                let whistles = half.long_whistles(seconds);
                long += whistles.len();
                if let Some(first) = whistles.first() {
                    found.push(Detection {
                        source_index: half.source_index,
                        seconds: first.start,
                        kind: SuggestionKind::PeriodStart,
                    });
                }
                if let Some(last) = whistles.last() {
                    found.push(Detection {
                        source_index: half.source_index,
                        seconds: last.start,
                        kind: SuggestionKind::PeriodEnd,
                    });
                }
            }
            let report = score(&truth.events, &found);
            println!(
                "WHIST  match={} long={seconds:.2} long_whistles={long} \
                 start_tp={} start_fn={} end_tp={} end_fn={}",
                truth.name,
                report.tally.period_start.tp,
                report.tally.period_start.misses,
                report.tally.period_end.tp,
                report.tally.period_end.misses,
            );
            for set in set_names(mine[0].tuning) {
                let entry = totals.entry((set, format!("{seconds:.2}"))).or_insert((
                    Tally::default(),
                    0,
                    0,
                ));
                entry.0.add(&report.tally);
                entry.1 += long;
                entry.2 += mine.len();
            }
        }
    }
    strongest_whistle(truths, halves);
    for ((set, seconds), (tally, long, halves)) in &totals {
        println!(
            "WSWEEP set={set} long={seconds} per_half={:.1} start_tp={} start_fp={} \
             start_fn={} start_r={} start_p={} end_tp={} end_fp={} end_fn={} end_r={} end_p={}",
            *long as f64 / (*halves).max(1) as f64,
            tally.period_start.tp,
            tally.period_start.fp,
            tally.period_start.misses,
            show_rate(tally.period_start.recall()),
            show_rate(tally.period_start.precision()),
            tally.period_end.tp,
            tally.period_end.fp,
            tally.period_end.misses,
            show_rate(tally.period_end.recall()),
            show_rate(tally.period_end.precision()),
        );
    }
}

/// How far into a file a period's opening whistle can be, and how far from its
/// end the closing one: measured, every file leads in 52–117 s and trails off
/// 44–101 s (V-4), so five minutes is generous at both ends.
const PERIOD_EDGE_SECONDS: f64 = 300.0;

/// The other selector worth measuring, because the spec's one has no signal:
/// the **loudest** whistle in the first and last [`PERIOD_EDGE_SECONDS`] of a
/// file, whatever its length.
///
/// Duration turned out not to mark the kick-off and final whistles at all — at
/// the tags they run 0.16–0.69 s, which is every other whistle's range too. The
/// level over the band's own median is the other thing a whistle has, and this
/// is the line that says whether it marks them instead.
fn strongest_whistle(truths: &[Truth], halves: &[Half]) {
    let mut totals: BTreeMap<&str, Tally> = BTreeMap::new();
    for truth in truths {
        let mine: Vec<&Half> = halves
            .iter()
            .filter(|h| h.match_name == truth.name)
            .collect();
        let mut found = Vec::new();
        for half in &mine {
            let loudest = |from: f64, to: f64| {
                half.signals
                    .whistles
                    .iter()
                    .filter(|w| w.start >= from && w.start <= to)
                    .max_by(|a, b| a.snr_db.total_cmp(&b.snr_db))
                    .map(|w| w.start)
            };
            let end = half.signals.video_seconds;
            for (at, kind) in [
                (
                    loudest(0.0, PERIOD_EDGE_SECONDS),
                    SuggestionKind::PeriodStart,
                ),
                (
                    loudest(end - PERIOD_EDGE_SECONDS, end),
                    SuggestionKind::PeriodEnd,
                ),
            ] {
                if let Some(seconds) = at {
                    found.push(Detection {
                        source_index: half.source_index,
                        seconds,
                        kind,
                    });
                }
            }
        }
        let report = score(&truth.events, &found);
        for hit in &report.period_hits {
            println!(
                "PSEL   match={} src={} kind={:?} tag={:.1} error={:+.1}",
                truth.name, hit.source_index, hit.kind, hit.truth_seconds, hit.error,
            );
        }
        for set in set_names(mine[0].tuning) {
            totals.entry(set).or_default().add(&report.tally);
        }
    }
    for (set, tally) in &totals {
        println!(
            "PSEL   set={set} rule=loudest_in_{PERIOD_EDGE_SECONDS:.0}s \
             start_tp={} start_fn={} start_r={} end_tp={} end_fn={} end_r={}",
            tally.period_start.tp,
            tally.period_start.misses,
            show_rate(tally.period_start.recall()),
            tally.period_end.tp,
            tally.period_end.misses,
            show_rate(tally.period_end.recall()),
        );
    }
}

// ------------------------------------------------------------ the whole rule

/// One point of the rule's grid.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    quantile: f64,
    still_min: f64,
    cheer_snr: f32,
    cheer_min: f64,
    window: f64,
    gate: bool,
    long_whistle: f64,
}

impl std::fmt::Display for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "quantile={:.2} still_min={:.1} cheer_snr={:.1} cheer_min={:.1} W={:.0} \
             gate={} long={:.2}",
            self.quantile,
            self.still_min,
            self.cheer_snr,
            self.cheer_min,
            self.window,
            self.gate,
            self.long_whistle,
        )
    }
}

impl Point {
    fn rule(&self) -> Rule {
        Rule {
            window_seconds: self.window,
            cheer_gates: self.gate,
            long_whistle_seconds: self.long_whistle,
        }
    }
}

/// Every combination the tuning match chooses from.
fn grid(long_whistle: f64) -> Vec<Point> {
    let mut out = Vec::new();
    for quantile in QUANTILE_SWEEP {
        for still_min in STILL_MIN_SWEEP {
            for cheer_snr in CHEER_SNR_SWEEP {
                for cheer_min in CHEER_MIN_SWEEP {
                    for window in WINDOW_SWEEP {
                        for gate in [true, false] {
                            out.push(Point {
                                quantile,
                                still_min,
                                cheer_snr,
                                cheer_min,
                                window,
                                gate,
                                long_whistle,
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

/// What one half's rule produced, positioned on its source.
fn detections(half: &Half, point: Point) -> Vec<Detection> {
    let kicks = half.kickoffs(point.quantile, point.still_min);
    let cheers = half.cheers(point.cheer_snr, point.cheer_min);
    suggest(&kicks, &cheers, &half.signals.whistles, point.rule())
        .into_iter()
        .map(|s| Detection {
            source_index: half.source_index,
            seconds: s.seconds,
            kind: s.kind,
        })
        .collect()
}

/// The whole rule: swept on the tuning match, chosen there, and read off the
/// held-out matches exactly once (G2).
fn rule(truths: &[Truth], halves: &[Half]) {
    // The long-whistle floor the period rule gets is chosen by the whistle
    // sweep above rather than inside this grid: periods and goals are separate
    // bars, and sweeping them together would let a period win pay for a goal
    // loss.
    let long_whistle = choose_long_whistle(truths, halves);
    let points = grid(long_whistle);

    let tuning = truths.first().expect("at least one match");
    let mut best: Option<(Point, f64, usize)> = None;
    for point in &points {
        let report = grade(tuning, halves, *point);
        let mut chance = Chance::default();
        for half in halves.iter().filter(|h| h.tuning) {
            chance.add(&detections(half, *point), half);
        }
        let lift = report
            .tally
            .goals
            .recall()
            .zip(chance.recall())
            .map(|(r, c)| r - c);
        println!(
            "RGRID  set=tuning {point} goal_tp={} goal_fp={} goal_fn={} \
             high_tp={} high_fp={} quiet_tp={} quiet_fp={} f1={} share={:.2} chance={} lift={}",
            report.tally.goals.tp,
            report.tally.goals.fp,
            report.tally.goals.misses,
            report.tally.goals_high.tp,
            report.tally.goals_high.fp,
            report.tally.goals_quiet.tp,
            report.tally.goals_quiet.fp,
            show_rate(f1(&report.tally)),
            chance.share(),
            show_rate(chance.recall()),
            show_rate(lift),
        );
        // **Lift, not F1**, and the reason is in the numbers: a goal window is
        // `W` seconds wide, so a grid this size always holds a point whose
        // windows cover half the match and whose recall is most of what
        // covering half a match gets you for nothing. F1 picks that point —
        // measured, it picked `W = 240 s` and 47% coverage — and lift is what
        // asks the only question worth asking, which is what the rule knows
        // beyond how much of the match it claimed. On a tie, fewer rows.
        let value = lift.unwrap_or(f64::MIN);
        let rows = report.tally.goals.tp + report.tally.goals.fp;
        let better = match best {
            None => true,
            Some((_, kept, kept_rows)) => {
                value > kept + 1e-9 || ((value - kept).abs() <= 1e-9 && rows < kept_rows)
            }
        };
        if better {
            best = Some((*point, value, rows));
        }
    }
    let (chosen, ..) = best.expect("the grid is not empty");
    println!("CHOSE  set=tuning {chosen}");

    // Held out, once, at the chosen point — and never touched again (G2).
    let mut held_out = Tally::default();
    let mut chance = Chance::default();
    for truth in truths {
        let mine: Vec<&Half> = halves
            .iter()
            .filter(|h| h.match_name == truth.name)
            .collect();
        let report = grade(truth, halves, chosen);
        report.tally.print("per_match", Some(&truth.name));
        report.print_diagnostics(&truth.name, &truth.events);
        let (seek_ok, seek_judged) = report.seek();
        println!("SEEK   match={} ok={seek_ok}/{seek_judged}", truth.name);
        for half in &mine {
            for onset in near_misses(
                &half.kickoffs(chosen.quantile, chosen.still_min),
                &half.cheers(chosen.cheer_snr, chosen.cheer_min),
                chosen.rule(),
            ) {
                println!(
                    "NEAR   {} cheer={onset:.1} reason=no_kickoff_within_W",
                    half.id()
                );
            }
        }
        if !mine[0].tuning {
            held_out.add(&report.tally);
            for half in &mine {
                chance.add(&detections(half, chosen), half);
            }
        }
    }
    held_out.print("held_out", None);
    chance.print("held_out");
}

/// The whistle floor the period rule runs at: the one that finds the most
/// period tags on the **tuning** match, ties going to the longer floor.
fn choose_long_whistle(truths: &[Truth], halves: &[Half]) -> f64 {
    let tuning: Vec<&Half> = halves.iter().filter(|h| h.tuning).collect();
    let truth = truths
        .iter()
        .find(|t| t.name == tuning[0].match_name)
        .expect("the tuning match was read");
    let mut best = (WHISTLE_LONG_SECONDS, 0usize);
    for seconds in LONG_WHISTLE_SWEEP {
        let mut found = Vec::new();
        for half in &tuning {
            let whistles = half.long_whistles(seconds);
            if let Some(first) = whistles.first() {
                found.push(Detection {
                    source_index: half.source_index,
                    seconds: first.start,
                    kind: SuggestionKind::PeriodStart,
                });
            }
            if let Some(last) = whistles.last() {
                found.push(Detection {
                    source_index: half.source_index,
                    seconds: last.start,
                    kind: SuggestionKind::PeriodEnd,
                });
            }
        }
        let tally = score(&truth.events, &found).tally;
        let hits = tally.period_start.tp + tally.period_end.tp;
        if hits > best.1 || (hits == best.1 && seconds > best.0) {
            best = (seconds, hits);
        }
    }
    println!(
        "CHOSE  set=tuning long_whistle={:.2} period_tags_found={}",
        best.0, best.1
    );
    best.0
}

/// One match's grade at one grid point, over both of its halves.
fn grade(truth: &Truth, halves: &[Half], point: Point) -> ScoreReport {
    let found: Vec<Detection> = halves
        .iter()
        .filter(|h| h.match_name == truth.name)
        .flat_map(|half| detections(half, point))
        .collect();
    score(&truth.events, &found)
}

/// The harmonic mean of a tally's goal precision and recall, or `None` when it
/// found nothing at all.
fn f1(tally: &Tally) -> Option<f64> {
    let (p, r) = (tally.goals.precision()?, tally.goals.recall()?);
    (p + r > 0.0).then(|| 2.0 * p * r / (p + r))
}

/// What a rule that knew nothing would have scored with the same windows.
///
/// A goal window covers a stretch of the half; a truth goal that fell
/// anywhere at random lands in the union of them with probability equal to
/// their share of the half. So `lift` is what the rule knows over and above
/// how much of the match it claimed — which is the only honest way to read a
/// rule that can be made to fire as often as you like.
#[derive(Debug, Clone, Copy, Default)]
struct Chance {
    covered: f64,
    seconds: f64,
    goals: usize,
    expected: f64,
    windows: usize,
}

impl Chance {
    fn add(&mut self, found: &[Detection], half: &Half) {
        let mut windows: Vec<(f64, f64)> = found
            .iter()
            .filter_map(|d| match d.kind {
                SuggestionKind::Goal { window, .. } => Some(window),
                _ => None,
            })
            .collect();
        windows.sort_by(|a, b| a.0.total_cmp(&b.0));
        // The union, because two windows that overlap cannot cover the same
        // second twice.
        let mut covered = 0.0;
        let mut edge = f64::NEG_INFINITY;
        for (start, end) in &windows {
            covered += (end - start.max(edge)).max(0.0);
            edge = edge.max(*end);
        }
        let goals = half
            .events
            .iter()
            .filter(|e| e.kind == TruthKind::Goal)
            .count();
        self.covered += covered;
        self.seconds += half.signals.video_seconds;
        self.goals += goals;
        self.windows += windows.len();
        self.expected += goals as f64 * (covered / half.signals.video_seconds.max(1.0)).min(1.0);
    }

    /// How much of the halves the windows claimed.
    fn share(&self) -> f64 {
        self.covered / self.seconds.max(1.0)
    }

    fn recall(&self) -> Option<f64> {
        (self.goals != 0).then(|| self.expected / self.goals as f64)
    }

    fn print(&self, set: &str) {
        println!(
            "CHANCE set={set} kind=goal windows={} covered={:.0}s of={:.0}s share={:.2} \
             expected_tp={:.1} chance_r={}",
            self.windows,
            self.covered,
            self.seconds,
            self.share(),
            self.expected,
            show_rate(self.recall()),
        );
    }
}

// -------------------------------------------------------------- the plumbing

/// One grid point's totals over every half in one set.
#[derive(Debug, Clone, Copy, Default)]
struct Firings {
    fired: usize,
    halves: usize,
    /// The most any single half produced: a rule that averages well and fires
    /// forty times in one half is not a rule the coach would keep.
    worst_half: usize,
    hits: usize,
    tags: usize,
    chance: f64,
}

impl Firings {
    fn add(&mut self, fired: usize, hits: usize, tags: usize) {
        self.fired += fired;
        self.halves += 1;
        self.worst_half = self.worst_half.max(fired);
        self.hits += hits;
        self.tags += tags;
    }

    /// How many of `tags` this many firings would have hit knowing nothing:
    /// each firing reaches `reach` seconds of a half `seconds` long.
    fn chance_from(&mut self, fired: usize, tags: usize, reach: f64, half: &Half) {
        let share = (reach / half.signals.video_seconds.max(1.0)).min(1.0);
        self.chance += tags as f64 * (1.0 - (1.0 - share).powi(fired as i32));
    }

    fn show(&self) -> String {
        let recall = (self.tags != 0).then(|| self.hits as f64 / self.tags as f64);
        // A floor rather than a precision where the tags are kick-offs: a
        // firing at a goal or a substitution is counted false because nothing
        // in this run knows what else it could be.
        let precision = (self.fired != 0).then(|| self.hits as f64 / self.fired as f64);
        let chance = (self.tags != 0).then(|| self.chance / self.tags as f64);
        format!(
            "fired={} per_half={:.1} worst_half={} hits={}/{} r={} p_floor={} chance={} lift={}",
            self.fired,
            self.fired as f64 / self.halves.max(1) as f64,
            self.worst_half,
            self.hits,
            self.tags,
            show_rate(recall),
            show_rate(precision),
            show_rate(chance),
            show_rate(recall.zip(chance).map(|(r, c)| r - c)),
        )
    }
}

/// Which of [`SETS`] a match counts towards.
fn set_names(tuning: bool) -> Vec<&'static str> {
    SETS.iter()
        .copied()
        .filter(|set| match *set {
            "tuning" => tuning,
            "held_out" => !tuning,
            _ => true,
        })
        .collect()
}

/// How well the tag's own neighbourhood scores, and how many seconds of the
/// half beat it.
fn tag_score(scores: &[f32], tag: f64) -> (f32, usize) {
    let first = ((tag - TAG_SEARCH) * THUMBNAIL_HZ).round().max(0.0) as usize;
    let last = (((tag + TAG_SEARCH) * THUMBNAIL_HZ).round() as usize).min(scores.len());
    let best = scores
        .get(first..last)
        .unwrap_or_default()
        .iter()
        .copied()
        .fold(f32::MIN, f32::max);
    let rank = scores.iter().filter(|&&s| s > best).count();
    (best, rank)
}

/// How far `at` is from the closest of `times`, or `None` when there are none.
fn nearest(times: &[f64], at: f64) -> Option<f64> {
    times.iter().map(|&t| (at - t).abs()).min_by(f64::total_cmp)
}

/// Everything the census rests on: the files a project names are there, and
/// every number it prints is finite.
fn check(truth: &Truth) {
    assert!(
        !truth.sources.is_empty(),
        "match {} has no sources",
        truth.name
    );
    for (i, path) in truth.sources.iter().enumerate() {
        assert!(
            path.is_file(),
            "match {} source {i} is missing — relink it in the app before measuring",
            truth.name
        );
    }
    for (i, d) in truth.durations.iter().enumerate() {
        assert!(
            d.is_finite() && *d > 0.0,
            "match {} source {i} has duration {d}",
            truth.name
        );
    }
    for e in &truth.events {
        assert!(
            e.seconds.is_finite() && e.seconds >= 0.0,
            "match {} has a {:?} tag at {}",
            truth.name,
            e.kind,
            e.seconds
        );
        assert!(
            e.source_index < truth.sources.len(),
            "match {} has a {:?} tag on source {}",
            truth.name,
            e.kind,
            e.source_index
        );
    }
}
