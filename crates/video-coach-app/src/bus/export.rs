//! Export (Phase 5 spec X4, Phase 8 specs E1, E5 and E6): a **run** of
//! targets, rendered one at a time by an [`Exporter`] on its own thread while
//! the bus goes on.
//!
//! **One export path.** All clips, one tag's clips, a single clip and the
//! goals reel are all [`ExportTarget`]s, each rendered as a compilation: one
//! plan, one schedule, one progress model, one cancel.
//!
//! **Everything is refused up front, naming the clip** (or, for the reel, the
//! game video's file). A missing game video or commentary recording fails the
//! whole run before a frame is rendered, rather than an hour into one. Media
//! itself only warns about either and degrades to a black inset or to silence,
//! so this check is what makes the loss visible at all.
//!
//! **Progress is frames, and the whole run travels in every event.** The
//! sheet renders the run it is handed, so it can't be left holding a state the
//! bus has moved past, and the remaining frames of the pending targets are
//! there to divide by the rate (spec E5).
//!
//! The exporter's messages arrive as their own input. It sends exactly one
//! `Finished`, last, and there is one exporter at a time on a FIFO channel,
//! so no message can be stale: the thread's own result decides the outcome,
//! and a cancel that loses the race to a finished file reports it done.
//!
//! Recording and export never overlap (a user decision): the recording guard
//! drops [`Command::Export`](super::Command::Export), and `can_record`
//! refuses to record while a run is going.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

use gstreamer::glib;
use uuid::Uuid;
use video_coach_core::audio::audio_regions;
use video_coach_core::cues::{scoreboard_cues, Cue};
use video_coach_core::export::{compilation_schedule, Compilation, RateWindow, OUTPUT_FPS};
use video_coach_core::metadata::{
    clip_label, file_tags, reel_label, CalendarDate, ALL_CLIPS_LABEL, WHOLE_MATCH_LABEL,
};
use video_coach_core::plan::{
    compilation_plan, default_scoreboard_mode, ExportTarget, PlanEntry, ScoreboardMode,
};
use video_coach_core::project::{Preferences, Project, Quality, Resolution};
use video_coach_core::reel::{reel_goals, ReelSide};
use video_coach_core::scoreboard::ScoreboardContext;
use video_coach_core::store::{EXPORTS_DIRNAME, RECORDINGS_DIRNAME};
use video_coach_core::tag::tag_summaries;
use video_coach_media::{
    ClipMedia, Encode, EntryMedia, ExportDone, ExportError, ExportJob, ExportMessage, Exporter,
    MatchMedia, Render,
};

use super::{Bus, Event, Input, Open, UserError};

/// Where one target of a run has got to.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetState {
    /// Waiting its turn. Nothing of it has been rendered.
    Pending,
    /// Rendering, with the output frames pushed so far.
    Running(usize),
    /// Written, at this path.
    Done(PathBuf),
    /// Gave up, with no file and any file already at its path untouched. The
    /// run goes on to the next target.
    Failed(String),
    /// Cancelled part-way, or never started.
    Cancelled,
}

/// One target of a run, as the sheet lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportTargetRun {
    /// What the sheet calls it, and what names its file (spec E6). Unique
    /// within the run — see [`de_duplicate`].
    pub label: String,
    /// The target's output frames — the denominator, and the only measure of
    /// its length (see `CompilationPlan::total_frames`).
    pub frames: usize,
    pub state: TargetState,
}

/// An export run, as the UI shows it: every target in order, plus the rate
/// the estimate is built on.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportRun {
    pub targets: Vec<ExportTargetRun>,
    /// Output frames per wall second over a trailing window, once it is
    /// steady (spec E5); `None` until then, and the sheet shows no estimate
    /// at all. It is measured over the **run**, so it carries across the gap
    /// between two targets.
    pub rate: Option<f64>,
}

impl ExportRun {
    /// Whether anything is still to render. The last event of a run has this
    /// `false`, which is how the UI knows the run is over.
    pub fn is_running(&self) -> bool {
        self.targets
            .iter()
            .any(|t| matches!(t.state, TargetState::Pending | TargetState::Running(_)))
    }

    /// Output frames still to render, across every target that hasn't
    /// finished. Time left is this divided by [`ExportRun::rate`].
    pub fn remaining_frames(&self) -> usize {
        self.targets
            .iter()
            .map(|t| match t.state {
                TargetState::Pending => t.frames,
                TargetState::Running(done) => t.frames.saturating_sub(done),
                _ => 0,
            })
            .sum()
    }
}

/// One row of the export sheet's target list (spec E8).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportTargetRow {
    pub target: ExportTarget,
    /// What the sheet calls it, and what names its file (spec E6).
    pub label: String,
    /// What its row counts: the clips it covers, the reel's goals — which are
    /// not its entries, since two goals close together share one — or the
    /// whole match's source videos.
    pub count: usize,
    /// What [`ExportTargetRow::count`] counts, singular: `"clip"`, `"goal"`
    /// or `"video"`.
    pub unit: &'static str,
    /// How long its output runs, from its frame count
    /// (`CompilationPlan::total_frames`).
    pub seconds: f64,
}

/// The sheet's targets: Whole match, All clips, one row per tag, the goals
/// reels, then `selected` if a clip is (spec E8, match vision specs R1, R1b
/// and W1).
///
/// A target with no plan entry is left out, since there is nothing to export
/// in it — which is also what keeps an empty project's sheet empty, and a
/// side's reel away until that side has scored.
pub fn export_targets(project: &Project, selected: Option<Uuid>) -> Vec<ExportTargetRow> {
    let row = |target: ExportTarget, label: String| {
        let plan = compilation_plan(project, &target);
        let (count, unit) = match target {
            ExportTarget::Reel(side) => (reel_goals(project, side).len(), "goal"),
            // One entry per source video, so the row reads "2 videos ·
            // 54:12" — the honest warning that this is the longest render
            // the app can be asked for (spec W4).
            ExportTarget::WholeMatch => (plan.entries.len(), "video"),
            _ => (plan.entries.len(), "clip"),
        };
        (!plan.entries.is_empty()).then(|| ExportTargetRow {
            target,
            label,
            count,
            unit,
            seconds: plan.total_frames() as f64 / f64::from(OUTPUT_FPS),
        })
    };
    let mut rows: Vec<ExportTargetRow> = row(ExportTarget::WholeMatch, WHOLE_MATCH_LABEL.into())
        .into_iter()
        .chain(row(ExportTarget::AllClips, ALL_CLIPS_LABEL.into()))
        .collect();
    for tag in tag_summaries(&project.clips) {
        rows.extend(row(ExportTarget::Tag(tag.tag.clone()), tag.tag));
    }
    // A reel per side that scored, and the whole reel only when both have:
    // with one side scoring it would be the same film twice (spec R1b).
    let scored = |side| !reel_goals(project, side).is_empty();
    if scored(ReelSide::Home) && scored(ReelSide::Away) {
        rows.extend(row(
            ExportTarget::Reel(ReelSide::All),
            reel_label(project, ReelSide::All),
        ));
    }
    for side in [ReelSide::Home, ReelSide::Away] {
        rows.extend(row(ExportTarget::Reel(side), reel_label(project, side)));
    }
    if let Some(clip) = selected.and_then(|id| project.clips.iter().find(|c| c.id == id)) {
        rows.extend(row(
            ExportTarget::Clip(clip.id),
            clip_label(clip).to_owned(),
        ));
    }
    rows
}

/// `<label> - <project>.mp4` (spec E6), with the two characters a file name
/// can't safely hold replaced: `/`, which is the path separator, and `:`,
/// which a share to a Mac or a Windows machine trips over.
fn file_name(label: &str, project_name: &str) -> String {
    let clean = |part: &str| part.replace(['/', ':'], "-");
    format!("{} - {}.mp4", clean(label), clean(project_name))
}

/// The run in progress: the jobs still to render, and what the UI was told.
pub(super) struct Active {
    /// The targets after the one rendering, in order.
    jobs: VecDeque<ExportJob>,
    /// Which of `run.targets` is rendering.
    index: usize,
    run: ExportRun,
    /// The exporter rendering `run.targets[index]`. Dropping it cancels and
    /// joins, which is how a shutdown stops a run.
    exporter: Exporter,
    /// When the run started, and how many frames the targets before this one
    /// rendered: the rate is the run's, not the target's.
    started: Instant,
    done_frames: usize,
    rate: RateWindow,
    /// When the current target started, for the `bus: exported …` log.
    target_started: Instant,
    /// A cancel has been asked for. The target rendering when it landed still
    /// reports its own outcome — one that had already finished keeps its file
    /// (spec E5) — and the targets after it are never started.
    cancelled: bool,
}

impl Active {
    /// Records how the target that just stopped ended, and the frames it
    /// really rendered: a failure half-way must not spike the rate with the
    /// frames it never got to.
    fn finish_target(&mut self, result: Result<ExportDone, ExportError>) {
        let target = &mut self.run.targets[self.index];
        let rendered = match target.state {
            TargetState::Running(done) => done,
            _ => 0,
        };
        target.state = match result {
            Ok(done) => {
                let d = &done.diagnostics;
                let seconds = self.target_started.elapsed().as_secs_f64();
                eprintln!(
                    "bus: exported {}: {} frames in {seconds:.1} s ({:.1} fps), \
                     decoder {:?}, glupload caps {:?}, encoder {}, chapters {:?}, \
                     sidecar {:?}, chapter list {:?}, moov reserve left {:.1} s",
                    done.path.display(),
                    target.frames,
                    target.frames as f64 / seconds,
                    d.decoder,
                    d.glupload_caps,
                    done.encoder,
                    done.chapters,
                    done.sidecar,
                    done.chapter_list,
                    done.reserve_remaining
                );
                TargetState::Done(done.path)
            }
            Err(ExportError::Cancelled) => TargetState::Cancelled,
            Err(ExportError::Failed(e)) => {
                eprintln!("bus: export failed: {e}");
                TargetState::Failed(e)
            }
        };
        self.done_frames += match target.state {
            TargetState::Done(_) => target.frames,
            _ => rendered,
        };
        // The next target starts its own measurement (spec X3): a copy runs
        // at thousands of output frames a wall second against an encode's
        // tens, and the targets queued behind it must not inherit that rate
        // and be promised they finish at once. The window needs a span before
        // it answers again, so the gap is silent rather than wrong — and the
        // event this finish emits must be silent too, or the sheet keeps the
        // copy's rate until the next target's first progress lands.
        self.rate = RateWindow::default();
        self.run.rate = None;
    }

    /// The next target's job, or `None` once the run is over.
    ///
    /// A cancel ends it here: the targets already written stay written, and
    /// the ones never started are marked cancelled rather than run.
    fn next_job(&mut self) -> Option<ExportJob> {
        self.index += 1;
        let job = match self.cancelled {
            true => None,
            false => self.jobs.pop_front(),
        };
        if job.is_none() {
            if let Some(rest) = self.run.targets.get_mut(self.index..) {
                for target in rest {
                    target.state = TargetState::Cancelled;
                }
            }
        }
        job
    }
}

impl Bus {
    /// Starts a run over `targets`, or says why it can't.
    pub(super) fn export(
        &mut self,
        targets: Vec<ExportTarget>,
        resolution: Resolution,
        quality: Quality,
        scoreboard: Option<ScoreboardMode>,
    ) {
        let pickers = Pickers {
            resolution,
            quality,
            scoreboard,
        };
        if let Err(e) = self.start_run(targets, pickers) {
            self.emit(Event::Error(e));
        }
    }

    fn start_run(&mut self, targets: Vec<ExportTarget>, pickers: Pickers) -> Result<(), UserError> {
        let refused = |why: &str| UserError::CantExport(why.into());
        // Before any I/O: resolving the targets stats a file per entry, and
        // there is no reason to do that to hit a field check (basket spec C3).
        self.refuse_if_busy()?;
        let Some(open) = &self.open else {
            return Err(refused("no project is open"));
        };
        if targets.is_empty() {
            return Err(refused("nothing is ticked"));
        }

        // Every target is checked before any of them runs, so a missing file
        // can't stop a run half-way through (spec E5).
        let exports = open.folder.join(EXPORTS_DIRNAME);
        let mut labels = Vec::with_capacity(targets.len());
        for target in &targets {
            labels.push(label(open, target)?);
        }
        de_duplicate(&mut labels);
        let mut jobs = Vec::with_capacity(targets.len());
        for (target, label) in targets.iter().zip(labels) {
            let job = job(open, &exports, target, &label, pickers)?;
            jobs.push((label, job));
        }
        // On demand, so a project that has never been exported has no empty
        // folder (spec E6). After the refusals: a run that can't start
        // shouldn't leave one behind either.
        std::fs::create_dir_all(&exports).map_err(|e| {
            UserError::CantExport(format!("could not create {}: {e}", exports.display()))
        })?;
        self.begin(jobs);

        // The sheet's pickers are the project's from here on (spec E4). After
        // the run began, so nothing above this can have dirtied the project on
        // its way to a refusal (basket spec C3).
        if let Some(open) = &mut self.open {
            let prefs = &mut open.project.preferences;
            if Pickers::of(prefs) != pickers {
                prefs.last_export_resolution = pickers.resolution;
                prefs.last_export_quality = pickers.quality;
                prefs.last_export_scoreboard = pickers.scoreboard;
                self.project_changed();
            }
        }
        Ok(())
    }

    /// The two refusals that cost nothing to check, so both job builders make
    /// them **first**, before they read a project (basket spec C3). They are
    /// also the whole of what [`Bus::begin`] would have to refuse, which is why
    /// it refuses nothing.
    pub(super) fn refuse_if_busy(&self) -> Result<(), UserError> {
        let refused = |why: &str| UserError::CantExport(why.into());
        if self.export.is_some() {
            return Err(refused("an export is running"));
        }
        // Both composite on the UI's GL context, and an export would take the
        // frames the preview is pacing itself on (spec P5).
        if self.preview.is_some() {
            return Err(refused("a preview is open; close it first"));
        }
        Ok(())
    }

    /// Begins a run over `jobs`, each with the label the sheet lists it under,
    /// and publishes it (basket spec C3).
    ///
    /// **Infallible, because the caller has already refused everything there is
    /// to refuse**: [`Bus::refuse_if_busy`] first, then a job per target — and
    /// an empty list of targets is itself one of those refusals (`"nothing is
    /// ticked"` for an export, `"the basket is empty"` for a basket). So a run
    /// that reaches here starts, and neither caller has to unwind work it did
    /// on the way.
    ///
    /// **The caller creates its own output directory**, after its own refusals
    /// and before this: an export the project's `exports/`, a basket its one
    /// folder. A directory is the last thing either does before starting, so a
    /// run that can't start leaves none behind.
    pub(super) fn begin(&mut self, jobs: Vec<(String, ExportJob)>) {
        let mut rows: Vec<ExportTargetRun> = jobs
            .iter()
            .map(|(label, job)| ExportTargetRun {
                label: label.clone(),
                frames: job.compilation.frames.len(),
                state: TargetState::Pending,
            })
            .collect();
        let mut jobs: VecDeque<ExportJob> = jobs.into_iter().map(|(_, job)| job).collect();

        let Some(first) = jobs.pop_front() else {
            // A builder that got this far with nothing is a bug in it, not
            // something the coach did: both refuse an empty list of targets
            // before they build a single job.
            return eprintln!("bus: a run was begun with no jobs");
        };
        rows[0].state = TargetState::Running(0);
        let now = Instant::now();
        self.export = Some(Active {
            exporter: self.start(first),
            jobs,
            index: 0,
            run: ExportRun {
                targets: rows,
                rate: None,
            },
            started: now,
            done_frames: 0,
            rate: RateWindow::default(),
            target_started: now,
            cancelled: false,
        });
        let run = self.export.as_ref().expect("just set").run.clone();
        self.emit(Event::Export(run));
    }

    /// Renders `job` on a thread of its own, forwarding its messages to the
    /// bus's own input.
    fn start(&self, job: ExportJob) -> Exporter {
        let tx = self.tx.clone();
        Exporter::start(job, move |msg| {
            // Fails only once the bus thread has exited.
            let _ = tx.send(Input::Export(msg));
        })
    }

    /// Asks the run, if one is going, to stop. The target rendering stops
    /// after its current frame and loses its `.part`; the targets already
    /// written are left alone, and the ones not started never run (spec E5).
    pub(super) fn cancel_export(&mut self) {
        if let Some(active) = &mut self.export {
            active.cancelled = true;
            active.exporter.cancel();
        }
    }

    pub(super) fn export_message(&mut self, msg: ExportMessage) {
        // Out of `self` for the length of this: starting the next target
        // needs the bus's own sender. It goes back below unless the run is
        // over.
        let Some(mut active) = self.export.take() else {
            return;
        };
        let over = match msg {
            ExportMessage::Progress(frames) => {
                active.run.targets[active.index].state = TargetState::Running(frames);
                let elapsed = active.started.elapsed().as_secs_f64();
                active.run.rate = active.rate.sample(active.done_frames + frames, elapsed);
                false
            }
            ExportMessage::Finished(result) => {
                // Joins the thread, which has nothing left to do.
                active.finish_target(result);
                match active.next_job() {
                    Some(job) => {
                        active.exporter = self.start(job);
                        active.target_started = Instant::now();
                        active.run.targets[active.index].state = TargetState::Running(0);
                        false
                    }
                    None => true,
                }
            }
        };
        self.emit(Event::Export(active.run.clone()));
        // The run being over is what lets a queued transcript have the
        // machine back; `Bus::run`'s tail picks that up (Phase 10 spec S5).
        if !over {
            self.export = Some(active);
        }
    }
}

/// What `target` is called, or why it can't run.
fn label(open: &Open, target: &ExportTarget) -> Result<String, UserError> {
    match target {
        ExportTarget::AllClips => Ok(ALL_CLIPS_LABEL.to_owned()),
        ExportTarget::Tag(tag) => Ok(tag.clone()),
        ExportTarget::Clip(id) => open
            .project
            .clips
            .iter()
            .find(|c| c.id == *id)
            .map(|clip| clip_label(clip).to_owned())
            .ok_or_else(|| UserError::CantExport("the clip is gone".into())),
        ExportTarget::Reel(side) => Ok(reel_label(&open.project, *side)),
        ExportTarget::WholeMatch => Ok(WHOLE_MATCH_LABEL.to_owned()),
    }
}

/// Makes every label in a run unique, suffixing the later of a pair.
///
/// The label names the file (spec E6), and nothing stops a clip being called
/// what a tag is called — ticking both would otherwise have the second target
/// overwrite the first's file half-way through the run.
fn de_duplicate(labels: &mut [String]) {
    let mut seen: HashSet<String> = HashSet::new();
    for label in labels {
        if seen.insert(label.clone()) {
            continue;
        }
        let mut n = 2;
        while !seen.insert(format!("{label} ({n})")) {
            n += 1;
        }
        *label = format!("{label} ({n})");
    }
}

/// The export sheet's three pickers, which travel together: through the run
/// into every job, and into the project's `Preferences` when it starts (spec
/// E4, M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pickers {
    resolution: Resolution,
    quality: Quality,
    /// `None` is the sheet's "Default": the target's own mode.
    scoreboard: Option<ScoreboardMode>,
}

impl Pickers {
    /// What `prefs` last remembered, for the "only when it changed" write-back
    /// that keeps opening the sheet from dirtying the project.
    fn of(prefs: &Preferences) -> Self {
        Pickers {
            resolution: prefs.last_export_resolution,
            quality: prefs.last_export_quality,
            scoreboard: prefs.last_export_scoreboard,
        }
    }
}

/// What the sheet's Scoreboard picker means for one target (spec M3).
struct Carry {
    /// Copy the sources' packets rather than re-encode them.
    copy: bool,
    /// The scoreboard beside the file: `None` for a target that carries no
    /// sidecar at all, so nothing at that path is written **or removed**.
    cues: Option<Vec<Cue>>,
    /// The board to burn into the picture, or `None`. Media reads this in one
    /// place, the per-frame overlay state, so `None` **is** "don't draw it" —
    /// there is no mode flag to carry into media at all.
    scoreboard: Option<ScoreboardContext>,
}

/// The whole of the mapping: the picker (or, for "Default", the target's own
/// mode) into the renderer and the two job fields that carry the board.
///
/// `context` is the run's frozen [`ScoreboardContext`], `None` for a project
/// with no scoreboard set up — which means no cues either, since there is
/// nothing to derive them from (spec E5). `files` is what a copy would join,
/// one per plan entry and in that order.
fn carry_scoreboard(
    target: &ExportTarget,
    picked: Option<ScoreboardMode>,
    compilation: &Compilation,
    context: Option<ScoreboardContext>,
    files: &[PathBuf],
) -> Carry {
    // The board burned into the picture, which is what every target but a
    // copied whole match does with it.
    let burned = |context| Carry {
        copy: false,
        cues: Some(Vec::new()),
        scoreboard: context,
    };
    // **Only the whole match can carry the board beside the file** (spec T1):
    // a clip or a reel is drawn on, zoomed and captioned, so it re-encodes
    // either way, and a subtitle line repeating its own text bar would be
    // clutter. Asking for a separate track therefore burns it in rather than
    // dropping it — the picker must never lose the board. And its cue slot is
    // `None`: a `.srt` beside a clip is the coach's own file, and no export of
    // ours put it there to remove.
    if !matches!(target, ExportTarget::WholeMatch) {
        return Carry {
            cues: None,
            ..burned(context)
        };
    }
    match picked.unwrap_or_else(|| default_scoreboard_mode(target)) {
        ScoreboardMode::Burned => burned(context),
        // **"Default" means the best available.** A project whose videos
        // can't be joined — Matroska, HEVC, two halves recorded differently —
        // is re-encoded with the board burned in, exactly as it was before
        // this path existed, rather than refused at a gate the coach never
        // asked to be held to. Asking for the track by hand still refuses,
        // in media, naming the file and the way out (spec L6): there the
        // coach chose the copy, and quietly spending an hour instead would be
        // the worst available answer.
        ScoreboardMode::Track => {
            if picked.is_none() {
                if let Err(why) = video_coach_media::can_copy(files) {
                    eprintln!(
                        "bus: the whole match can't be copied ({why}), \
                         so it is re-encoded with the scoreboard burned in"
                    );
                    return burned(context);
                }
            }
            Carry {
                copy: true,
                cues: Some(match &context {
                    Some(context) => scoreboard_cues(compilation, context),
                    None => Vec::new(),
                }),
                scoreboard: None,
            }
        }
    }
}

/// The day to tag an export with: the first game video's own modification
/// time, read here because the bus is what knows the paths and media has no
/// business stat-ing files.
///
/// **The footage's date, not the export's** (a user decision): this coach's
/// game videos are downloads, so the file's mtime is within a day of the
/// match, which is the date anyone reading the file wants. `None` — no
/// sources, an unreadable file, a time before 1970 — writes no date at all
/// rather than today's.
///
/// The day a timestamp falls on is the reader's own, so it is resolved in the
/// local zone here; `video_coach_core` has no clock and takes the answer.
fn source_date(sources: &[PathBuf]) -> Option<CalendarDate> {
    let modified = std::fs::metadata(sources.first()?).ok()?.modified().ok()?;
    let seconds = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let local = glib::DateTime::from_unix_local(i64::try_from(seconds).ok()?).ok()?;
    Some(CalendarDate {
        year: local.year(),
        month: u32::try_from(local.month()).ok()?,
        day: u32::try_from(local.day_of_month()).ok()?,
    })
}

/// One entry's game video and, when it plays a clip, that clip's own media —
/// or why it can't run. **The one place a missing game video or commentary
/// recording is refused** (spec E5, basket spec V1), for an export of the open
/// project and for a basket piece alike.
///
/// `whose` prefixes the refusal: empty for an export, `"Rovers v Athletic — "`
/// for a basket piece, where two projects can hold clips with the same name.
/// The alternative — a second resolver writing the same three sentences again —
/// is how the two drift apart on the first edit to either.
///
/// The match's record is **not** joined here: [`job`] only knows whether the
/// board is burned in once it has this list (see [`carry_scoreboard`]), so each
/// builder wraps these in its own [`EntryMedia`].
///
/// The files are **stat**ed rather than read off `Bus::missing`, which is one
/// flag per source of the *open* project and says nothing about the closed
/// projects a basket reaches into (basket spec V2). An ordinary export is
/// marginally more current for it: a video deleted since the last refresh is
/// caught here.
pub(super) fn entry_media(
    folder: &Path,
    project: &Project,
    entry: &PlanEntry,
    whose: &str,
) -> Result<(PathBuf, Option<ClipMedia>), UserError> {
    let refused = |why: String| UserError::CantExport(why);
    let clip = entry.clip_id.map(|id| {
        project
            .clips
            .iter()
            .find(|c| c.id == id)
            .expect("the plan's clips are the project's clips")
    });
    // The game video the entry reads. A source the project doesn't have and
    // one whose file isn't there are the same refusal, and neither can reach
    // the job.
    let source = project
        .source_videos
        .get(entry.source_index)
        .map(|s| folder.join(&s.relative_path))
        .filter(|path| path.exists());
    let Some(source) = source else {
        let what = match clip {
            Some(clip) => format!("{}'s game video", clip_label(clip)),
            // A reel or whole-match entry has no clip to name: the file names
            // itself.
            None => {
                let file = project
                    .source_videos
                    .get(entry.source_index)
                    .map_or("a video", |s| s.display_name.as_str());
                format!("{file} (the game video)")
            }
        };
        return Err(refused(format!(
            "{whose}{what} is missing; relink it first"
        )));
    };
    let Some(clip) = clip else {
        return Ok((source, None));
    };
    let name = clip_label(clip);
    let recording = folder
        .join(RECORDINGS_DIRNAME)
        .join(&clip.recording_filename);
    if !recording.exists() {
        return Err(refused(format!(
            "{whose}{name}'s commentary recording is missing"
        )));
    }
    Ok((
        source,
        Some(ClipMedia {
            recording,
            clip: clip.clone(),
        }),
    ))
}

/// The job that renders `target` as `label`, or why it can't run.
///
/// A snapshot: later edits to the project don't reach a running export. Each
/// entry carries its own game video, its own clip and — shared by `Arc` with
/// every other entry of the same project — the match's board, highlights and
/// avatar, so nothing in media has to resolve a project-local index.
fn job(
    open: &Open,
    exports: &Path,
    target: &ExportTarget,
    label: &str,
    pickers: Pickers,
) -> Result<ExportJob, UserError> {
    let refused = |why: String| UserError::CantExport(why);
    let compilation = compilation_schedule(&open.project, target);
    if compilation.frames.is_empty() {
        return Err(refused(format!("{label} has nothing to export")));
    }

    let sources: Vec<PathBuf> = open
        .project
        .source_videos
        .iter()
        .map(|s| open.folder.join(&s.relative_path))
        .collect();
    // Each entry's game video and its clip's own media. The match's record
    // joins them below rather than here, because the Scoreboard picker decides
    // whether the board is burned in or carried beside the file, and it reads
    // this list to answer.
    let mut pieces: Vec<(PathBuf, Option<ClipMedia>)> =
        Vec::with_capacity(compilation.plan.entries.len());
    for entry in &compilation.plan.entries {
        pieces.push(entry_media(&open.folder, &open.project, entry, "")?);
    }

    // The files a copy would join, in entry order — every one of them asked
    // for above, so there is nothing to drop and nothing to report.
    let files: Vec<PathBuf> = pieces.iter().map(|(source, _)| source.clone()).collect();
    let carry = carry_scoreboard(
        target,
        pickers.scoreboard,
        &compilation,
        // Frozen with the project as it is now: the run's own copy of the
        // events on the concat timeline (spec S2).
        ScoreboardContext::for_project(&open.project),
        &files,
    );
    // One record of the match for the whole run, shared by every entry of it.
    let match_media = Arc::new(MatchMedia {
        scoreboard: carry.scoreboard,
        highlights: open.project.player_highlights.clone(),
        // The project's one image, snapshotted like everything else here: a
        // pick or a removal while this run is going does not reach it (I6).
        avatar: open
            .project
            .avatar
            .as_ref()
            .map(|file| open.folder.join(file)),
    });
    let job = ExportJob {
        render: match carry.copy {
            true => Render::Copy(files),
            false => Render::Encode(Encode {
                audio: audio_regions(&compilation, &open.project.preferences),
                entries: pieces
                    .into_iter()
                    .map(|(source, clip)| EntryMedia {
                        source,
                        clip,
                        match_media: match_media.clone(),
                    })
                    .collect(),
                resolution: pickers.resolution,
                quality: pickers.quality,
            }),
        },
        tags: file_tags(&open.project, target, source_date(&sources)),
        compilation,
        path: exports.join(file_name(label, &open.project.name)),
        cues: carry.cues,
    };
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_joins_the_label_and_the_project_without_path_characters() {
        assert_eq!(file_name("All clips", "Game"), "All clips - Game.mp4");
        assert_eq!(
            file_name("4/4 press", "U13 vs. Ash: away"),
            "4-4 press - U13 vs. Ash- away.mp4"
        );
    }

    #[test]
    fn the_remaining_frames_are_the_unrendered_ones() {
        let target = |frames, state| ExportTargetRun {
            label: "t".into(),
            frames,
            state,
        };
        let run = ExportRun {
            targets: vec![
                target(100, TargetState::Done("a.mp4".into())),
                target(100, TargetState::Running(30)),
                target(50, TargetState::Pending),
            ],
            rate: None,
        };
        assert_eq!(run.remaining_frames(), 120);
        assert!(run.is_running());

        // A failed target owes nothing: the run goes on without it.
        let stopped = ExportRun {
            targets: vec![
                target(100, TargetState::Failed("no encoder".into())),
                target(50, TargetState::Cancelled),
            ],
            rate: None,
        };
        assert_eq!(stopped.remaining_frames(), 0);
        assert!(!stopped.is_running());
    }
}
