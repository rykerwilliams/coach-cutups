//! The picture half of the measurement (Task 3.4): what the motion pass sees
//! on the coach's own tagged matches, and whether a kick-off is a **known
//! picture**.
//!
//! `#[ignore]`d, like `ground_truth.rs`, and for the same reason: it needs
//! whole matches of real footage that CI has no copy of and never will. Run it
//! on the reference laptop with
//!
//! ```text
//! COACH_GROUND_TRUTH=B=/local/b:A=/local/a:C=/local/c \
//!   flock /tmp/claude-1000/cargo.lock nice -n 19 \
//!   cargo test --release -p video-coach-harness --test motion_truth \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! **A separate run from `ground_truth.rs` while P3 is being built**, because
//! it needs no sound: the motion pass is the expensive half and the audio pass
//! is not read here at all. Task 3.5 scores both cues against one rule and
//! folds the two runs into one.
//!
//! **`--release`.** The pass decodes six whole halves.
//!
//! **The folders are read-only:** `store::read`, `kickoffs.txt` and the source
//! videos, and nothing else. Nothing identifying reaches the terminal.
//!
//! # The two cues, measured side by side
//!
//! - **Stillness** (spec D3) is the plan's own: a still interval, then motion
//!   again. It knows nothing about football, so the `STILL` lines are mostly a
//!   count of how often it fires against how many kick-offs there are.
//! - **Picture similarity** is the coach's: "kick offs are a kind of known
//!   picture". The camera is a fixed tripod with a virtual pan, so two
//!   kick-offs of the same match are framed almost identically — and the six
//!   tagged period starts are themselves kick-off frames, which is a template
//!   with no extra tagging asked of anybody.
//!
//! **What is held out, exactly.** A template never contains a frame from the
//! match it is scoring. `template=cross` is built from the **other matches'**
//! period starts only; `template=other_half` from the **same match's other
//! half**. So `cross` is the answer to "does a template from one match find
//! kick-offs in another", which is the question that decides whether this
//! ships as a fixed detector or needs calibrating per match; `other_half` is
//! the same question one venue in, and the gap between the two is the price of
//! changing grounds.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use video_coach_core::motion::{
    peaks, still_intervals_at, Template, Thumbnail, KICKOFF_MIN_GAP_SECONDS, KICKOFF_SIMILARITY,
    MOTION_HZ, STILL_MIN_SECONDS, STILL_THETA, THUMBNAIL_HEIGHT, THUMBNAIL_HZ, THUMBNAIL_WIDTH,
};
use video_coach_harness::score::show_rate;
use video_coach_harness::truth::{folders, Truth, TruthKind};
use video_coach_media::analyze;

/// The stillness thresholds the run reports at. The probe behind the plan saw
/// 19–21 intervals a half at **every** one of these, which is the claim this
/// run confirms or refutes over six halves.
const STILL_THETA_SWEEP: [f32; 5] = [2.0, 3.0, 4.0, 5.0, 6.0];

/// The stillness floors. A shorter floor finds a kick-off whose walk-back was
/// brief and costs candidates everywhere else, which is the whole trade.
const STILL_MIN_SWEEP: [f64; 3] = [6.0, STILL_MIN_SECONDS, 15.0];

/// The similarity thresholds. Nothing has ever measured this cue, so the sweep
/// is wide: 0.5 is "vaguely the same scene" and 0.9 is "very nearly the same
/// picture".
const SIMILARITY_SWEEP: [f32; 5] = [0.5, 0.6, 0.7, KICKOFF_SIMILARITY, 0.9];

/// How much of a tagged kick-off goes into a template: the tag's own second
/// and this many either side, so a tag a second or two early still carries the
/// framing. Five frames per kick-off.
const TEMPLATE_RADIUS: f64 = 2.0;

/// How far a firing may sit from a tag and still be that kick-off. The same
/// tolerance the scorer gives a period event, for the same reason: it is a bar
/// on the coach's tagging as much as on the detector.
const KICKOFF_TOLERANCE: f64 = 10.0;

/// The sets every sweep is totalled over. `tuning` is the first folder in
/// `COACH_GROUND_TRUTH`, `held_out` is the rest, and `all` exists only so a
/// number can be compared with one measured over everything.
const SETS: [&str; 3] = ["tuning", "held_out", "all"];

/// How far either side of a tag the best score is looked for, when the
/// question is how distinctive the picture is rather than whether a rule
/// fired.
const TAG_SEARCH: f64 = 5.0;

#[test]
#[ignore = "needs the coach's tagged matches: see this file's module docs"]
fn motion_truth() {
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

    println!(
        "RUN    matches={} motion_hz={MOTION_HZ:.1} thumbnail_hz={THUMBNAIL_HZ:.1} \
         thumbnail={THUMBNAIL_WIDTH}x{THUMBNAIL_HEIGHT} still_theta={STILL_THETA:.1} \
         still_min={STILL_MIN_SECONDS:.1}s similarity={KICKOFF_SIMILARITY:.2} \
         peak_gap={KICKOFF_MIN_GAP_SECONDS:.0}s template_radius={TEMPLATE_RADIUS:.1}s \
         kickoff_tolerance={KICKOFF_TOLERANCE:.1}s",
        truths.len(),
    );

    // G2's split: the first folder is the tuning match and the rest are held
    // out. Nothing here *chooses* a constant, so the split is reported rather
    // than obeyed — but a number read off the match a threshold was picked on
    // is worth less than the same number from a match it has never seen, and
    // the report has to say which it is.
    let tuning = truths.first().expect("at least one match").name.clone();

    // One pass over the footage, everything else read off what it produced.
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

    stillness(&halves);
    picture(&halves);
}

/// One half's picture, and what it cost to read.
struct Half {
    /// `A`, `B`, `C` … — never the folder name.
    match_name: String,
    source_index: usize,
    /// From the tuning match, the first folder in `COACH_GROUND_TRUTH` (G2).
    tuning: bool,
    /// Every kick-off tagged on this half: the period start, and the restarts
    /// `kickoffs.txt` names if it exists.
    kickoffs: Vec<f64>,
    /// The tagged period start, which is a kick-off frame and the only one a
    /// template is ever built from.
    period_start: Option<f64>,
    motion: Vec<f32>,
    thumbnails: Vec<Thumbnail>,
    seconds: f64,
    cost: Duration,
}

impl Half {
    fn analyse(truth: &Truth, source_index: usize, path: &Path, tuning: bool) -> Half {
        // Nothing cancels a measurement run; the flag is what the pass takes.
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let series = analyze::motion::series(path, &cancel).expect("the source has picture");
        let cost = started.elapsed();
        let half = Half {
            match_name: truth.name.clone(),
            source_index,
            tuning,
            kickoffs: truth
                .on(source_index, TruthKind::PeriodStart)
                .chain(truth.on(source_index, TruthKind::Restart))
                .map(|e| e.seconds)
                .collect(),
            period_start: truth
                .on(source_index, TruthKind::PeriodStart)
                .next()
                .map(|e| e.seconds),
            motion: series.motion,
            thumbnails: series.thumbnails,
            seconds: series.seconds,
            cost,
        };
        // V-6: the pass's wall time per file, stated per file length rather
        // than per "half" — one match plays 30-minute halves against two on
        // 25, so a single "per half" figure would mean nothing.
        println!(
            "MOTION match={} src={source_index} dur={:.0} frames={} hz={:.2} \
             thumbnails={} pass_s={:.1} realtime={:.0}x kickoff_tags={}",
            half.match_name,
            half.seconds,
            series.frames,
            series.frames as f64 / half.seconds.max(1.0),
            half.thumbnails.len(),
            half.cost.as_secs_f64(),
            half.seconds / half.cost.as_secs_f64(),
            half.kickoffs.len(),
        );
        // V-8: whether the raw thumbnail difference separates a walk-back from
        // play at all. Global motion is **not** removed, so the virtual
        // camera's pan is in these numbers on purpose; if the quantiles do not
        // separate, that is the measurement that would justify removing it.
        println!(
            "MDIST  match={} src={source_index} p05={:.2} p25={:.2} p50={:.2} \
             p75={:.2} p95={:.2} max={:.2}",
            half.match_name,
            quantile(&half.motion, 0.05),
            quantile(&half.motion, 0.25),
            quantile(&half.motion, 0.50),
            quantile(&half.motion, 0.75),
            quantile(&half.motion, 0.95),
            quantile(&half.motion, 1.0),
        );
        half
    }

    /// Which totals this half counts towards.
    fn sets(&self) -> Vec<&'static str> {
        SETS.iter()
            .copied()
            .filter(|set| match *set {
                "tuning" => self.tuning,
                "held_out" => !self.tuning,
                _ => true,
            })
            .collect()
    }

    fn id(&self) -> String {
        format!("match={} src={}", self.match_name, self.source_index)
    }

    /// The thumbnails within [`TEMPLATE_RADIUS`] of `seconds`.
    fn around(&self, seconds: f64) -> Vec<Thumbnail> {
        let first = ((seconds - TEMPLATE_RADIUS) * THUMBNAIL_HZ)
            .round()
            .max(0.0) as usize;
        let last = (((seconds + TEMPLATE_RADIUS) * THUMBNAIL_HZ).round() as usize)
            .min(self.thumbnails.len().saturating_sub(1));
        self.thumbnails
            .get(first..=last)
            .unwrap_or_default()
            .to_vec()
    }
}

/// The stillness cue (spec D3), swept.
///
/// A still interval's **end** is what the kick-off pattern reads as the
/// restart, so that is what a tag is matched against.
fn stillness(halves: &[Half]) {
    let mut totals: BTreeMap<(&str, String, String), Firings> = BTreeMap::new();
    let mut halves_in: BTreeMap<&str, usize> = BTreeMap::new();
    for half in halves {
        for set in half.sets() {
            *halves_in.entry(set).or_default() += 1;
        }
        for min_seconds in STILL_MIN_SWEEP {
            for theta in STILL_THETA_SWEEP {
                let intervals = still_intervals_at(&half.motion, MOTION_HZ, theta, min_seconds);
                let ends: Vec<f64> = intervals.iter().map(|i| i.end).collect();
                let hits = half
                    .kickoffs
                    .iter()
                    .filter(|&&tag| nearest(&ends, tag).is_some_and(|d| d <= KICKOFF_TOLERANCE))
                    .count();
                println!(
                    "STILL  {} theta={theta:.1} min={min_seconds:.1} intervals={} \
                     kickoffs={hits}/{}",
                    half.id(),
                    intervals.len(),
                    half.kickoffs.len(),
                );
                for set in half.sets() {
                    totals
                        .entry((set, format!("{theta:.1}"), format!("{min_seconds:.1}")))
                        .or_default()
                        .add(intervals.len(), hits, half.kickoffs.len());
                }
            }
        }
    }
    for ((set, theta, min_seconds), firings) in &totals {
        println!(
            "SSWEEP set={set} theta={theta} min={min_seconds} {}",
            firings.show(halves_in.get(set).copied().unwrap_or_default())
        );
    }
}

/// The picture cue, against templates that never contain the match they score.
fn picture(halves: &[Half]) {
    // Every match's tagged period starts, as thumbnails: the frames a
    // template is built out of. Restarts are targets, never template
    // material — the coach has not been asked to tag one precisely, and
    // `kickoffs.txt` may not exist at all.
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
    let mut halves_in: BTreeMap<&str, usize> = BTreeMap::new();
    for half in halves {
        for set in half.sets() {
            *halves_in.entry(set).or_default() += 1;
        }
        for kind in ["cross", "other_half"] {
            // Held out by construction: `cross` drops this match entirely,
            // `other_half` drops this half's own frames from its match.
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
            let scores = template.scores(&half.thumbnails);

            // How distinctive a kick-off is, before any threshold: its best
            // score in the neighbourhood of the tag, and how many seconds of
            // the half score higher. Rank 0 means the kick-off is the single
            // most template-like second of the whole half.
            for &tag in &half.kickoffs {
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
                let hits = half
                    .kickoffs
                    .iter()
                    .filter(|&&tag| nearest(&times, tag).is_some_and(|d| d <= KICKOFF_TOLERANCE))
                    .count();
                println!(
                    "PSWEEP {} template={kind} threshold={threshold:.2} peaks={} \
                     kickoffs={hits}/{}",
                    half.id(),
                    found.len(),
                    half.kickoffs.len(),
                );
                for set in half.sets() {
                    totals
                        .entry((set, kind.to_owned(), format!("{threshold:.2}")))
                        .or_default()
                        .add(found.len(), hits, half.kickoffs.len());
                }
            }

            // Where the peaks actually fall: the strongest few, each with its
            // distance to the nearest tag. A detector that fires five times a
            // half is judged by these, not by a rate.
            let mut found = peaks(
                &scores,
                THUMBNAIL_HZ,
                SIMILARITY_SWEEP[0],
                KICKOFF_MIN_GAP_SECONDS,
            );
            found.sort_by(|a, b| b.score.total_cmp(&a.score));
            for peak in found.iter().take(5) {
                let offset = nearest_signed(&half.kickoffs, peak.seconds);
                println!(
                    "PEAK   {} template={kind} at={:.0} score={:.3} nearest_tag={}",
                    half.id(),
                    peak.seconds,
                    peak.score,
                    offset.map_or_else(|| "none".to_owned(), |d| format!("{d:+.0}")),
                );
            }
        }
    }

    for ((set, kind, threshold), firings) in &totals {
        println!(
            "PSWEEP set={set} template={kind} threshold={threshold} {}",
            firings.show(halves_in.get(set).copied().unwrap_or_default())
        );
    }
    // The one line that answers "does a template port across matches": the
    // same tags scored by a template from another match, and by one from the
    // same match's other half. If `cross` is far below `other_half`, a shipped
    // detector needs calibrating per match.
    for ((set, kind), best) in &distinctive {
        let mean = best.iter().map(|&b| f64::from(b)).sum::<f64>() / best.len().max(1) as f64;
        let worst = best.iter().copied().fold(f32::MAX, f32::min);
        println!(
            "PORT   set={set} template={kind} tags={} mean_best={mean:.3} worst_best={worst:.3}",
            best.len(),
        );
    }
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
    nearest_signed(times, at).map(f64::abs)
}

/// [`nearest`], keeping the sign: positive when `at` is after the tag.
fn nearest_signed(times: &[f64], at: f64) -> Option<f64> {
    times
        .iter()
        .map(|&t| at - t)
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
}

/// The `q`th quantile of `values`, by nearest rank.
fn quantile(values: &[f32], q: f64) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted[((q * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1)]
}

/// One grid point's totals over every half in the run.
#[derive(Debug, Clone, Copy, Default)]
struct Firings {
    fired: usize,
    /// The most any single half produced: a rule that averages well and fires
    /// forty times in one half is not a rule the coach would keep.
    worst_half: usize,
    hits: usize,
    tags: usize,
}

impl Firings {
    fn add(&mut self, fired: usize, hits: usize, tags: usize) {
        self.fired += fired;
        self.worst_half = self.worst_half.max(fired);
        self.hits += hits;
        self.tags += tags;
    }

    fn show(&self, halves: usize) -> String {
        let recall = (self.tags != 0).then(|| self.hits as f64 / self.tags as f64);
        // Precision against kick-off tags only, and it is a floor rather than
        // a precision: a firing at a goal or a substitution is counted false
        // here because nothing in this run knows what else it could be.
        let precision = (self.fired != 0).then(|| self.hits as f64 / self.fired as f64);
        format!(
            "fired={} per_half={:.1} worst_half={} kickoffs={}/{} r={} p_floor={}",
            self.fired,
            self.fired as f64 / halves.max(1) as f64,
            self.worst_half,
            self.hits,
            self.tags,
            show_rate(recall),
            show_rate(precision),
        )
    }
}
