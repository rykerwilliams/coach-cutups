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

use std::fmt;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use video_coach_core::signals::{
    cheer_excess, cheers_from, clap_texture_at, claps_from, whistles, ClapTexture, Whistle,
    CHEER_MIN_SECONDS, CHEER_SNR_DB, CLAP_MIN_SECONDS, CLAP_RATE_SNR_DB, CLAP_TEXTURE_SECONDS,
    MEDIAN_SECONDS, ONSET_RISE_DB, SIGNAL_SAMPLE_RATE, WHISTLE_LONG_SECONDS, WHISTLE_MIN_SECONDS,
    WHISTLE_PITCH_HZ, WHISTLE_SNR_DB, WHISTLE_TONALITY_DB,
};
use video_coach_harness::score::{
    onset_coverage, print_audio_diagnostics, score, show_rate, Detection, ScoreReport, Tally,
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

/// The onset rises the texture pass is run at. This is the one clap threshold
/// that changes the series rather than reading it, so each value costs its own
/// pass over the samples; three is what a half's seconds will pay for.
const CLAP_RISE_SWEEP: [f32; 3] = [4.0, ONSET_RISE_DB, 9.0];

/// How far a texture must stand over its own rolling median. Wider than the
/// cheer's ±1 dB because nothing has ever measured this cue: 2 dB is "a quarter
/// more transients than usual" and 6 dB is "four times as many".
const CLAP_SNR_SWEEP: [f32; 4] = [2.0, CLAP_RATE_SNR_DB, 4.0, 6.0];

/// How long a texture must hold.
const CLAP_MIN_SWEEP: [f64; 3] = [0.5, CLAP_MIN_SECONDS, 2.0];

/// The sets every sweep is totalled over. The split is G2's: constants are
/// chosen on the tuning match and read off the held-out ones, and `all` exists
/// only so a number can be compared with the one Task 3.3 printed.
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

    let (tuning, held_out) = truths.split_first().expect("at least one match");
    println!(
        "RUN    tuning={} held_out={} period_tolerance={PERIOD_TOLERANCE:.1}s \
         seek_lead={SEEK_LEAD:.1}s seek_window={SEEK_WINDOW:.1}s \
         cheer_tolerance={CHEER_TOLERANCE:.1}s median={MEDIAN_SECONDS:.0}s \
         whistle_snr={WHISTLE_SNR_DB:.1}dB whistle_tonality={WHISTLE_TONALITY_DB:.1}dB \
         whistle_pitch={WHISTLE_PITCH_HZ:.0}Hz whistle_min={WHISTLE_MIN_SECONDS:.2}s \
         whistle_long={WHISTLE_LONG_SECONDS:.1}s cheer_snr={CHEER_SNR_DB:.1}dB \
         cheer_min={CHEER_MIN_SECONDS:.1}s onset_rise={ONSET_RISE_DB:.1}dB \
         clap_block={CLAP_TEXTURE_SECONDS:.1}s clap_rate_snr={CLAP_RATE_SNR_DB:.1}dB \
         clap_min={CLAP_MIN_SECONDS:.1}s \
         detector=none",
        tuning.name,
        held_out
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );

    // Two cues on the same tags, at the same tolerance, over the same halves:
    // the level rule Task 3.3 measured, and the texture — how many sharp
    // transients a second the 2 kHz-up band holds. The question is whether the
    // texture finds goals the level cue cannot hear.
    let mut grids = [Grid::level("cheer"), Grid::texture("clap_rate")];
    let mut union = [Sweep::default(); SETS.len()];

    // No detector exists yet (Tasks 3.4–3.5 build it), so every match is
    // scored against an empty detection set: precision undefined, recall 0.
    // What the audio pass adds here is the `SIGNAL`, `DIAG` and `SWEEP` lines
    // — whether the cue is in the sound at all, before any rule reads it.
    let detected: Vec<Detection> = Vec::new();
    let mut aggregate = Tally::default();
    for (i, truth) in truths.iter().enumerate() {
        // G2's split, and the only place it is decided: the first folder named
        // is the tuning match and nothing else is.
        let sets: &[usize] = if i == 0 { &[0, 2] } else { &[1, 2] };
        for (src, path) in truth.sources.iter().enumerate() {
            let analysis = analyse(path);
            analysis.print(&truth.name, src);

            let cheers = cheers_from(&analysis.excess, CHEER_SNR_DB, CHEER_MIN_SECONDS);
            let claps = claps_from(
                &analysis.texture(ONSET_RISE_DB).rate_excess,
                &analysis.texture(ONSET_RISE_DB).rate,
                CLAP_RATE_SNR_DB,
                CLAP_MIN_SECONDS,
            );
            print_audio_diagnostics(
                &truth.name,
                src,
                &truth.events,
                &analysis.whistles,
                &cheers,
                &claps,
            );

            for grid in &mut grids {
                for (index, point) in grid.points.clone().iter().enumerate() {
                    let onsets = analysis.onsets(grid.feature, *point);
                    let (covered, goals) = onset_coverage(&truth.events, src, &onsets);
                    println!(
                        "SWEEP  match={} src={src} feature={} {point} bursts={} \
                         goals_covered={covered}/{goals}",
                        truth.name,
                        grid.feature,
                        onsets.len(),
                    );
                    for &set in sets {
                        grid.cells[index][set].add(onsets.len(), covered, goals, analysis.seconds);
                    }
                }
            }

            // The one number that says whether the texture cue is worth having
            // even if it loses on its own: how much of the match the two cues
            // cover between them at their initial constants.
            let mut both: Vec<f64> = cheers.iter().map(|c| c.onset).collect();
            both.extend(claps.iter().map(|c| c.onset));
            let (covered, goals) = onset_coverage(&truth.events, src, &both);
            for &set in sets {
                union[set].add(both.len(), covered, goals, analysis.seconds);
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
    for grid in &grids {
        grid.print();
    }
    for (set, totals) in union.iter().enumerate() {
        totals.print(
            SETS[set],
            "cheer_or_clap",
            Point {
                rise: None,
                snr: f32::NAN,
                min: f64::NAN,
            },
        );
    }
}

/// One point of a feature's threshold grid.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    /// The onset rise the texture pass ran at — `None` for a feature the rise
    /// does not reach.
    rise: Option<f32>,
    snr: f32,
    min: f64,
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.rise {
            Some(rise) => write!(f, "rise={rise:.1} ")?,
            None => write!(f, "rise=n/a ")?,
        }
        write!(f, "snr={:.1} min={:.1}", self.snr, self.min)
    }
}

/// One feature's totals over its grid, for each of [`SETS`].
struct Grid {
    feature: &'static str,
    points: Vec<Point>,
    cells: Vec<[Sweep; SETS.len()]>,
}

impl Grid {
    fn new(feature: &'static str, points: Vec<Point>) -> Grid {
        let cells = vec![[Sweep::default(); SETS.len()]; points.len()];
        Grid {
            feature,
            points,
            cells,
        }
    }

    /// The level cue's grid, exactly as Task 3.3 swept it.
    fn level(feature: &'static str) -> Grid {
        let mut points = Vec::new();
        for &snr in &CHEER_SNR_SWEEP {
            for &min in &CHEER_MIN_SWEEP {
                points.push(Point {
                    rise: None,
                    snr,
                    min,
                });
            }
        }
        Grid::new(feature, points)
    }

    /// The onset-rate grid: the rise as well, because it is what the series is
    /// made of.
    fn texture(feature: &'static str) -> Grid {
        let mut points = Vec::new();
        for &rise in &CLAP_RISE_SWEEP {
            for &snr in &CLAP_SNR_SWEEP {
                for &min in &CLAP_MIN_SWEEP {
                    points.push(Point {
                        rise: Some(rise),
                        snr,
                        min,
                    });
                }
            }
        }
        Grid::new(feature, points)
    }

    fn print(&self) {
        for (point, cells) in self.points.iter().zip(&self.cells) {
            for (set, totals) in cells.iter().enumerate() {
                totals.print(SETS[set], self.feature, *point);
            }
        }
    }
}

/// What one source's sound says, and what it cost to find out.
struct Analysis {
    whistles: Vec<Whistle>,
    /// The cheer band over its rolling median, kept rather than the cheers so
    /// the sweep costs nothing.
    excess: Vec<f32>,
    /// One texture per [`CLAP_RISE_SWEEP`] value, in that order.
    textures: Vec<ClapTexture>,
    /// How much sound there was, which is what the timing lines are per.
    seconds: f64,
    decode: Duration,
    signals: Duration,
    texture: Duration,
}

impl Analysis {
    /// The texture measured at `rise`.
    fn texture(&self, rise: f32) -> &ClapTexture {
        let at = CLAP_RISE_SWEEP
            .iter()
            .position(|r| *r == rise)
            .expect("the rise is one this run measured");
        &self.textures[at]
    }

    /// Where one feature says a burst began, at one grid point — the one shape
    /// every cue is graded in, so the comparison between them is like for like.
    fn onsets(&self, feature: &str, point: Point) -> Vec<f64> {
        match feature {
            "cheer" => cheers_from(&self.excess, point.snr, point.min)
                .iter()
                .map(|c| c.onset)
                .collect(),
            "clap_rate" => {
                let texture = self.texture(point.rise.expect("the rate grid carries a rise"));
                claps_from(&texture.rate_excess, &texture.rate, point.snr, point.min)
                    .iter()
                    .map(|c| c.onset)
                    .collect()
            }
            other => panic!("no feature called {other}"),
        }
    }

    /// One `SIGNAL` line: what the pass found and what it cost (V-6's audio
    /// half; the motion pass is Task 3.4's).
    fn print(&self, match_name: &str, source_index: usize) {
        let texture = self.texture(ONSET_RISE_DB);
        let median = |values: &[f32]| {
            let mut sorted = values.to_vec();
            sorted.sort_by(f32::total_cmp);
            sorted.get(sorted.len() / 2).copied().unwrap_or(0.0)
        };
        println!(
            "SIGNAL match={match_name} src={source_index} dur={:.0} decode_s={:.1} \
             signals_s={:.1} texture_s={:.1} realtime={:.0}x whistles={} long={} longest={:.2} \
             onset_rate_median={:.1} onset_rate_max={:.1}",
            self.seconds,
            self.decode.as_secs_f64(),
            self.signals.as_secs_f64(),
            self.texture.as_secs_f64(),
            self.seconds / (self.decode + self.signals + self.texture).as_secs_f64(),
            self.whistles.len(),
            self.whistles.iter().filter(|w| w.is_long()).count(),
            // What the long floor would have to come down to to find any:
            // `long=0` on its own says nothing about whether the floor is
            // wrong or the whistles are absent.
            self.whistles.iter().map(|w| w.duration).fold(0.0, f64::max),
            // The baseline the texture's dB are over. A half whose background
            // already holds thirty transients a second has no headroom for
            // applause to stand out in, and that is a fact about the venue
            // rather than about the threshold.
            median(&texture.rate),
            texture.rate.iter().copied().fold(0.0, f32::max),
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
    let signals = started.elapsed();
    let started = Instant::now();
    let textures = CLAP_RISE_SWEEP
        .iter()
        .map(|&rise| clap_texture_at(&samples, rise))
        .collect();
    Analysis {
        whistles,
        excess,
        textures,
        seconds: samples.len() as f64 / f64::from(SIGNAL_SAMPLE_RATE),
        decode,
        signals,
        texture: started.elapsed(),
    }
}

/// One grid point's totals over every half in one set.
#[derive(Debug, Clone, Copy, Default)]
struct Sweep {
    bursts: usize,
    halves: usize,
    /// The most any single half produced.
    worst_half: usize,
    covered: usize,
    goals: usize,
    /// How many goals this many bursts would have covered if they had fallen
    /// at random — see [`Sweep::print`].
    chance: f64,
}

impl Sweep {
    fn add(&mut self, bursts: usize, covered: usize, goals: usize, seconds: f64) {
        self.bursts += bursts;
        self.halves += 1;
        self.worst_half = self.worst_half.max(bursts);
        self.covered += covered;
        self.goals += goals;
        // A goal is "covered" when some burst's onset lands within
        // CHEER_TOLERANCE of it, so `bursts` onsets scattered at random over a
        // half of `seconds` cover it with probability
        // `1 − (1 − 2·tolerance/seconds)^bursts`. This is not a nicety: a grid
        // point that fires every fourteen seconds covers seven goals in ten
        // *knowing nothing*, and without this column that reads as a detector.
        let reach = (2.0 * CHEER_TOLERANCE / seconds).min(1.0);
        self.chance += goals as f64 * (1.0 - (1.0 - reach).powi(bursts as i32));
    }

    fn print(&self, set: &str, feature: &str, point: Point) {
        let recall = (self.goals != 0).then(|| self.covered as f64 / self.goals as f64);
        // Firings a half, not firings: the sets hold different numbers of
        // halves, and a rule is kept or dropped on what one half of it looks
        // like to the coach.
        let per_half = (self.halves != 0).then(|| self.bursts as f64 / self.halves as f64);
        // `lift` is the only column that compares two cues fairly: recall
        // alone rewards a rule for firing more often, and these rules can be
        // made to fire as often as you like.
        let chance = (self.goals != 0).then(|| self.chance / self.goals as f64);
        println!(
            "SWEEP  set={set} feature={feature} {point} bursts={} per_half={} \
             worst_half={} goals_covered={}/{} r={} chance={} lift={}",
            self.bursts,
            per_half.map_or("n/a".to_string(), |r| format!("{r:.1}")),
            self.worst_half,
            self.covered,
            self.goals,
            show_rate(recall),
            show_rate(chance),
            show_rate(recall.zip(chance).map(|(r, c)| r - c)),
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
