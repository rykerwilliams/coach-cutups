//! The whole match copied rather than re-encoded (spec L), end to end on
//! generated fixtures.
//!
//! The sources are counter fixtures, so the joined file is read back frame by
//! frame by the number each one shows: a copy that lost, duplicated or
//! reordered a packet says so in that list. `ffprobe` is the independent
//! reader for everything the decoder can't see — the `stsd`, the packet count
//! and the chapters.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use video_coach_core::export::{compilation_schedule, Compilation};
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Project, Quality, Resolution, SourceRef};
use video_coach_media::fixtures::{counter_video, decode_counters, CounterKind};
use video_coach_media::{ExportDone, ExportError, ExportJob, ExportMessage, Exporter, Render};

/// Far beyond any copy here; only a hang reaches it.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Every fixture's frame rate, which is also the output's.
const FPS: u32 = 30;

/// A whole match's sources and the compilation over them.
struct Match {
    files: Vec<PathBuf>,
    /// Each source's frame count, in order: what the joined file must read
    /// back as `0..frames` per source.
    frames: Vec<u32>,
    compilation: Compilation,
}

/// `sources` as `(name, width, height, frames)`, written as H.264 + AAC in
/// MP4 — the shape a copy can join — and planned as one whole match.
fn whole_match(dir: &Path, sources: &[(&str, u32, u32, u32)]) -> Match {
    gst::init().unwrap();
    let mut project = Project::new("Match");
    let files = sources
        .iter()
        .map(|&(name, w, h, frames)| {
            let file = format!("{name}.mp4");
            project.source_videos.push(SourceRef {
                relative_path: file.clone(),
                display_name: name.into(),
                duration_seconds: f64::from(frames) / f64::from(FPS),
                display_aspect: f64::from(w) / f64::from(h),
            });
            counter_video(&dir.join(file), w, h, FPS, frames, CounterKind::H264AacMp4)
        })
        .collect();
    Match {
        files,
        frames: sources.iter().map(|&(_, _, _, frames)| frames).collect(),
        compilation: compilation_schedule(&project, &ExportTarget::WholeMatch),
    }
}

/// A copy of `m` to `path`.
fn job(m: &Match, path: PathBuf) -> ExportJob {
    ExportJob {
        entries: vec![None; m.compilation.plan.entries.len()],
        compilation: m.compilation.clone(),
        sources: m.files.clone(),
        audio: Vec::new(),
        path,
        render: Render::Copy,
        // Both unread by a copy, which carries the sources' own pixels.
        resolution: Resolution::R1080,
        quality: Quality::Medium,
        scoreboard: None,
        highlights: Vec::new(),
        avatar: None,
    }
}

/// Runs `job` to its `Finished`, calling `on_progress` with the frames copied
/// so far on the copy thread, and `with_exporter` on this one while it runs.
fn copy_with(
    job: ExportJob,
    mut on_progress: impl FnMut(usize) + Send + 'static,
    with_exporter: impl FnOnce(&Exporter),
) -> Result<ExportDone, ExportError> {
    let frames = job.compilation.plan.total_frames();
    let (tx, rx) = mpsc::channel();
    let begun = Instant::now();
    let exporter = Exporter::start(job, move |msg| match msg {
        ExportMessage::Progress(p) => on_progress(p),
        ExportMessage::Finished(result) => {
            let _ = tx.send(result);
        }
    });
    with_exporter(&exporter);
    let result = rx.recv_timeout(TIMEOUT).expect("the copy finished");
    eprintln!(
        "copy: {frames} output frames in {:.3} s",
        begun.elapsed().as_secs_f64()
    );
    result
}

fn copy(job: ExportJob) -> Result<ExportDone, ExportError> {
    copy_with(job, |_| {}, |_| {})
}

/// `path` read by `ffprobe`, as JSON.
///
/// `ffprobe` is the independent reader: it sees the `stsd` and the packets a
/// decoder hides. Without it these tests fail, never skip — it is a test-only
/// build dependency (`packaging/build-deps.txt`).
fn ffprobe(path: &Path, args: &[&str]) -> serde_json::Value {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-of", "json"])
        .args(args)
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
    serde_json::from_slice(&out.stdout).expect("ffprobe wrote JSON")
}

/// `path`'s video stream as `(codec, profile, width, height, packets)`.
fn video_stream(path: &Path) -> (String, String, i64, i64, i64) {
    let probe = ffprobe(
        path,
        &["-select_streams", "v:0", "-show_streams", "-count_packets"],
    );
    let stream = &probe["streams"][0];
    let text = |key: &str| stream[key].as_str().unwrap_or_default().to_owned();
    let number = |key: &str| {
        stream[key]
            .as_i64()
            .or_else(|| stream[key].as_str()?.parse().ok())
            .unwrap_or_default()
    };
    (
        text("codec_name"),
        text("profile"),
        number("width"),
        number("height"),
        number("nb_read_packets"),
    )
}

/// `path`'s chapters as `(start in seconds, title)`.
fn chapters(path: &Path) -> Vec<(f64, String)> {
    ffprobe(path, &["-show_chapters"])["chapters"]
        .as_array()
        .expect("a chapters array")
        .iter()
        .map(|c| {
            (
                c["start_time"].as_str().unwrap().parse().unwrap(),
                c["tags"]["title"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// Two sources joined: every frame of the first, then every frame of the
/// second, in the sources' own codec, with the plan's chapters on top.
///
/// The chapters are what prove the `moov` was reserved: without
/// `reserved-max-duration` the muxer writes it last, `chapters::splice` finds
/// no room and skips, and this reads back empty.
#[test]
fn a_copy_of_two_sources_is_lossless_and_chaptered() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match(
        dir.path(),
        &[("first half", 640, 360, 60), ("second half", 640, 360, 45)],
    );
    let inputs: Vec<(String, String, i64, i64, i64)> =
        m.files.iter().map(|f| video_stream(f)).collect();
    let expected: Vec<(f64, String)> = m.compilation.plan.chapters.clone();
    assert_eq!(expected.len(), 2, "one chapter per source: {expected:?}");

    let path = dir.path().join("out.mp4");
    let done = copy(job(&m, path.clone())).unwrap();
    assert_eq!(done.encoder, "copy");
    assert_eq!(done.diagnostics, Default::default());
    assert!(
        done.reserve_remaining.is_some_and(|left| left > 0.0),
        "the moov reserve was never read back: {:?}",
        done.reserve_remaining
    );

    let counters = decode_counters(&path);
    let want: Vec<u32> = m.frames.iter().flat_map(|&frames| 0..frames).collect();
    assert_eq!(
        counters, want,
        "the join lost, repeated or reordered frames"
    );

    let (codec, profile, w, h, packets) = video_stream(&path);
    assert_eq!(
        (codec.as_str(), profile.as_str(), w, h),
        (
            inputs[0].0.as_str(),
            inputs[0].1.as_str(),
            inputs[0].2,
            inputs[0].3
        ),
        "the copy re-described the video"
    );
    assert_eq!(
        packets,
        inputs.iter().map(|i| i.4).sum::<i64>(),
        "the copy is not packet for packet"
    );

    let got = chapters(&path);
    assert_eq!(got.len(), expected.len(), "chapters read back: {got:?}");
    for ((at, title), (want_at, want_title)) in got.iter().zip(&expected) {
        assert_eq!(title, want_title);
        assert!(
            (at - want_at).abs() < 0.001,
            "{title:?} starts at {at}, not {want_at}"
        );
    }
}

/// Two sources recorded differently: refused, naming the one that differs,
/// with nothing written.
///
/// **Without the gate this run succeeds** (measured): `mp4mux` writes one
/// file with one `stsd` describing most of its samples wrongly, with no error
/// and no warning. Nothing downstream refuses a mismatch, so the gate is the
/// whole of the protection.
#[test]
fn a_mismatched_pair_refuses_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match(
        dir.path(),
        &[("first half", 640, 360, 60), ("second half", 320, 240, 45)],
    );
    let path = dir.path().join("out.mp4");

    let result = copy(job(&m, path.clone()));
    // Without the gate this reads `Ok(ExportDone { .. })`, which is the whole
    // point of the test.
    let Err(ExportError::Failed(why)) = &result else {
        panic!("expected a refusal, got {result:?}");
    };
    assert!(
        why.contains("second half"),
        "the refusal names no file: {why}"
    );
    assert!(
        why.contains("burned in"),
        "the refusal offers no way out: {why}"
    );
    assert!(!path.exists(), "a refused copy left a file");
    assert!(
        !dir.path().join("out.mp4.part").exists(),
        "a refused copy left its .part"
    );
}

/// Cancelling part-way: no file, no `.part`, and the run says so.
///
/// **Two 30-second sources is what makes "part-way" mean it.** Uncancelled,
/// this copy runs 0.149 s (measured) against the ~0.02 s the first progress
/// report takes, so the cancel lands in the middle of the run rather than in
/// a race with its end.
#[test]
fn a_cancelled_copy_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match(
        dir.path(),
        &[
            ("first half", 1280, 720, 900),
            ("second half", 1280, 720, 900),
        ],
    );
    let path = dir.path().join("out.mp4");

    let (seen, copied) = mpsc::channel();
    let result = copy_with(
        job(&m, path.clone()),
        move |p| {
            let _ = seen.send(p);
        },
        |exporter| {
            // Err means the copy finished before it reported anything, which
            // the assertion below reports.
            let _ = copied.recv_timeout(TIMEOUT);
            exporter.cancel();
        },
    );
    assert_eq!(result, Err(ExportError::Cancelled));
    assert!(!path.exists(), "a cancelled copy left a file");
    assert!(
        !dir.path().join("out.mp4.part").exists(),
        "a cancelled copy left its .part"
    );
}
