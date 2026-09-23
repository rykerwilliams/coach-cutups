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
use std::time::Instant;

use uuid::Uuid;
use video_coach_core::audio::audio_regions;
use video_coach_core::export::{compilation_schedule, RateWindow, OUTPUT_FPS};
use video_coach_core::plan::{compilation_plan, ExportTarget};
use video_coach_core::project::{Clip, Project, Quality, Resolution};
use video_coach_core::reel::reel_goals;
use video_coach_core::scoreboard::ScoreboardContext;
use video_coach_core::store::{EXPORTS_DIRNAME, RECORDINGS_DIRNAME};
use video_coach_core::tag::tag_summaries;
use video_coach_media::{EntryMedia, ExportDone, ExportError, ExportJob, ExportMessage, Exporter};

use super::{Bus, Event, Input, Open, UserError};

/// What the every-clip target is called, in the sheet and in its file name.
const ALL_CLIPS_LABEL: &str = "All clips";
/// What the goals reel is called, in the sheet and in its file name.
const REEL_LABEL: &str = "All goals";
/// What the whole-match export is called, in the sheet and in its file name.
const WHOLE_MATCH_LABEL: &str = "Whole match";

/// What a clip with no name of its own is called.
const UNTITLED: &str = "Untitled";

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

/// The sheet's targets: Whole match, All clips, one row per tag, All goals,
/// then `selected` if a clip is (spec E8, match vision specs R1 and W1).
///
/// A target with no plan entry is left out, since there is nothing to export
/// in it — which is also what keeps an empty project's sheet empty, and the
/// reel's row away until there is a goal.
pub fn export_targets(project: &Project, selected: Option<Uuid>) -> Vec<ExportTargetRow> {
    let row = |target: ExportTarget, label: String| {
        let plan = compilation_plan(project, &target);
        let (count, unit) = match target {
            ExportTarget::Reel => (reel_goals(project).len(), "goal"),
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
    rows.extend(row(ExportTarget::Reel, REEL_LABEL.into()));
    if let Some(clip) = selected.and_then(|id| project.clips.iter().find(|c| c.id == id)) {
        rows.extend(row(
            ExportTarget::Clip(clip.id),
            clip_label(clip).to_owned(),
        ));
    }
    rows
}

/// What a clip is called in a file name and in a refusal: its own name, or
/// [`UNTITLED`] when it hasn't been given one.
fn clip_label(clip: &Clip) -> &str {
    match clip.name.trim() {
        "" => UNTITLED,
        name => name,
    }
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
                     decoder {:?}, glupload caps {:?}, encoder {}, chapters {:?}",
                    done.path.display(),
                    target.frames,
                    target.frames as f64 / seconds,
                    d.decoder,
                    d.glupload_caps,
                    done.encoder,
                    done.chapters
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
    ) {
        if let Err(e) = self.start_run(targets, resolution, quality) {
            self.emit(Event::Error(e));
        }
    }

    fn start_run(
        &mut self,
        targets: Vec<ExportTarget>,
        resolution: Resolution,
        quality: Quality,
    ) -> Result<(), UserError> {
        let refused = |why: &str| UserError::CantExport(why.into());
        if self.export.is_some() {
            return Err(refused("an export is running"));
        }
        // Both composite on the UI's GL context, and an export would take the
        // frames the preview is pacing itself on (spec P5).
        if self.preview.is_some() {
            return Err(refused("a preview is open; close it first"));
        }
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
        let mut jobs = VecDeque::with_capacity(targets.len());
        let mut rows = Vec::with_capacity(targets.len());
        for (target, label) in targets.iter().zip(labels) {
            let job = job(
                open,
                &self.missing,
                &exports,
                target,
                &label,
                resolution,
                quality,
            )?;
            rows.push(ExportTargetRun {
                label,
                frames: job.compilation.frames.len(),
                state: TargetState::Pending,
            });
            jobs.push_back(job);
        }
        // On demand, so a project that has never been exported has no empty
        // folder (spec E6). After the refusals: a run that can't start
        // shouldn't leave one behind either.
        std::fs::create_dir_all(&exports).map_err(|e| {
            UserError::CantExport(format!("could not create {}: {e}", exports.display()))
        })?;

        // The sheet's pickers are the project's from here on (spec E4).
        if let Some(open) = &mut self.open {
            let prefs = &mut open.project.preferences;
            if (prefs.last_export_resolution, prefs.last_export_quality) != (resolution, quality) {
                prefs.last_export_resolution = resolution;
                prefs.last_export_quality = quality;
                self.project_changed();
            }
        }

        let first = jobs.pop_front().expect("the targets are not empty");
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
        Ok(())
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
        ExportTarget::Reel => Ok(REEL_LABEL.to_owned()),
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

/// The job that renders `target` as `label`, or why it can't run.
///
/// A snapshot: later edits to the project don't reach a running export. The
/// whole source list goes with it because `PlanEntry::source_index` indexes
/// it — a compilation may walk several game videos.
fn job(
    open: &Open,
    missing: &[bool],
    exports: &Path,
    target: &ExportTarget,
    label: &str,
    resolution: Resolution,
    quality: Quality,
) -> Result<ExportJob, UserError> {
    let refused = |why: String| UserError::CantExport(why);
    let compilation = compilation_schedule(&open.project, target);
    if compilation.frames.is_empty() {
        return Err(refused(format!("{label} has nothing to export")));
    }

    let recordings = open.folder.join(RECORDINGS_DIRNAME);
    let mut entries = Vec::with_capacity(compilation.plan.entries.len());
    for entry in &compilation.plan.entries {
        let clip = entry.clip_id.map(|id| {
            open.project
                .clips
                .iter()
                .find(|c| c.id == id)
                .expect("the plan's clips are the project's clips")
        });
        if missing.get(entry.source_index).copied().unwrap_or(true) {
            let what = match clip {
                Some(clip) => format!("{}'s game video", clip_label(clip)),
                // A reel or whole-match entry has no clip to name: the file
                // names itself.
                None => {
                    let file = open
                        .project
                        .source_videos
                        .get(entry.source_index)
                        .map_or("a video", |s| s.display_name.as_str());
                    format!("{file} (the game video)")
                }
            };
            return Err(refused(format!("{what} is missing; relink it first")));
        }
        let Some(clip) = clip else {
            entries.push(None);
            continue;
        };
        let name = clip_label(clip);
        let recording = recordings.join(&clip.recording_filename);
        if !recording.exists() {
            return Err(refused(format!("{name}'s commentary recording is missing")));
        }
        entries.push(Some(EntryMedia {
            recording,
            clip: clip.clone(),
        }));
    }

    let job = ExportJob {
        audio: audio_regions(&compilation, &open.project.preferences),
        compilation,
        entries,
        sources: open
            .project
            .source_videos
            .iter()
            .map(|s| open.folder.join(&s.relative_path))
            .collect(),
        path: exports.join(file_name(label, &open.project.name)),
        resolution,
        quality,
        // Frozen with the project as it is now: the run's own copy of the
        // events on the concat timeline (spec S2).
        scoreboard: ScoreboardContext::for_project(&open.project),
        highlights: open.project.player_highlights.clone(),
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
