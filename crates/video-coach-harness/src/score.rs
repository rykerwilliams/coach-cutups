//! Grading detections against the coach's own tags (spec G3), and the report
//! the measurement phase reads.
//!
//! Pure: no I/O, no GStreamer, no footage. It lives in the harness rather than
//! in core because only the ground-truth test calls it — nothing the app ships
//! scores anything.
//!
//! The detection types mirror the spec's `SuggestionKind` but are **owned
//! here**, so P3 measures without a project-format change. P4 adds the stored
//! shape.

use std::fmt;

use video_coach_core::signals::{Cheer, Whistle};

use crate::truth::{TruthEvent, TruthKind};

/// How far a period detection may sit from its tag and still match (G3).
///
/// This is a bar on the **coach's** tag accuracy as much as the detector's:
/// `V` is pressed on a whistle that has to be heard first, so a systematic
/// reaction delay lands in this number rather than in the detector. Task 3.3
/// measures the signed offset from each of the twelve period tags to its
/// nearest long whistle before this value is trusted. If the tags sit
/// consistently further out, the tolerance is what is wrong — and widening it
/// goes back to the user rather than happening here, because the same ±10 s is
/// what makes a suggestion resolvable (D6).
pub const PERIOD_TOLERANCE: f64 = 10.0;

/// How far before its `at` a high-tier goal's seek point sits (D6).
pub const SEEK_LEAD: f64 = 10.0;

/// How long after the seek point the goal must arrive for the Seek bar (G4).
pub const SEEK_WINDOW: f64 = 20.0;

// ------------------------------------------------------------- detections

/// How much evidence a goal detection has (D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalTier {
    /// A cheer in `[window start, K − 15 s]`.
    High,
    /// A kick-off with no cheer behind it.
    Quiet,
}

impl fmt::Display for GoalTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GoalTier::High => "high",
            GoalTier::Quiet => "quiet",
        })
    }
}

/// What a detection claims to have found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DetectionKind {
    Goal {
        tier: GoalTier,
        /// `[start, end]` in source seconds: where the goal must be (D4).
        window: (f64, f64),
        /// The estimated instant, high tier only.
        at: Option<f64>,
    },
    PeriodStart,
    PeriodEnd,
}

/// One suggestion, positioned on one source.
///
/// `seconds` is `K` for a goal or a period start, and the whistle for a period
/// end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub source_index: usize,
    pub seconds: f64,
    pub kind: DetectionKind,
}

impl Detection {
    /// Where Seek jumps (D6): `at − 10 s` clamped to the window for a high-tier
    /// goal, the window start for a quiet one, and 10 s early for a period.
    pub fn seek(&self) -> f64 {
        match self.kind {
            DetectionKind::Goal {
                window,
                at: Some(at),
                ..
            } => (at - SEEK_LEAD).max(window.0),
            DetectionKind::Goal {
                window, at: None, ..
            } => window.0,
            DetectionKind::PeriodStart | DetectionKind::PeriodEnd => self.seconds - SEEK_LEAD,
        }
    }
}

// ----------------------------------------------------------------- counts

/// True positives, false positives and false negatives for one kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub tp: usize,
    pub fp: usize,
    /// Spelled out because `fn` is a keyword; printed as `fn=`.
    pub misses: usize,
}

impl Counts {
    /// `None` when nothing was detected — never `0.0`, which reads on a report
    /// line as a detector that got everything wrong rather than one that said
    /// nothing.
    pub fn precision(&self) -> Option<f64> {
        rate(self.tp, self.tp + self.fp)
    }

    /// `None` when there is nothing to find.
    pub fn recall(&self) -> Option<f64> {
        rate(self.tp, self.tp + self.misses)
    }

    fn add(&mut self, other: Counts) {
        self.tp += other.tp;
        self.fp += other.fp;
        self.misses += other.misses;
    }
}

fn rate(hits: usize, total: usize) -> Option<f64> {
    (total != 0).then(|| hits as f64 / total as f64)
}

/// A rate as the report prints it: two decimals, or `n/a` for an undefined
/// one. Never `0.00`, which reads as a detector that got everything wrong
/// rather than one that was never asked.
pub fn show_rate(rate: Option<f64>) -> String {
    rate.map_or_else(|| "n/a".to_string(), |r| format!("{r:.2}"))
}

/// Every count one run produces, per kind and per tier.
///
/// Separate from the diagnostics so that the held-out aggregate is a sum:
/// counts add across matches, and the `HIT` and `MISS` lines stay with the
/// match they came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub goals: Counts,
    pub goals_high: Counts,
    pub goals_quiet: Counts,
    pub period_start: Counts,
    pub period_end: Counts,
}

impl Tally {
    pub fn add(&mut self, other: &Tally) {
        self.goals.add(other.goals);
        self.goals_high.add(other.goals_high);
        self.goals_quiet.add(other.goals_quiet);
        self.period_start.add(other.period_start);
        self.period_end.add(other.period_end);
    }

    /// The five `SCORE` lines for one set. `set` is `held_out`, `tuning` or
    /// `per_match`; `match_name` is the `A`/`B`/`C` letter on a per-match row.
    pub fn print(&self, set: &str, match_name: Option<&str>) {
        let who = match_name.map_or(String::new(), |m| format!(" match={m}"));
        for (kind, tier, c) in [
            ("goal", Some("all"), self.goals),
            ("goal", Some("high"), self.goals_high),
            ("goal", Some("quiet"), self.goals_quiet),
            ("period_start", None, self.period_start),
            ("period_end", None, self.period_end),
        ] {
            let tier = tier.map_or(String::new(), |t| format!(" tier={t}"));
            println!(
                "SCORE  set={set}{who} kind={kind}{tier} tp={} fp={} fn={} p={} r={}",
                c.tp,
                c.fp,
                c.misses,
                show_rate(c.precision()),
                show_rate(c.recall()),
            );
        }
    }
}

// ----------------------------------------------------------- the report

/// A truth goal and the detection that found it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoalHit {
    pub source_index: usize,
    pub truth_seconds: f64,
    pub tier: GoalTier,
    /// How far into its window the goal sat, and how far before `K`. Both
    /// retune `W` without paying for another analysis run.
    pub from_window_start: f64,
    pub from_k: f64,
    /// Whether the goal lies in `[seek, seek + 20 s]` — G4's Seek bar, which
    /// only the high tier is judged on.
    pub seek_ok: Option<bool>,
}

/// A truth period event and the detection that found it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodHit {
    pub source_index: usize,
    pub kind: TruthKind,
    pub truth_seconds: f64,
    /// Detection − truth: positive means the detector was late.
    pub error: f64,
}

/// One run's grade for one match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoreReport {
    pub tally: Tally,
    pub goal_hits: Vec<GoalHit>,
    pub period_hits: Vec<PeriodHit>,
    pub missed_goals: Vec<TruthEvent>,
    pub missed_periods: Vec<TruthEvent>,
    pub false_goals: Vec<Detection>,
    pub false_periods: Vec<Detection>,
}

impl ScoreReport {
    /// G4's Seek bar: how many matched high-tier goals landed in the 20 s after
    /// their seek point, out of how many there were.
    pub fn seek(&self) -> (usize, usize) {
        let judged = self.goal_hits.iter().filter_map(|h| h.seek_ok);
        (judged.clone().filter(|ok| *ok).count(), judged.count())
    }

    /// The per-goal and per-period diagnostics, and one `MISS` line per truth
    /// goal nothing found.
    ///
    /// The stage columns on a `MISS` line are filled by the passes Tasks
    /// 3.3–3.5 add; until then they read `none`, and `restart` reads `unknown`
    /// unless a `kickoffs.txt` supplied one.
    pub fn print_diagnostics(&self, match_name: &str, truth: &[TruthEvent]) {
        for h in &self.goal_hits {
            println!(
                "HIT    match={match_name} src={} kind=goal tier={} goal={:.1} from_window_start={:.1} from_k={:.1} seek_ok={}",
                h.source_index,
                h.tier,
                h.truth_seconds,
                h.from_window_start,
                h.from_k,
                h.seek_ok.map_or("n/a".into(), |ok| ok.to_string()),
            );
        }
        for h in &self.period_hits {
            println!(
                "HIT    match={match_name} src={} kind={} at={:.1} error={:+.1}",
                h.source_index,
                kind_key(h.kind),
                h.truth_seconds,
                h.error,
            );
        }
        for m in &self.missed_goals {
            // The first restart at or after the goal, when `kickoffs.txt`
            // supplied one: the walk-back is what the kick-off rule is looking
            // for, so a miss with no restart in the notes is a different kind
            // of miss from one with a restart the rule failed to see.
            let restart = truth
                .iter()
                .find(|t| {
                    t.kind == TruthKind::Restart
                        && t.source_index == m.source_index
                        && t.seconds >= m.seconds
                })
                .map_or("unknown".to_string(), |t| format!("{:.1}", t.seconds));
            println!(
                "MISS   match={match_name} src={} goal={:.1} still=none whistle=none cheer=none restart={restart}",
                m.source_index, m.seconds,
            );
        }
        for m in &self.missed_periods {
            println!(
                "MISS   match={match_name} src={} {}={:.1} whistle=none",
                m.source_index,
                kind_key(m.kind),
                m.seconds,
            );
        }
    }
}

// ------------------------------------------------------- audio diagnostics

/// How near a truth goal a cheer's onset must be for the cheer to have covered
/// it.
///
/// Generous on purpose. Measured on one whole half, a goal's cheer starts
/// **0.4–1.4 s after** the frame the coach tags, so five seconds either side
/// is far wider than the signal needs; anything outside it is the detector
/// missing the goal, not the coach tagging it late. The number that decides
/// whether the cheer cue is viable at all is a recall, and a recall read off a
/// tolerance that is argued about is worth nothing.
pub const CHEER_TOLERANCE: f64 = 5.0;

/// The item whose time is nearest `at`, with the signed offset to it —
/// positive when the signal is **later** than the tag.
pub fn nearest<'a, T: 'a>(
    items: impl IntoIterator<Item = &'a T>,
    at: f64,
    time: impl Fn(&T) -> f64,
) -> Option<(&'a T, f64)> {
    items
        .into_iter()
        .map(|item| {
            let offset = time(item) - at;
            (item, offset)
        })
        .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
}

/// The `DIAG` lines (Task 3.3): for every tag on one source, the signal that
/// should have found it and how far away it sat.
///
/// **No detection is involved.** These say whether the cue exists in the
/// sound at all, which is a different question from whether the rules built on
/// it fire — and it is the one that decides whether there is any point in
/// building the rules. A period tag's offset is also the only honest way to
/// read [`PERIOD_TOLERANCE`]: if the twelve tags sit tens of seconds from
/// their nearest long whistle, that ±10 s is measuring the coach's reaction.
pub fn print_audio_diagnostics(
    match_name: &str,
    source_index: usize,
    truth: &[TruthEvent],
    whistles: &[Whistle],
    cheers: &[Cheer],
) {
    let long: Vec<&Whistle> = whistles.iter().filter(|w| w.is_long()).collect();
    for tag in truth
        .iter()
        .filter(|t| t.source_index == source_index && t.kind != TruthKind::Restart)
    {
        let head = format!(
            "DIAG   match={match_name} src={source_index} kind={} tag={:.1}",
            kind_key(tag.kind),
            tag.seconds,
        );
        match tag.kind {
            TruthKind::Goal => match nearest(cheers, tag.seconds, |c| c.onset) {
                Some((cheer, offset)) => println!(
                    "{head} cheer={offset:+.1} dur={:.1} peak={:+.1}",
                    cheer.duration, cheer.peak_db
                ),
                None => println!("{head} cheer=none"),
            },
            // The long whistle is what D4 says a period ends on, so it is
            // reported first; when the half has none — and measured, no half
            // in the design footage has one at the initial constants — the
            // nearest whistle of **any** length is reported instead, marked
            // `long=false`. "None at all" and "none long enough" are different
            // failures, and only one of them is a threshold.
            _ => match nearest(long.iter().copied(), tag.seconds, |w| w.start)
                .or_else(|| nearest(whistles, tag.seconds, |w| w.start))
            {
                Some((whistle, offset)) => println!(
                    "{head} whistle={offset:+.1} long={} dur={:.2} snr={:.1} tonality={:.1}",
                    whistle.is_long(),
                    whistle.duration,
                    whistle.snr_db,
                    whistle.tonality_db
                ),
                None => println!("{head} whistle=none"),
            },
        }
    }
}

/// How many goals on one source have a cheer within [`CHEER_TOLERANCE`], out
/// of how many there are.
pub fn cheer_coverage(
    truth: &[TruthEvent],
    source_index: usize,
    cheers: &[Cheer],
) -> (usize, usize) {
    let goals = truth
        .iter()
        .filter(|t| t.source_index == source_index && t.kind == TruthKind::Goal);
    let mut covered = 0;
    let mut total = 0;
    for goal in goals {
        total += 1;
        if nearest(cheers, goal.seconds, |c| c.onset)
            .is_some_and(|(_, offset)| offset.abs() <= CHEER_TOLERANCE)
        {
            covered += 1;
        }
    }
    (covered, total)
}

fn kind_key(kind: TruthKind) -> &'static str {
    match kind {
        TruthKind::Goal => "goal",
        TruthKind::PeriodStart => "period_start",
        TruthKind::PeriodEnd => "period_end",
        TruthKind::Restart => "restart",
    }
}

// ---------------------------------------------------------------- scoring

/// Grade `found` against `truth` for one match.
///
/// Pairing is **one-to-one**. A goal detection takes the earliest unmatched
/// truth goal inside its window, so a window holding two truth goals scores one
/// true positive and one false negative rather than two true positives — which
/// is the mistake that silently inflates recall. Period events pair greedily by
/// nearest time within [`PERIOD_TOLERANCE`], each side used once, ties broken
/// by the earlier detection.
///
/// `TruthKind::Restart` is a diagnostic, never a thing to detect, so it is not
/// scored.
pub fn score(truth: &[TruthEvent], found: &[Detection]) -> ScoreReport {
    let mut report = ScoreReport::default();

    score_goals(truth, found, &mut report);
    for (kind, detected) in [
        (TruthKind::PeriodStart, DetectionKind::PeriodStart),
        (TruthKind::PeriodEnd, DetectionKind::PeriodEnd),
    ] {
        let counts = score_periods(truth, found, kind, detected, &mut report);
        match kind {
            TruthKind::PeriodStart => report.tally.period_start = counts,
            _ => report.tally.period_end = counts,
        }
    }
    // Chronological, so two runs diff line by line: the pairing walks nearest
    // pair first, which is not an order anyone reads a match in.
    report.period_hits.sort_by(|a, b| {
        a.source_index
            .cmp(&b.source_index)
            .then_with(|| a.truth_seconds.total_cmp(&b.truth_seconds))
    });
    report.missed_periods.sort_by(|a, b| {
        a.source_index
            .cmp(&b.source_index)
            .then_with(|| a.seconds.total_cmp(&b.seconds))
    });
    report
}

fn score_goals(truth: &[TruthEvent], found: &[Detection], report: &mut ScoreReport) {
    let goals: Vec<&TruthEvent> = truth.iter().filter(|t| t.kind == TruthKind::Goal).collect();
    let mut taken = vec![false; goals.len()];

    // Earliest window first, so that when two windows do overlap — they cannot
    // within one run (D4), but a hand-built set or a future rule change could
    // — the pairing is deterministic.
    let mut detections: Vec<&Detection> = found
        .iter()
        .filter(|d| matches!(d.kind, DetectionKind::Goal { .. }))
        .collect();
    detections.sort_by(|a, b| window_of(a).0.total_cmp(&window_of(b).0));

    for d in detections {
        let (start, end) = window_of(d);
        let DetectionKind::Goal { tier, at, .. } = d.kind else {
            unreachable!("filtered to goals above")
        };
        let hit = goals.iter().enumerate().find(|(i, t)| {
            !taken[*i] && t.source_index == d.source_index && t.seconds >= start && t.seconds <= end
        });
        match hit {
            Some((i, t)) => {
                taken[i] = true;
                let seek = d.seek();
                report.goal_hits.push(GoalHit {
                    source_index: t.source_index,
                    truth_seconds: t.seconds,
                    tier,
                    from_window_start: t.seconds - start,
                    from_k: d.seconds - t.seconds,
                    seek_ok: (tier == GoalTier::High && at.is_some())
                        .then_some(t.seconds >= seek && t.seconds <= seek + SEEK_WINDOW),
                });
                counts_for(&mut report.tally, tier).tp += 1;
                report.tally.goals.tp += 1;
            }
            None => {
                report.false_goals.push(*d);
                counts_for(&mut report.tally, tier).fp += 1;
                report.tally.goals.fp += 1;
            }
        }
    }

    for (i, t) in goals.iter().enumerate() {
        if !taken[i] {
            report.missed_goals.push(**t);
        }
    }
    // A tier's false negatives are every truth goal that tier did not find, so
    // its recall answers "how much of the match would this tier alone cover?".
    report.tally.goals.misses = report.missed_goals.len();
    report.tally.goals_high.misses = goals.len() - report.tally.goals_high.tp;
    report.tally.goals_quiet.misses = goals.len() - report.tally.goals_quiet.tp;
}

fn counts_for(tally: &mut Tally, tier: GoalTier) -> &mut Counts {
    match tier {
        GoalTier::High => &mut tally.goals_high,
        GoalTier::Quiet => &mut tally.goals_quiet,
    }
}

fn window_of(d: &Detection) -> (f64, f64) {
    match d.kind {
        DetectionKind::Goal { window, .. } => window,
        _ => (d.seconds, d.seconds),
    }
}

fn score_periods(
    truth: &[TruthEvent],
    found: &[Detection],
    kind: TruthKind,
    detected: DetectionKind,
    report: &mut ScoreReport,
) -> Counts {
    let tags: Vec<&TruthEvent> = truth.iter().filter(|t| t.kind == kind).collect();
    let detections: Vec<&Detection> = found.iter().filter(|d| d.kind == detected).collect();

    // Every pair within tolerance, nearest first; ties go to the earlier
    // detection, then to the earlier tag, so the pairing is total and stable.
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (di, d) in detections.iter().enumerate() {
        for (ti, t) in tags.iter().enumerate() {
            if t.source_index != d.source_index {
                continue;
            }
            let error = d.seconds - t.seconds;
            if error.abs() <= PERIOD_TOLERANCE {
                pairs.push((error.abs(), di, ti));
            }
        }
    }
    pairs.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| detections[a.1].seconds.total_cmp(&detections[b.1].seconds))
            .then(a.2.cmp(&b.2))
    });

    let mut used_d = vec![false; detections.len()];
    let mut used_t = vec![false; tags.len()];
    let mut counts = Counts::default();
    for (_, di, ti) in pairs {
        if used_d[di] || used_t[ti] {
            continue;
        }
        used_d[di] = true;
        used_t[ti] = true;
        counts.tp += 1;
        report.period_hits.push(PeriodHit {
            source_index: tags[ti].source_index,
            kind,
            truth_seconds: tags[ti].seconds,
            error: detections[di].seconds - tags[ti].seconds,
        });
    }
    for (di, d) in detections.iter().enumerate() {
        if !used_d[di] {
            counts.fp += 1;
            report.false_periods.push(**d);
        }
    }
    for (ti, t) in tags.iter().enumerate() {
        if !used_t[ti] {
            counts.misses += 1;
            report.missed_periods.push(**t);
        }
    }
    counts
}
