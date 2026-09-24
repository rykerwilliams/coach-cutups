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
//!   cargo test -p video-coach-harness --test ground_truth \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `:`-separated project folders, **first is the tuning match** (G2) and the
//! rest are held out. An entry may carry a one- or two-character label
//! (`B=/local/b`); without one a match is named by its position, `A`, `B`,
//! `C` … Either way nothing identifying — no folder name, no team name — ever
//! reaches the terminal.
//!
//! **The folders are read-only.** This test opens each project with
//! `store::read` and reads `kickoffs.txt`; it never builds a `Bus`, never
//! calls `store::write`, and writes no file inside a project folder.
//!
//! `--test-threads=1` because the analysis later tasks add decodes whole
//! halves, and two at once would fight over the decoder and ruin every timing
//! line.

use video_coach_harness::score::{
    score, Detection, ScoreReport, Tally, PERIOD_TOLERANCE, SEEK_LEAD, SEEK_WINDOW,
};
use video_coach_harness::truth::{folders, Truth};

#[test]
#[ignore = "needs the coach's tagged matches: see this file's module docs"]
fn ground_truth() {
    let var = std::env::var("COACH_GROUND_TRUTH").expect(
        "COACH_GROUND_TRUTH names the tagged project folders, `:`-separated, \
         tuning match first — see this test's module docs",
    );
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
         seek_lead={SEEK_LEAD:.1}s seek_window={SEEK_WINDOW:.1}s detector=none",
        tuning.name,
        held_out
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );

    // No detector exists yet (Tasks 3.3–3.5 build it), so every match is
    // scored against an empty detection set: precision undefined, recall 0.
    // What this run proves is the tooling — that every project reads, the tags
    // interpret, and the report comes out.
    let detected: Vec<Detection> = Vec::new();
    let mut aggregate = Tally::default();
    for (i, truth) in truths.iter().enumerate() {
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
