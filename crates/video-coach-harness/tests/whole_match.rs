//! Bus end to end: the export sheet's Scoreboard picker (spec M), the copied
//! whole match and the `.srt` beside it.
//!
//! The sources here are H.264 + AAC in MP4 — the shape a copy can join —
//! rather than the WebM fixtures the rest of the harness uses, because what
//! these tests are about is the renderer the picker chooses.
//!
//! Layout per test: `<tmp>/config` holds the state file, `<tmp>/project` the
//! project (and, once a run starts, its `exports/`), `<tmp>/media` the
//! fixture game videos.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use uuid::Uuid;
use video_coach_app::bus::{Command, Event, ExportRun, TargetState};
use video_coach_core::cues::{cues_to_srt, scoreboard_cues};
use video_coach_core::export::compilation_schedule;
use video_coach_core::plan::{compilation_plan, ExportTarget, ScoreboardMode};
use video_coach_core::project::{Project, Quality, Resolution, SourceRef};
use video_coach_core::scoreboard::{
    MatchEventKind, ScoreboardConfig, ScoreboardContext, TeamConfig,
};
use video_coach_core::store::{self, EXPORTS_DIRNAME};
use video_coach_core::stroke::Rgba;
use video_coach_harness::{add_clips, Harness};
use video_coach_media::fixtures::{counter_video, CounterKind};

/// Every fixture's frame rate, which is also the output's.
const FPS: u32 = 30;

/// A project of H.264 + AAC MP4 sources, with a scoreboard, a kick-off and a
/// home goal tagged, opened on a fresh bus.
struct Match {
    h: Harness,
    folder: PathBuf,
    /// The project as the tags left it.
    project: Project,
    /// Each source's video packet count, for the copy's own proof.
    packets: Vec<i64>,
    _tmp: TempDir,
}

impl Match {
    /// `sources` as `(name, width, height, frames)`. The kick-off is tagged
    /// early in the first source and a home goal half-way through it, so the
    /// cue list has a score turning over and a clock running.
    fn open(sources: &[(&str, u32, u32, u32)]) -> Self {
        Self::open_with(sources, &[])
    }

    /// [`Match::open`], with a clip of `clip_seconds[i]` on source 0 for each
    /// entry — a target of its own beside the whole match.
    fn open_with(sources: &[(&str, u32, u32, u32)], clip_seconds: &[f64]) -> Self {
        gstreamer::init().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("project");
        let media = tmp.path().join("media");
        for dir in [&folder, &media] {
            std::fs::create_dir(dir).unwrap();
        }

        let mut project = Project::new("Game");
        let mut packets = Vec::with_capacity(sources.len());
        for &(name, w, h, frames) in sources {
            let file = format!("{name}.mp4");
            let path = counter_video(
                &media.join(&file),
                w,
                h,
                FPS,
                frames,
                CounterKind::H264AacMp4,
            );
            packets.push(video_packets(&path));
            project.source_videos.push(SourceRef {
                relative_path: format!("../media/{file}"),
                display_name: name.into(),
                duration_seconds: f64::from(frames) / f64::from(FPS),
                display_aspect: f64::from(w) / f64::from(h),
            });
        }
        add_clips(&folder, &mut project, &vec![0; clip_seconds.len()]);
        for (clip, &secs) in project.clips.iter_mut().zip(clip_seconds) {
            clip.recording_duration = secs;
            clip.name = "Lesson".into();
        }
        store::write(&folder, &mut project).unwrap();

        let mut h = Harness::new(&tmp.path().join("config"));
        h.send(Command::OpenProject(folder.clone()));
        h.wait_opened();

        h.send(Command::SetScoreboard(ScoreboardConfig {
            home: TeamConfig::new("Rovers", Rgba::RED, Rgba::RED),
            away: TeamConfig::new("United", Rgba::RED, Rgba::RED),
            format: Default::default(),
            auto_back_anchor_p1: false,
        }));
        h.wait_changed();
        let tags = [
            (MatchEventKind::StartStop, 0.2),
            (MatchEventKind::HomeGoal, 1.0),
        ];
        for &(kind, source_seconds) in &tags {
            h.send(Command::TagMatchEvent {
                kind,
                source_index: 0,
                source_seconds,
            });
        }
        let project = tags
            .iter()
            .map(|_| h.wait_changed())
            .last()
            .unwrap()
            .project;

        Match {
            h,
            folder,
            project: (*project).clone(),
            packets,
            _tmp: tmp,
        }
    }

    fn export(&self, targets: Vec<ExportTarget>, scoreboard: Option<ScoreboardMode>) {
        self.h.send(Command::Export {
            targets,
            resolution: Resolution::R720,
            quality: Quality::Low,
            scoreboard,
        });
    }

    /// The run's last word: the event with nothing left running.
    fn outcome(&mut self) -> ExportRun {
        self.h.wait_map("the run's outcome", |e| match e {
            Event::Export(run) if !run.is_running() => Some(run.clone()),
            _ => None,
        })
    }

    fn exports(&self) -> PathBuf {
        self.folder.join(EXPORTS_DIRNAME)
    }

    fn clip(&self) -> Uuid {
        self.project.clips[0].id
    }

    /// The scoreboard cue list of `target`, as core builds it: what the
    /// sidecar must hold, byte for byte.
    fn srt(&self, target: &ExportTarget) -> String {
        let compilation = compilation_schedule(&self.project, target);
        let context = ScoreboardContext::for_project(&self.project).expect("a scoreboard is set");
        cues_to_srt(&scoreboard_cues(&compilation, &context))
    }
}

/// The files in `exports/`, sorted, `.part` files included. Empty when no run
/// has created the folder.
fn outputs(exports: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(exports)
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `path`'s video packet count, as `ffprobe` reads it.
///
/// `ffprobe` is the independent reader: it counts the packets a decoder
/// hides. Without it these tests fail, never skip — it is a test-only build
/// dependency (`packaging/build-deps.txt`).
fn video_packets(path: &Path) -> i64 {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-of", "json"])
        .args(["-select_streams", "v:0", "-show_streams", "-count_packets"])
        .arg(path)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "ffprobe didn't run ({e}): install the `ffmpeg` package (packaging/build-deps.txt)"
            )
        });
    assert!(
        out.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let probe: serde_json::Value = serde_json::from_slice(&out.stdout).expect("ffprobe wrote JSON");
    let packets = &probe["streams"][0]["nb_read_packets"];
    packets
        .as_i64()
        .or_else(|| packets.as_str()?.parse().ok())
        .unwrap_or_else(|| panic!("no packet count in {probe}"))
}

/// The whole match asked for on a separate track is copied, not re-encoded,
/// and the scoreboard lands beside it as core formats it.
#[test]
fn the_whole_match_is_copied_with_a_sidecar() {
    let mut m = Match::open(&[("first half", 640, 360, 60), ("second half", 640, 360, 45)]);
    let target = ExportTarget::WholeMatch;
    let plan = compilation_plan(&m.project, &target);
    let srt = m.srt(&target);

    m.export(vec![target], Some(ScoreboardMode::Track));
    let run = m.outcome();
    assert_eq!(run.targets[0].label, "Whole match");
    assert_eq!(run.targets[0].frames, plan.total_frames());
    let TargetState::Done(path) = &run.targets[0].state else {
        panic!("{:?}", run.targets[0]);
    };

    assert_eq!(
        outputs(&m.exports()),
        ["Whole match - Game.mp4", "Whole match - Game.srt"]
    );
    // Every source packet is in the output: a copy, not a re-encode, which
    // would have written 1280x720 frames of its own.
    assert_eq!(video_packets(path), m.packets.iter().sum::<i64>());

    let written = std::fs::read_to_string(path.with_extension("srt")).unwrap();
    assert_eq!(written, srt);
    let first = written.lines().nth(2).expect("a first cue");
    assert!(
        first.starts_with("Rovers 0 - 0 United · "),
        "the first cue reads {first:?}"
    );
    m.h.shutdown();
}

/// With the picker on Default the mode is the target's: the whole match is
/// copied and gets its sidecar, the clip is re-encoded and gets none.
#[test]
fn the_default_mode_copies_the_whole_match_and_burns_a_clip() {
    let mut m = Match::open_with(&[("first half", 640, 360, 30)], &[0.5]);
    m.export(
        vec![ExportTarget::WholeMatch, ExportTarget::Clip(m.clip())],
        None,
    );
    let run = m.outcome();
    for target in &run.targets {
        assert!(matches!(target.state, TargetState::Done(_)), "{target:#?}");
    }
    assert_eq!(
        outputs(&m.exports()),
        [
            "Lesson - Game.mp4",
            "Whole match - Game.mp4",
            "Whole match - Game.srt",
        ]
    );
    m.h.shutdown();
}

/// Only the whole match carries a cue list (spec T1). A clip asked for on a
/// separate track is still re-encoded — for its drawings, its inset and its
/// zoom — with the board simply left out of the picture, and nothing beside
/// it: it already says what it is in its own text bar.
#[test]
fn a_clip_on_a_separate_track_gets_no_sidecar() {
    let mut m = Match::open_with(&[("first half", 640, 360, 30)], &[0.5]);
    m.export(
        vec![ExportTarget::Clip(m.clip())],
        Some(ScoreboardMode::Track),
    );
    let run = m.outcome();
    assert!(
        matches!(run.targets[0].state, TargetState::Done(_)),
        "{:#?}",
        run.targets[0]
    );
    assert_eq!(outputs(&m.exports()), ["Lesson - Game.mp4"]);
    m.h.shutdown();
}

/// Switching the picker back burns the board into the picture, and the
/// sidecar the last run left goes with it — or the old score plays over the
/// new film.
#[test]
fn a_burned_whole_match_removes_a_stale_sidecar() {
    let mut m = Match::open(&[("first half", 640, 360, 30)]);
    m.export(vec![ExportTarget::WholeMatch], Some(ScoreboardMode::Track));
    m.outcome();
    let sidecar = m.exports().join("Whole match - Game.srt");
    assert!(sidecar.exists(), "{:?}", outputs(&m.exports()));

    m.export(vec![ExportTarget::WholeMatch], Some(ScoreboardMode::Burned));
    let run = m.outcome();
    assert!(
        matches!(run.targets[0].state, TargetState::Done(_)),
        "{:#?}",
        run.targets[0]
    );
    assert_eq!(outputs(&m.exports()), ["Whole match - Game.mp4"]);
    m.h.shutdown();
}
