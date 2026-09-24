//! The measurement tool: grade the detector against the coach's own tagged
//! matches (spec G3).
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
//! **`--release`, and it is not optional for the timing lines.** The whistle
//! bank is forty Goertzel evaluations over every 32 ms window of a half, and
//! an unoptimised build spends about forty times as long on it as the one the
//! coach would run.
//!
//! **The folders are read-only.** This test opens each project with
//! `store::read`, reads `kickoffs.txt` and decodes the source videos; it never
//! builds a `Bus`, never calls `store::write`, and writes no file inside a
//! project folder.
//!
//! `--test-threads=1` because the analysis decodes whole halves, and two at
//! once would fight over the decoder and ruin every timing line.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use video_coach_core::signals::{
    cheer_excess, cheers_from, whistles, Whistle, CHEER_MIN_SECONDS, CHEER_SNR_DB, MEDIAN_SECONDS,
    SIGNAL_SAMPLE_RATE, WHISTLE_LONG_SECONDS, WHISTLE_MIN_SECONDS, WHISTLE_PITCH_HZ,
    WHISTLE_SNR_DB, WHISTLE_TONALITY_DB,
};
use video_coach_harness::score::{
    cheer_coverage, print_audio_diagnostics, score, show_rate, Detection, ScoreReport, Tally,
    CHEER_TOLERANCE, PERIOD_TOLERANCE, SEEK_LEAD, SEEK_WINDOW,
};
use video_coach_harness::truth::{folders, Truth};
use video_coach_media::analyze;

/// The cheer thresholds the run reports at: the initial value and one step
/// either side of it (Task 3.3). The analysis is what costs minutes and the
/// rule is pure, so a grid is free once the samples are read.
const CHEER_SNR_SWEEP: [f32; 3] = [CHEER_SNR_DB - 1.0, CHEER_SNR_DB, CHEER_SNR_DB + 1.0];
const CHEER_MIN_SWEEP: [f64; 3] = [
    CHEER_MIN_SECONDS - 0.5,
    CHEER_MIN_SECONDS,
    CHEER_MIN_SECONDS + 0.5,
];

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

    let (tuning, held_out) = truths.split_first().expect("at least one match");
    println!(
        "RUN    tuning={} held_out={} period_tolerance={PERIOD_TOLERANCE:.1}s \
         seek_lead={SEEK_LEAD:.1}s seek_window={SEEK_WINDOW:.1}s \
         cheer_tolerance={CHEER_TOLERANCE:.1}s median={MEDIAN_SECONDS:.0}s \
         whistle_snr={WHISTLE_SNR_DB:.1}dB whistle_tonality={WHISTLE_TONALITY_DB:.1}dB \
         whistle_pitch={WHISTLE_PITCH_HZ:.0}Hz whistle_min={WHISTLE_MIN_SECONDS:.2}s \
         whistle_long={WHISTLE_LONG_SECONDS:.1}s cheer_snr={CHEER_SNR_DB:.1}dB \
         cheer_min={CHEER_MIN_SECONDS:.1}s detector=none",
        tuning.name,
        held_out
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );

    // The sweep's totals, indexed as CHEER_SNR_SWEEP × CHEER_MIN_SWEEP: how
    // many cheers the whole set produced, how many of the sixteen truth goals
    // one covered, and the worst single half — a rule that averages well and
    // fires forty times in one half is not a rule the coach would keep.
    let mut sweep = [[Sweep::default(); CHEER_MIN_SWEEP.len()]; CHEER_SNR_SWEEP.len()];

    // No detector exists yet (Tasks 3.4–3.5 build it), so every match is
    // scored against an empty detection set: precision undefined, recall 0.
    // What the audio pass adds here is the `SIGNAL`, `DIAG` and `SWEEP` lines
    // — whether the cue is in the sound at all, before any rule reads it.
    let detected: Vec<Detection> = Vec::new();
    let mut aggregate = Tally::default();
    for (i, truth) in truths.iter().enumerate() {
        for (src, path) in truth.sources.iter().enumerate() {
            let analysis = analyse(path);
            analysis.print(&truth.name, src);
            let cheers = cheers_from(&analysis.excess, CHEER_SNR_DB, CHEER_MIN_SECONDS);
            print_audio_diagnostics(&truth.name, src, &truth.events, &analysis.whistles, &cheers);
            for (s, &snr) in CHEER_SNR_SWEEP.iter().enumerate() {
                for (m, &min) in CHEER_MIN_SWEEP.iter().enumerate() {
                    let cheers = cheers_from(&analysis.excess, snr, min);
                    let (covered, goals) = cheer_coverage(&truth.events, src, &cheers);
                    println!(
                        "SWEEP  match={} src={src} cheer_snr={snr:.1} cheer_min={min:.1} \
                         cheers={} goals_covered={covered}/{goals}",
                        truth.name,
                        cheers.len(),
                    );
                    sweep[s][m].add(cheers.len(), covered, goals);
                }
            }
        }

        let report: ScoreReport = score(&truth.events, &detected);
        report.tally.print("per_match", Some(&truth.name));
        report.print_diagnostics(&truth.name, &truth.events);
        // The tuning match is excluded: a tool that judges on the match it
        // chose its constants on passes anything (G2).
        if i > 0 {
            aggregate.add(&report.tally);
        }
    }
    aggregate.print("held_out", None);
    for (s, &snr) in CHEER_SNR_SWEEP.iter().enumerate() {
        for (m, &min) in CHEER_MIN_SWEEP.iter().enumerate() {
            sweep[s][m].print(snr, min);
        }
    }
}

/// What one source's sound says, and what it cost to find out.
struct Analysis {
    whistles: Vec<Whistle>,
    /// The cheer band over its rolling median, kept rather than the cheers so
    /// the sweep costs nothing.
    excess: Vec<f32>,
    /// How much sound there was, which is what the timing lines are per.
    seconds: f64,
    decode: Duration,
    signals: Duration,
}

impl Analysis {
    /// One `SIGNAL` line: what the pass found and what it cost (V-6's audio
    /// half; the motion pass is Task 3.4's).
    fn print(&self, match_name: &str, source_index: usize) {
        println!(
            "SIGNAL match={match_name} src={source_index} dur={:.0} decode_s={:.1} \
             signals_s={:.1} realtime={:.0}x whistles={} long={} longest={:.2}",
            self.seconds,
            self.decode.as_secs_f64(),
            self.signals.as_secs_f64(),
            self.seconds / (self.decode + self.signals).as_secs_f64(),
            self.whistles.len(),
            self.whistles.iter().filter(|w| w.is_long()).count(),
            // What the long floor would have to come down to to find any:
            // `long=0` on its own says nothing about whether the floor is
            // wrong or the whistles are absent.
            self.whistles.iter().map(|w| w.duration).fold(0.0, f64::max),
        );
    }
}

fn analyse(path: &Path) -> Analysis {
    // Nothing cancels a measurement run: the flag is what the pass takes, and
    // a job that can be cancelled is Task 3.4's `Analyzer`.
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let samples = analyze::audio::samples(path, &cancel).expect("the source has sound");
    let decode = started.elapsed();
    let started = Instant::now();
    let whistles = whistles(&samples);
    let excess = cheer_excess(&samples);
    Analysis {
        whistles,
        excess,
        seconds: samples.len() as f64 / f64::from(SIGNAL_SAMPLE_RATE),
        decode,
        signals: started.elapsed(),
    }
}

/// One grid point's totals over every half in the run.
#[derive(Debug, Clone, Copy, Default)]
struct Sweep {
    cheers: usize,
    /// The most any single half produced.
    worst_half: usize,
    covered: usize,
    goals: usize,
}

impl Sweep {
    fn add(&mut self, cheers: usize, covered: usize, goals: usize) {
        self.cheers += cheers;
        self.worst_half = self.worst_half.max(cheers);
        self.covered += covered;
        self.goals += goals;
    }

    fn print(&self, snr: f32, min: f64) {
        let recall = (self.goals != 0).then(|| self.covered as f64 / self.goals as f64);
        println!(
            "SWEEP  set=all cheer_snr={snr:.1} cheer_min={min:.1} cheers={} \
             worst_half={} goals_covered={}/{} r={}",
            self.cheers,
            self.worst_half,
            self.covered,
            self.goals,
            show_rate(recall),
        );
    }
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
