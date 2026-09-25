//! The whole match copied rather than re-encoded (spec L), end to end on
//! generated fixtures.
//!
//! The sources are counter fixtures, so the joined file is read back frame by
//! frame by the number each one shows: a copy that lost, duplicated or
//! reordered a packet says so in that list. `ffprobe`
//! ([`fixtures::ffprobe`](video_coach_media::fixtures::ffprobe)) is the
//! independent reader for everything the decoder can't see — the `stsd`, the
//! packet count, the presentation times and the chapters.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use video_coach_core::cues::{cues_to_srt, Cue};
use video_coach_core::export::{compilation_schedule, Compilation};
use video_coach_core::metadata::FileTags;
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{Project, SourceRef};
use video_coach_media::fixtures::{
    assert_export_tags, counter_video_with, decode_counters, ffprobe, sample_export_tags,
    CounterKind, CounterQuirks,
};
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
    whole_match_with(
        dir,
        sources,
        CounterKind::H264AacMp4,
        CounterQuirks::default(),
    )
}

/// [`whole_match`] of `kind`, with `quirks` on every source.
fn whole_match_with(
    dir: &Path,
    sources: &[(&str, u32, u32, u32)],
    kind: CounterKind,
    quirks: CounterQuirks,
) -> Match {
    gst::init().unwrap();
    let mut project = Project::new("Match");
    let files = sources
        .iter()
        .map(|&(name, w, h, frames)| {
            let file = format!("{name}.mp4");
            project.source_videos.push(SourceRef {
                relative_path: file.clone(),
                display_name: name.into(),
                // The **picture's** length, which is what a fixture's name
                // says. A source whose sound runs past it then has a plan
                // that disagrees with its file, which is the disagreement
                // `a_later_source_starts_where_the_plan_says` is about.
                duration_seconds: f64::from(frames) / f64::from(FPS),
                display_aspect: f64::from(w) / f64::from(h),
            });
            counter_video_with(&dir.join(file), w, h, FPS, frames, kind, quirks)
        })
        .collect();
    Match {
        files,
        frames: sources.iter().map(|&(_, _, _, frames)| frames).collect(),
        compilation: compilation_schedule(&project, &ExportTarget::WholeMatch),
    }
}

/// A copy of `m` to `path`, with no scoreboard at all: no sidecar written and
/// no track requested.
fn job(m: &Match, path: PathBuf) -> ExportJob {
    ExportJob {
        tags: FileTags::default(),
        compilation: m.compilation.clone(),
        path,
        cues: Some(Vec::new()),
        // One file per plan entry, in entry order: a whole match's entries are
        // its sources, in the order the project lists them.
        render: Render::Copy(m.files.clone()),
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

/// Every video packet's presentation time in `path`, in the file's own order.
fn video_pts(path: &Path) -> Vec<f64> {
    ffprobe(path, &["-select_streams", "v:0", "-show_packets"])["packets"]
        .as_array()
        .expect("a packets array")
        .iter()
        .map(|p| {
            p["pts_time"]
                .as_str()
                .expect("a packet presentation time")
                .parse()
                .expect("a presentation time in seconds")
        })
        .collect()
}

/// How long `path`'s `stream` (`v:0`, `a:0`) runs, in seconds.
fn stream_seconds(path: &Path, stream: &str) -> f64 {
    ffprobe(path, &["-select_streams", stream, "-show_streams"])["streams"][0]["duration"]
        .as_str()
        .expect("a stream duration")
        .parse()
        .expect("a stream duration in seconds")
}

/// `path`'s subtitle track as `(start, end, text)`, read out of the samples
/// themselves (spec T1).
///
/// **`mp4mux` writes an empty sample between cues** — measured, and expected:
/// 3,400 cues come back as 6,799 samples, and here two come back as three.
/// They are the muxer's own "nothing is on screen now", two bytes of zero
/// length and no text, and they are dropped here, so what is left is the cue
/// list that was written.
fn subtitle_cues(path: &Path) -> Vec<(f64, f64, String)> {
    ffprobe(
        path,
        &["-select_streams", "s:0", "-show_packets", "-show_data"],
    )["packets"]
        .as_array()
        .expect("a packets array")
        .iter()
        .filter_map(|p| {
            let seconds = |key: &str| {
                p[key]
                    .as_str()
                    .expect("a packet time")
                    .parse::<f64>()
                    .expect("a packet time in seconds")
            };
            let text = tx3g_text(p["data"].as_str().unwrap_or_default());
            let start = seconds("pts_time");
            (!text.is_empty()).then(|| (start, start + seconds("duration_time"), text))
        })
        .collect()
}

/// The text of one `tx3g` sample, out of `ffprobe -show_data`'s hex dump.
///
/// A sample is a two-byte big-endian length and then the UTF-8 itself. The
/// dump's own ASCII column is not read instead: it replaces every byte past
/// 0x7f with a dot, and the scoreboard's separator is a `·`. The hex runs in
/// fixed columns — eight bytes of offset, `": "`, then 39 columns — so taking
/// those and keeping the hex digits cannot stray into the ASCII.
fn tx3g_text(dump: &str) -> String {
    let hex: String = dump
        .lines()
        .flat_map(|line| line.chars().skip(10).take(39))
        .filter(char::is_ascii_hexdigit)
        .collect();
    let bytes: Vec<u8> = hex
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("hex is ASCII"), 16)
                .expect("a hex byte")
        })
        .collect();
    String::from_utf8(bytes.get(2..).unwrap_or_default().to_vec()).expect("a tx3g sample is UTF-8")
}

/// How many streams `path` has.
fn streams(path: &Path) -> usize {
    ffprobe(path, &["-show_streams"])["streams"]
        .as_array()
        .expect("a streams array")
        .len()
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

/// Every source's frames, in order: what the joined file must read back as.
fn want(m: &Match) -> Vec<u32> {
    m.frames.iter().flat_map(|&frames| 0..frames).collect()
}

/// Two sources joined: every frame of the first, then every frame of the
/// second, in the sources' own codec, with the plan's chapters on top and the
/// scoreboard both in an `.srt` beside it and on a `tx3g` track inside it.
///
/// The chapters are what prove the `moov` was reserved: without
/// `reserved-max-duration` the muxer writes it last, `chapters::splice` finds
/// no room and skips, and this reads back empty.
///
/// The cues are written as core formats them — the score and the clock are
/// core's, and nothing about SRT lives in media — and the same lines go on the
/// embedded track, which is what survives the file being copied somewhere a
/// sidecar isn't (spec T1). A cue whose separator is a `·` is deliberate: the
/// track carries UTF-8, not ASCII.
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
    let cues = vec![
        Cue {
            start: 0.0,
            end: 1.5,
            text: "Rovers 0 - 0 Athletic · 00:00".into(),
        },
        Cue {
            start: 1.5,
            end: 3.5,
            text: "Rovers 1 - 0 Athletic · 00:01".into(),
        },
    ];

    let path = dir.path().join("out.mp4");
    let done = copy(ExportJob {
        tags: FileTags::default(),
        cues: Some(cues.clone()),
        ..job(&m, path.clone())
    })
    .unwrap();
    assert_eq!(done.encoder, "copy");
    assert_eq!(done.diagnostics, Default::default());
    assert!(
        done.reserve_remaining > 0.0,
        "the moov reserve was never read back: {}",
        done.reserve_remaining
    );

    assert_eq!(
        decode_counters(&path),
        want(&m),
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

    let sidecar = dir.path().join("out.srt");
    assert_eq!(done.sidecar, Some(sidecar.clone()));
    assert_eq!(
        std::fs::read_to_string(&sidecar).unwrap(),
        cues_to_srt(&cues)
    );

    // And the same cues inside the file. The times are exact to the
    // millisecond because the track's timescale is pinned to 1000 (spec T1):
    // left automatic, a cue boundary rounds to whatever `mp4mux` chose.
    let embedded = subtitle_cues(&path);
    assert_eq!(
        embedded.len(),
        cues.len(),
        "the text track reads: {embedded:?}"
    );
    for ((start, end, text), cue) in embedded.iter().zip(&cues) {
        assert_eq!(text, &cue.text);
        assert!(
            (start - cue.start).abs() < 0.001 && (end - cue.end).abs() < 0.001,
            "the cue {text:?} runs {start}..{end}, not {}..{}",
            cue.start,
            cue.end
        );
    }
    let text = &ffprobe(&path, &["-select_streams", "s:0", "-show_streams"])["streams"][0];
    assert_eq!(text["codec_tag_string"].as_str(), Some("tx3g"));
    assert_eq!(text["time_base"].as_str(), Some("1/1000"));

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

/// One source: the common project, where nothing is joined at all.
///
/// **The join is the code that doesn't run here**, and with a single entry
/// `whole_match_chapters` gives no chapters either (spec E1), so the chapter
/// splice is handed an empty list. The copy still earns its place: it is the
/// file's own packets, with the scoreboard beside it.
#[test]
fn a_single_source_is_copied_with_no_chapters() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match(dir.path(), &[("the match", 640, 360, 45)]);
    assert!(
        m.compilation.plan.chapters.is_empty(),
        "one source has no chapters to write"
    );
    let cues = vec![Cue {
        start: 0.0,
        end: 1.5,
        text: "Rovers 0 - 0 Athletic · 00:00".into(),
    }];
    let path = dir.path().join("out.mp4");
    // No chapters means no pasteable list either, and the one an earlier run
    // left beside this path goes with them.
    let chapter_list = dir.path().join("out.chapters.txt");
    std::fs::write(&chapter_list, "0:00 an older cut\n").unwrap();

    let done = copy(ExportJob {
        tags: FileTags::default(),
        cues: Some(cues.clone()),
        ..job(&m, path.clone())
    })
    .unwrap();

    assert_eq!(decode_counters(&path), want(&m), "the copy lost frames");
    assert_eq!(video_stream(&path).4, video_stream(&m.files[0]).4);
    assert_eq!(done.sidecar, Some(dir.path().join("out.srt")));
    assert!(chapters(&path).is_empty(), "a lone source got a chapter");
    assert_eq!(done.chapter_list, None);
    assert!(
        !chapter_list.exists(),
        "a copy with no chapters left the old list beside it"
    );
    // Picture, sound and the scoreboard: a lone source is carried on the same
    // three tracks a joined one is.
    assert_eq!(streams(&path), 3, "a track went missing");
    assert_eq!(
        subtitle_cues(&path),
        vec![(cues[0].start, cues[0].end, cues[0].text.clone())]
    );
}

/// Sources with no sound at all, and no scoreboard to carry: neither pad is
/// requested and the output has picture and nothing else (spec E4, T1).
///
/// The muxer takes its pads once and for all before the first packet, so
/// "there is no audio track" and "there is no subtitle track" are decisions
/// made before `PLAYING` that nothing after can revisit — which is why they
/// have a test of their own rather than riding on the sounded ones.
#[test]
fn sources_with_no_sound_are_copied_without_an_audio_track() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match_with(
        dir.path(),
        &[("first half", 640, 360, 60), ("second half", 640, 360, 45)],
        CounterKind::H264Mp4BFrames,
        CounterQuirks::default(),
    );
    let path = dir.path().join("out.mp4");

    copy(job(&m, path.clone())).unwrap();

    assert_eq!(
        decode_counters(&path),
        want(&m),
        "the join lost, repeated or reordered frames"
    );
    assert_eq!(
        streams(&path),
        1,
        "the copy invented a track nothing was asked to be put on"
    );
}

/// Sources whose sound stops three seconds short of their picture: the
/// muxer needs sound the first file has run out of, and only the *next*
/// file's demuxer can give it.
///
/// **This is the deadlock, with the race taken out.** The two-`concat` graph
/// hung on it every time, and on ordinary footage — where the two tracks end
/// a few milliseconds apart — roughly one run in three, or four in eight
/// under load: the muxer waited on a stream whose demuxer was blocked
/// pushing the *other* one, and how far apart the two `concat`s switched was
/// the machine's business. Nothing here is the machine's business any more:
/// one file is copied at a time, and a push never waits unless every muxer
/// pad already has something to write.
#[test]
fn a_source_whose_sound_stops_early_is_copied() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match_with(
        dir.path(),
        &[
            ("first half", 640, 360, 120),
            ("second half", 640, 360, 120),
        ],
        CounterKind::H264AacMp4,
        CounterQuirks {
            // Four seconds of picture, one of sound: three times the second
            // a `queue` holds, so no queue size could have hidden this.
            audio_tail: -90,
            ..CounterQuirks::default()
        },
    );
    let path = dir.path().join("out.mp4");

    copy(job(&m, path.clone())).unwrap();

    assert_eq!(
        decode_counters(&path),
        want(&m),
        "the join lost, repeated or reordered frames"
    );

    // **And the second half's sound starts where its picture does.** One base
    // per source advances both tracks together (spec L7): eight seconds of
    // picture, and sound that runs out a second into each half — five in all.
    // A base per track, which `concat` had, would start the second half's
    // sound where the first half's ran out, three seconds early, and every
    // further source would add its own.
    let (video, audio) = (stream_seconds(&path, "v:0"), stream_seconds(&path, "a:0"));
    assert!(
        (video - 8.0).abs() < 0.05 && (audio - 5.0).abs() < 0.1,
        "the join moved a track: {video} s of picture against {audio} s of sound"
    );
}

/// A later source starts where the **plan** says it does, not where the last
/// one's longest track happened to end.
///
/// **Three placements have to agree**: the copy's, the chapters' and the
/// cues'. The last two come from `start_frame / OUTPUT_FPS`, so the first does
/// too. Here each source's sound runs a second past its picture, which is what
/// ordinary camera footage does in miniature — and a copy that started the
/// next source at the end of *everything* written would put it a second late,
/// with its chapter and its score a second early.
#[test]
fn a_later_source_starts_where_the_plan_says() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match_with(
        dir.path(),
        &[("first half", 640, 360, 60), ("second half", 640, 360, 60)],
        CounterKind::H264AacMp4,
        CounterQuirks {
            // A second of sound past the picture, on every source.
            audio_tail: 30,
            ..CounterQuirks::default()
        },
    );
    let path = dir.path().join("out.mp4");

    copy(job(&m, path.clone())).unwrap();

    assert_eq!(
        decode_counters(&path),
        want(&m),
        "the join lost, repeated or reordered frames"
    );
    let start = f64::from(m.compilation.plan.entries[1].start_frame as u32) / f64::from(FPS);
    let first = video_stream(&m.files[0]).4 as usize;
    let pts = video_pts(&path);
    assert!(
        pts.len() > first,
        "the copy is short: {} packets",
        pts.len()
    );
    // The lowest presentation time among the second source's packets: with
    // B-frames they arrive in decode order, so the first one written is not
    // necessarily the first one shown.
    let second = pts[first..].iter().copied().fold(f64::INFINITY, f64::min);
    assert!(
        (second - start).abs() < 0.02,
        "the second half starts at {second} s; the plan, its chapter and its \
         cues say {start} s"
    );
    // And the sound the plan didn't account for is simply carried: four
    // seconds of picture, and a tail that ends with the second half's own.
    let (video, audio) = (stream_seconds(&path, "v:0"), stream_seconds(&path, "a:0"));
    assert!(
        (video - 4.0).abs() < 0.05 && (audio - 5.0).abs() < 0.1,
        "the overlap left {video} s of picture against {audio} s of sound"
    );
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
/// **The copy is held where it is until the cancel has landed.** The first
/// progress report blocks the copy thread until this one has set the flag, so
/// the test measures the cancel rather than racing a copy that takes a
/// seventh of a second.
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

    let (report, reported) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let result = copy_with(
        job(&m, path.clone()),
        {
            let mut held = false;
            move |frames| {
                if !std::mem::replace(&mut held, true) {
                    let _ = report.send(frames);
                    let _ = resume.recv_timeout(TIMEOUT);
                }
            }
        },
        |exporter| {
            reported
                .recv_timeout(TIMEOUT)
                .expect("the copy reported its progress");
            exporter.cancel();
            let _ = release.send(());
        },
    );
    assert_eq!(result, Err(ExportError::Cancelled));
    assert!(!path.exists(), "a cancelled copy left a file");
    assert!(
        !dir.path().join("out.mp4.part").exists(),
        "a cancelled copy left its .part"
    );
}

/// Cancelling while the headers are still being read: the same answer, and
/// still nothing written.
///
/// **The `.part` doesn't exist yet at that point**, which is the whole design
/// of the gate (spec L6) — so what this pins is that the header pass notices
/// the flag at all, rather than reading four files out and only then asking.
#[test]
fn a_cancel_during_the_header_pass_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match(
        dir.path(),
        &[
            ("first half", 640, 360, 30),
            ("second half", 640, 360, 30),
            ("third half", 640, 360, 30),
            ("fourth half", 640, 360, 30),
        ],
    );
    let path = dir.path().join("out.mp4");

    // Cancelled before the copy thread can have read a header: the flag is
    // set within microseconds of the run starting, and one file's header takes
    // milliseconds.
    let result = copy_with(job(&m, path.clone()), |_| {}, Exporter::cancel);

    assert_eq!(result, Err(ExportError::Cancelled));
    assert!(!path.exists(), "a cancelled copy left a file");
    assert!(
        !dir.path().join("out.mp4.part").exists(),
        "a cancelled copy left its .part"
    );
}

/// The copy tags its file, without costing it a byte of the streams or the
/// chapters behind it.
///
/// **Tags are header boxes**, so the proof that they are free is the rest of
/// the file being what it was: the same `stsd`, the same packet count, the
/// same frames in the same order, and the chapters still spliced into the
/// `moov` the tags share. Measured, `mp4mux` grows the reserved header to fit
/// them rather than spending the `free` box `chapters::splice` eats into.
#[test]
fn a_tagged_copy_is_still_lossless_and_still_chaptered() {
    let dir = tempfile::tempdir().unwrap();
    let m = whole_match(
        dir.path(),
        &[("first half", 640, 360, 45), ("second half", 640, 360, 30)],
    );
    let inputs: Vec<(String, String, i64, i64, i64)> =
        m.files.iter().map(|f| video_stream(f)).collect();
    let expected: Vec<(f64, String)> = m.compilation.plan.chapters.clone();
    assert_eq!(expected.len(), 2, "one chapter per source: {expected:?}");

    let path = dir.path().join("out.mp4");
    let done = copy(ExportJob {
        tags: sample_export_tags(),
        ..job(&m, path.clone())
    })
    .unwrap();
    assert_eq!(done.encoder, "copy");
    assert!(
        done.reserve_remaining > 0.0,
        "the tags ate the moov reserve"
    );

    assert_export_tags(&path);

    // Still the sources' own packets, described the way they described
    // themselves.
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
    assert_eq!(decode_counters(&path), want(&m));

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

/// The pasteable chapter list lands beside the copied file too: it is
/// `composite::export`'s `finish` that writes it, which both renderers go
/// through, and this is the copy's half of that.
///
/// **The chapters are set by hand and the film is three seconds long**, as on
/// the encoded path: the list is a pure function of `plan.chapters`
/// (`video_coach_core::chapters::chapter_list`), so joining half an hour of
/// footage to space three chapters ten seconds apart would buy nothing. The
/// `chpl` box gets the same list, which is what proves the two readings of
/// `plan.chapters` — the one inside the file and the one beside it — are of
/// the same list.
#[test]
fn a_chapter_list_lands_beside_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = whole_match(
        dir.path(),
        &[("first half", 640, 360, 60), ("second half", 640, 360, 45)],
    );
    m.compilation.plan.chapters = vec![
        (0.0, "Kick-off".into()),
        (845.4, "Rovers goal 1-0".into()),
        (1651.9, "Half time".into()),
    ];
    let path = dir.path().join("out.mp4");

    let done = copy(job(&m, path.clone())).unwrap();

    let chapter_list = dir.path().join("out.chapters.txt");
    assert_eq!(done.chapter_list, Some(chapter_list.clone()));
    assert_eq!(
        std::fs::read_to_string(&chapter_list).unwrap(),
        "0:00 Kick-off\n14:05 Rovers goal 1-0\n27:31 Half time\n"
    );
    assert_eq!(decode_counters(&path), want(&m), "the copy lost frames");
    let titles: Vec<String> = chapters(&path).into_iter().map(|c| c.1).collect();
    assert_eq!(titles, ["Kick-off", "Rovers goal 1-0", "Half time"]);
}
