//! Export end to end through the real graph: the GPU where there is one,
//! llvmpipe on CI.
//!
//! **720p, short entries.** llvmpipe composites a 1080p frame in 75 ms with
//! one pad and 92 ms with three (measured); the export ships three pads, so
//! the whole suite renders at 720p and keeps every target to a second or two.
//!
//! The sources are counter fixtures, so every output frame is checked against
//! the schedule by the number it shows; the sound is checked the same way,
//! against fixtures whose tone is known to the sample.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_pbutils as pbutils;
use uuid::Uuid;
use video_coach_core::audio::audio_regions;
use video_coach_core::avatar::avatar_box;
use video_coach_core::event::{CommentaryEvent, EventKind};
use video_coach_core::export::{compilation_schedule, Compilation, FrameSpec, OUTPUT_FPS};
use video_coach_core::layout::{bar_rect, pip_rect, scoreboard_rects, Rect as LayoutRect};
use video_coach_core::metadata::FileTags;
use video_coach_core::plan::ExportTarget;
use video_coach_core::project::{
    Clip, Inset, Preferences, Project, Quality, Resolution, SourceRef,
};
use video_coach_core::scoreboard::{
    MatchEventKind, MatchFormat, ScoreboardConfig, ScoreboardContext, TeamConfig,
};
use video_coach_core::stroke::{Rgba, Stroke, StrokePoint};
use video_coach_core::zoom::Zoom;
use video_coach_media::fixtures::{
    self, assert_export_tags, block_centre, counter_video, counter_video_with, decode_counters,
    ffprobe, one_entry, read_counter, sample_export_tags, CounterKind, CounterQuirks, COUNTER_BITS,
};
use video_coach_media::{
    ChapterOutcome, ClipMedia, Encode, EntryMedia, ExportDone, ExportError, ExportJob,
    ExportMessage, Exporter, MatchMedia, Render,
};

/// Far beyond any export here, even on a loaded llvmpipe runner; only a hang
/// reaches it.
const TIMEOUT: Duration = Duration::from_secs(120);

/// The suite's output size (see the module comment).
const OUT_W: i32 = 1280;
const OUT_H: i32 = 720;

const BLUE: u32 = 0x0000_00ff;
const GREEN: u32 = 0x0000_ff00;

/// How far a channel may be from the colour that was encoded. I420, the
/// mixer's conversions and H.264 at QP 24 all move it a little, but nowhere
/// near the gap between the colours these tests use.
const TOLERANCE: i32 = 40;

/// A source fixture: 5 s of counter video.
struct Source {
    path: PathBuf,
    fps: u32,
    frames: u32,
}

fn source(dir: &Path, kind: CounterKind) -> Source {
    gst::init().unwrap();
    let (name, fps) = match kind {
        CounterKind::Vp8WebmWithAudio => ("src.webm", 25),
        CounterKind::H264Mp4BFrames | CounterKind::H264AacMp4 => ("src.mp4", 60),
    };
    let frames = 5 * fps;
    let path = counter_video(&dir.join(name), 640, 360, fps, frames, kind);
    Source { path, fps, frames }
}

/// Runs `job` to its `Finished`, calling `on_progress` with the frames pushed
/// so far for each progress message, on the export thread.
fn export_with(
    job: ExportJob,
    mut on_progress: impl FnMut(usize) + Send + 'static,
    with_exporter: impl FnOnce(&Exporter),
) -> Result<ExportDone, ExportError> {
    let frames = job.compilation.frames.len();
    let (tx, rx) = mpsc::channel();
    let begun = Instant::now();
    let exporter = Exporter::start(job, move |msg| match msg {
        ExportMessage::Progress(p) => on_progress(p),
        ExportMessage::Finished(result) => {
            let _ = tx.send(result);
        }
    });
    with_exporter(&exporter);
    let result = rx.recv_timeout(TIMEOUT).expect("the export finished");
    eprintln!(
        "export: {frames} frames in {:.2} s via {:?}",
        begun.elapsed().as_secs_f64(),
        result.as_ref().map(|d| (&d.encoder, &d.diagnostics))
    );
    result
}

fn export(job: ExportJob) -> Result<ExportDone, ExportError> {
    export_with(job, |_| {}, |_| {})
}

fn clip(start: f64, duration: f64, events: Vec<CommentaryEvent>) -> Clip {
    Clip {
        id: Uuid::nil(),
        name: "c".into(),
        notes: String::new(),
        tags: Vec::new(),
        source_index: 0,
        start_source_seconds: start,
        recording_duration: duration,
        recording_filename: "c.mkv".into(),
        events,
        show_pip: false,
        inset: Inset::Camera,
        sort_index: 0,
        created_at: "2026-09-19T00:00:00Z".into(),
        transcript: String::new(),
    }
}

/// One entry's media: the game video its frames come from, its commentary
/// recording and its clip, belonging to a match with no board, no highlights
/// and no avatar — which is every test here but the ones about those.
///
/// A test that needs a match writes `EntryMedia { match_media, ..media(...) }`.
fn media(source: PathBuf, recording: PathBuf, clip: Clip) -> EntryMedia {
    EntryMedia {
        source,
        clip: Some(ClipMedia { recording, clip }),
        match_media: Arc::default(),
    }
}

/// A match whose only property is its avatar image.
fn with_avatar(avatar: PathBuf) -> Arc<MatchMedia> {
    Arc::new(MatchMedia {
        avatar: Some(avatar),
        ..MatchMedia::default()
    })
}

/// A one-entry export of `frames` from `source`, with no picture-in-picture,
/// no text bar and **no audio edit**: the plain picture, which most of these
/// tests are about. An empty edit still writes a full-length silent track.
fn job(source: PathBuf, frames: Vec<FrameSpec>, path: PathBuf) -> ExportJob {
    let clip = clip(0.0, frames.len() as f64 / f64::from(OUTPUT_FPS), Vec::new());
    ExportJob {
        tags: FileTags::default(),
        compilation: one_entry(&clip, frames, ""),
        path,
        cues: None,
        render: Render::Encode(Encode {
            // The recording is unread: `show_pip` is off, so the pad takes the
            // filler.
            entries: vec![media(source, PathBuf::new(), clip)],
            audio: Vec::new(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    }
}

/// Plays, a freeze, skips forward (near and far) and back, with every anchor
/// off frame boundaries at 25 and 60 fps, and identity zoom: 87 frames.
fn steered_clip() -> Clip {
    let event = |t: f64, kind: EventKind| CommentaryEvent::new(t, kind);
    clip(
        1.013,
        2.9,
        vec![
            event(
                0.8,
                EventKind::Pause {
                    source_time: 1.8137,
                },
            ),
            event(
                1.3,
                EventKind::Play {
                    source_time: 1.8137,
                },
            ),
            // Within the 0.5 s pull-ahead: pulled forward.
            event(1.6, EventKind::Skip { delta: 0.3 }),
            // Beyond it: an accurate seek.
            event(1.9, EventKind::Skip { delta: 1.2 }),
            // Backwards: a seek.
            event(2.4, EventKind::Skip { delta: -3.0 }),
        ],
    )
}

/// The frame a correct export shows for `source_time`: the last one with PTS
/// `floor(i·1e9/fps)` at or before `round(source_time·1e9)`, in integers.
fn oracle(source_time: f64, fps: u32, frames: u32) -> u32 {
    let target = (source_time * 1e9).round() as u128;
    let i = ((target + 1) * u128::from(fps) - 1) / 1_000_000_000;
    (i as u32).min(frames - 1)
}

/// Width, height and frame rate of `path`'s video stream, and the file's
/// duration in seconds.
fn shape(path: &Path) -> (u32, u32, gst::Fraction, f64) {
    let uri = gst::glib::filename_to_uri(path, None).unwrap();
    let info = pbutils::Discoverer::new(gst::ClockTime::from_seconds(10))
        .unwrap()
        .discover_uri(&uri)
        .unwrap();
    let video = info
        .video_streams()
        .into_iter()
        .next()
        .expect("a video stream");
    let duration = info.duration().expect("a duration").nseconds() as f64 / 1e9;
    (video.width(), video.height(), video.framerate(), duration)
}

/// The top-level MP4 box types, in file order.
fn top_level_boxes(path: &Path) -> Vec<String> {
    let data = std::fs::read(path).unwrap();
    let mut boxes = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        boxes.push(String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned());
        let size = match size {
            1 => u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) as usize,
            0 => data.len() - at,
            n => n,
        };
        at += size.max(8);
    }
    boxes
}

/// Asserts `got` is `expected` frame for frame, naming the first few that
/// differ.
fn counters_match(got: &[u32], expected: &[u32]) {
    let wrong: Vec<_> = (0..expected.len().max(got.len()))
        .filter(|&n| got.get(n) != expected.get(n))
        .map(|n| (n, got.get(n), expected.get(n)))
        .collect();
    assert!(
        wrong.is_empty(),
        "{} of {} frames wrong (frame, got, expected): {:?}",
        wrong.len(),
        expected.len(),
        &wrong[..wrong.len().min(10)]
    );
}

/// Asserts `path`'s duration is the schedule's, within a frame. `glvideomixer`
/// ignores `identity eos-after=N` and runs to the demuxer's segment end
/// instead, which is how a "600-frame" benchmark produced an unreadable
/// 88.9 s file: the duration is the assertion that catches it.
fn duration_is_the_schedule_s(path: &Path, frames: usize) {
    let (w, h, rate, duration) = shape(path);
    assert_eq!(
        (w, h, rate),
        (OUT_W as u32, OUT_H as u32, gst::Fraction::new(30, 1))
    );
    let expected = frames as f64 / f64::from(OUTPUT_FPS);
    assert!(
        (duration - expected).abs() <= 1.0 / f64::from(OUTPUT_FPS),
        "{duration} s of output for a {expected} s schedule"
    );
    has_one_aac_track(path);
}

/// Every export writes exactly one 48 kHz stereo AAC track, whatever its
/// sources carry. Silence is still a track: a muxer pad that never sees a
/// buffer leaves one it cannot finish, and the duration above is the max of
/// the two tracks, so it only means anything with both of them present.
fn has_one_aac_track(path: &Path) {
    let uri = gst::glib::filename_to_uri(path, None).unwrap();
    let info = pbutils::Discoverer::new(gst::ClockTime::from_seconds(10))
        .unwrap()
        .discover_uri(&uri)
        .unwrap();
    let streams = info.audio_streams();
    assert_eq!(streams.len(), 1, "one audio stream");
    let audio = streams.into_iter().next().expect("checked just above");
    assert_eq!((audio.sample_rate(), audio.channels()), (48_000, 2));
    assert!(audio.bitrate() > 0, "the audio track carries no data");
}

fn round_trip(kind: CounterKind) {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), kind);
    let counters = decode_counters(&src.path);
    assert_eq!(counters, (0..src.frames).collect::<Vec<_>>());
}

#[test]
fn a_vp8_counter_fixture_decodes_to_its_frame_numbers() {
    round_trip(CounterKind::Vp8WebmWithAudio);
}

#[test]
fn an_h264_counter_fixture_decodes_to_its_frame_numbers() {
    round_trip(CounterKind::H264Mp4BFrames);
}

/// The fixture must carry the edit-list trap, or the fiducial test on it
/// proves nothing about stream time.
#[test]
fn the_h264_fixture_has_an_edit_list() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), CounterKind::H264Mp4BFrames);
    let pipeline = gst::parse::launch("filesrc name=in ! qtdemux ! appsink name=sink sync=false")
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline
        .by_name("in")
        .unwrap()
        .set_property("location", &src.path);
    let sink = pipeline
        .by_name("sink")
        .and_downcast::<gst_app::AppSink>()
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let sample = sink
        .try_pull_sample(gst::ClockTime::from_seconds(10))
        .unwrap();
    let _ = pipeline.set_state(gst::State::Null);
    let start = sample
        .segment()
        .and_then(|s| s.downcast_ref::<gst::ClockTime>())
        .and_then(|s| s.start())
        .unwrap();
    assert!(start > gst::ClockTime::ZERO, "segment start {start}");
}

/// The fiducial export: every frame of a steered clip checked against the
/// schedule, **with its sound**, so the file the duration is asserted on is
/// the one the app writes rather than a picture-only variant.
///
/// The VP8 source carries a tone and the H.264 one carries nothing, so between
/// the two this also runs the game track's seeks (a freeze, three skips) and
/// the silent-source path. The recording is silent and shorter than the entry:
/// the commentary track runs out and pads with silence.
fn fiducial(kind: CounterKind) {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), kind);
    let clip = steered_clip();
    let compilation = fixtures::one_clip(&clip, f64::from(src.frames) / f64::from(src.fps));
    assert_eq!(compilation.frames.len(), 87);
    let expected: Vec<u32> = compilation
        .frames
        .iter()
        .map(|f| oracle(f.source_time, src.fps, src.frames))
        .collect();
    let recording =
        fixtures::solid_video(&dir.path().join("rec.webm"), 320, 180, 30, 30, GREEN, true);
    let path = dir.path().join("out.mp4");
    let audio = audio_regions(&compilation, &Preferences::default());
    let done = export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            audio,
            entries: vec![media(src.path.clone(), recording, clip)],
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();
    assert_eq!(done.path, path);

    counters_match(&decode_counters(&path), &expected);
    duration_is_the_schedule_s(&path, expected.len());
    let boxes = top_level_boxes(&path);
    let position = |t: &str| boxes.iter().position(|b| b == t).unwrap();
    assert!(position("moov") < position("mdat"), "boxes: {boxes:?}");
    assert!(!dir.path().join("out.mp4.part").exists());
}

#[test]
fn a_vp8_export_shows_the_scheduled_frame_every_frame() {
    fiducial(CounterKind::Vp8WebmWithAudio);
}

/// Also the edit-list trap: raw PTS would put every frame two frames early.
#[test]
fn an_h264_export_shows_the_scheduled_frame_every_frame() {
    fiducial(CounterKind::H264Mp4BFrames);
}

/// Three clips, one after the other in one file, with a different source
/// size and frame rate at each join: every output frame shows the source frame
/// its entry's schedule asked for, read out of that entry's own rect.
///
/// That is the per-entry geometry, the caps change at the join and the
/// concatenation, in one assertion. It is **not** a reproduction of the
/// four-frames-early race: setting a rect from the pushing thread only lands
/// early while frames are still queued, and this schedule is short enough that
/// a machine keeping up drains between entries (checked: the test still passes
/// with the rect set eagerly). The race is measured in the spec; keying the
/// rect to the buffer's PTS is what removes it, and this test is what says the
/// keying itself is right.
///
/// It is also the only multi-entry run, so it carries the per-entry furniture
/// too: the picture-in-picture on, **off**, on again — the caps change on the
/// PiP pad that used to fail the export outright — a line of its own on each
/// entry's bar, and the real audio edit over all three.
#[test]
fn a_three_clip_export_shows_each_entry_s_frames_in_its_own_rect() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 16:9 at 25 fps, then 4:3 at 50 fps, then 16:9 again: the fit rect and
    // the source's frame rate both change at every join. Both rates divide
    // 1000, since WebM's timecodes are milliseconds and the oracle below
    // counts in nanoseconds.
    let wide = counter_video(
        &dir.path().join("wide.webm"),
        640,
        360,
        25,
        60,
        CounterKind::Vp8WebmWithAudio,
    );
    let narrow = counter_video(
        &dir.path().join("narrow.webm"),
        480,
        360,
        50,
        60,
        CounterKind::Vp8WebmWithAudio,
    );
    // The inset's own video, for the entries that show one. The middle entry's
    // recording is a file that isn't there: `show_pip` is off, so it is never
    // opened, and the pad takes the transparent filler for those frames.
    let webcam = fixtures::solid_video(&dir.path().join("cam.webm"), 640, 360, 30, 60, GREEN, true);
    let recordings = [webcam.clone(), dir.path().join("none.webm"), webcam];

    let per_entry = 18;
    let seconds = f64::from(per_entry) / f64::from(OUTPUT_FPS);
    let clips: Vec<Clip> = [(0, true), (1, false), (0, true)]
        .into_iter()
        .enumerate()
        .map(|(i, (source_index, show_pip))| Clip {
            id: Uuid::new_v4(),
            name: format!("Clip {i}"),
            tags: vec![format!("t{i}")],
            source_index,
            show_pip,
            inset: Inset::Camera,
            ..clip(0.0, seconds, Vec::new())
        })
        .collect();
    // 60 frames at 25 and at 50 fps.
    let compilation = compilation(&clips, &[2.4, 1.2]);
    let frames = compilation.frames.clone();
    assert_eq!(frames.len(), 3 * per_entry as usize);

    let path = dir.path().join("out.mp4");
    let audio = audio_regions(&compilation, &Preferences::default());
    export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            audio,
            entries: clips
                .iter()
                .zip(recordings)
                .map(|(clip, recording)| {
                    let source = [&wide, &narrow][clip.source_index].clone();
                    media(source, recording, clip.clone())
                })
                .collect(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    // Entries 0 and 2 fill the frame; entry 1 is pillarboxed to
    // (160, 0, 960, 720), so its counter has to be read out of that rect. The
    // inset and the bar are clear of every counter block.
    let out = fixtures::decode_gray(&path);
    assert_eq!(
        out.len(),
        frames.len(),
        "one output frame per schedule frame"
    );
    let got: Vec<u32> = out
        .iter()
        .enumerate()
        .map(|(n, frame)| {
            assert_eq!(
                (frame.width, frame.height),
                (OUT_W as usize, OUT_H as usize)
            );
            match n / per_entry as usize {
                1 => read_counter(&frame.crop(160, 0, 960, 720)),
                _ => read_counter(frame),
            }
        })
        .collect();
    let expected: Vec<u32> = frames
        .iter()
        .map(|f| match f.entry {
            1 => oracle(f.source_time, 50, 60),
            _ => oracle(f.source_time, 25, 60),
        })
        .collect();
    counters_match(&got, &expected);

    // The counter check above is the geometry check: entries 0 and 2's frames
    // are read out of the whole frame and entry 1's out of its pillarbox, so a
    // rect that arrived four frames early (the measured race) would make the
    // last frames of entry 0 unreadable. That only means anything if the two
    // rects really do read differently, which this pins.
    let last_of_entry_0 = &out[per_entry as usize - 1];
    assert_ne!(
        read_counter(&last_of_entry_0.crop(160, 0, 960, 720)),
        expected[per_entry as usize - 1],
        "the two entries' rects read the same, so the check above is vacuous"
    );

    // The inset is there for the entries that asked for it and nowhere else.
    let pip = pip_rect(f64::from(OUT_W), f64::from(OUT_H), 16.0 / 9.0);
    let centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    let inset = |entry: usize| {
        out[entry * per_entry as usize + per_entry as usize / 2].mean(centre.0, centre.1, 4)
    };
    for entry in [0, 2] {
        assert!(inset(entry) > 100.0, "no inset on entry {entry}");
    }
    // Entry 1's inset would sit in its pillarbox bar, so with no PiP it is the
    // mixer's black background.
    assert!(inset(1) < 40.0, "an inset on the entry that has none");

    // And each entry's own line is on the bar: white glyphs over a strip that
    // is black in the counter fixture and tinted blacker still by the bar.
    let bar = bar_rect(f64::from(OUT_W), f64::from(OUT_H));
    let glyphs: Vec<usize> = (0..3)
        .map(|entry| {
            let frame = &out[entry * per_entry as usize + per_entry as usize / 2];
            (bar.y as usize..OUT_H as usize)
                .flat_map(|y| (0..640).map(move |x| (x, y)))
                .filter(|&(x, y)| frame.mean(x, y, 0) > 150.0)
                .count()
        })
        .collect();
    assert!(
        glyphs.iter().all(|&n| n > 50),
        "an entry drew no glyphs on its bar: {glyphs:?}"
    );

    duration_is_the_schedule_s(&path, frames.len());
}

/// The compilation a project of `clips` over `source_durations` plans — the
/// app's own path, so the entries carry real segments, real text and the audio
/// edit's input.
fn compilation(clips: &[Clip], source_durations: &[f64]) -> Compilation {
    let mut project = Project::new("p");
    project.source_videos = source_durations
        .iter()
        .map(|&duration_seconds| SourceRef {
            relative_path: "src".into(),
            display_name: "src".into(),
            duration_seconds,
            display_aspect: 16.0 / 9.0,
        })
        .collect();
    project.clips = clips.to_vec();
    compilation_schedule(&project, &ExportTarget::AllClips)
}

/// An export of `times` from `source` shows `expected`.
fn exports_as(source: PathBuf, times: &[f64], expected: &[u32]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.mp4");
    let frames = times
        .iter()
        .map(|&source_time| FrameSpec {
            entry: 0,
            source_time,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    export(job(source, frames, path.clone())).unwrap();
    assert_eq!(decode_counters(&path), expected);
}

/// A seek landing in a gap between frames (VFR, a dropped frame) answers
/// with the frame before it, although that frame's duration ends before the
/// target: an accurate seek drops it.
#[test]
fn a_seek_into_a_gap_shows_the_frame_before_it() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Frame 10 at 0.40 s lasts to 0.44 s; frame 11 is at 0.64 s, 12 at 0.68 s.
    let source = counter_video_with(
        &dir.path().join("src.webm"),
        640,
        360,
        25,
        50,
        CounterKind::Vp8WebmWithAudio,
        CounterQuirks {
            gap: Some((10, 5)),
            audio_tail: 0,
        },
    );
    exports_as(source, &[0.5, 0.5, 0.7], &[10, 10, 12]);
}

/// An anchor exactly on a frame boundary shows that frame, not the one before
/// it. The 30 fps MP4 (timescale 3000, a two-frame edit list: the class of
/// Trace's downloads) puts every third frame 1 ns after its boundary in stream
/// time. A coach types round anchors, so this is the common case. Just off
/// the boundary, the frame is the same.
#[test]
fn a_boundary_anchor_in_an_edit_listed_h264_shows_its_own_frame() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = counter_video(
        &dir.path().join("src.mp4"),
        640,
        360,
        30,
        60,
        CounterKind::H264Mp4BFrames,
    );
    // As the schedule computes them: anchor plus output time, in seconds.
    let on: Vec<f64> = (0..6).map(|n| 1.0 + f64::from(n) / 30.0).collect();
    let off: Vec<f64> = on.iter().map(|t| t + 0.004).collect();
    let expected: Vec<u32> = (30..36).collect();
    exports_as(source.clone(), &on, &expected);
    exports_as(source, &off, &expected);
}

/// Past the video's end, where the audio runs on, the export shows the last
/// frame: an accurate seek there finds no video at all.
#[test]
fn a_seek_past_the_video_shows_its_last_frame() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 2 s of video, 3 s of audio.
    let source = counter_video_with(
        &dir.path().join("src.webm"),
        640,
        360,
        25,
        50,
        CounterKind::Vp8WebmWithAudio,
        CounterQuirks {
            gap: None,
            audio_tail: 25,
        },
    );
    exports_as(source, &[2.5, 2.6], &[49, 49]);
}

/// A 4:3 source is pillarboxed into black bars, and a zoom moves the counter
/// where the fit rect and the zoom mapping predict, clipped to the picture.
#[test]
fn a_4_3_source_is_pillarboxed_and_zoomed_as_predicted() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = counter_video(
        &dir.path().join("src.webm"),
        480,
        360,
        25,
        50,
        CounterKind::Vp8WebmWithAudio,
    );
    // Frame 40 = bits 3 and 5; bit 4 is off.
    let shown = 40;
    let zoom = Zoom::new(2.0, 0.2, -0.2);
    let frames = [Zoom::IDENTITY, zoom, zoom]
        .map(|zoom| FrameSpec {
            entry: 0,
            source_time: 1.6,
            zoom,
        })
        .to_vec();
    let path = dir.path().join("out.mp4");
    export(job(source, frames, path.clone())).unwrap();

    let out = fixtures::decode_gray(&path);
    assert_eq!(out.len(), 3);
    // The fit rect of 4:3 in 1280×720.
    let (fx, fw, fh) = (160.0, 960.0, 720.0);
    for frame in &out {
        assert_eq!(
            (frame.width, frame.height),
            (OUT_W as usize, OUT_H as usize)
        );
        for x in [15, 80, 145, 1135, 1200, 1265] {
            for y in [15, 360, 705] {
                let v = frame.mean(x, y, 8);
                assert!(v < 40.0, "bar pixel ({x}, {y}) is {v}, not black");
            }
        }
    }
    assert_eq!(read_counter(&out[0].crop(160, 0, 960, 720)), shown);

    // Zoomed: the source point (0.5 + pan) sits at the picture's centre, and
    // distances from it scale by s.
    let mut checked = 0;
    for bit in 0..COUNTER_BITS {
        let (u, v) = block_centre(bit);
        let x = fx + fw * (0.5 + (u - 0.5 - zoom.pan_x) * zoom.scale);
        let y = fh * (0.5 + (v - 0.5 - zoom.pan_y) * zoom.scale);
        // Only blocks whose centre is well inside the picture.
        if x < fx + 40.0 || x > fx + fw - 40.0 || !(40.0..fh - 40.0).contains(&y) {
            continue;
        }
        let lit = shown >> bit & 1 == 1;
        let level = out[1].mean(x as usize, y as usize, 6);
        assert_eq!(
            level > 128.0,
            lit,
            "bit {bit} at ({x:.0}, {y:.0}) reads {level:.0}, expected lit={lit}"
        );
        checked += 1;
    }
    assert_eq!(checked, 3, "bits 3, 4 and 5 should be in view");
}

/// A horizontal stroke across the picture at `y`, from `x = 0.2` to `x = 0.8`,
/// logged (as the recorder does) at pen-up.
fn stroke(y: f64, color: Rgba) -> CommentaryEvent {
    let points = [0.2, 0.5, 0.8]
        .into_iter()
        .enumerate()
        .map(|(i, x)| StrokePoint {
            x,
            y,
            t: i as f64 * 0.05,
        })
        .collect();
    CommentaryEvent::new(
        0.05,
        EventKind::Stroke(Stroke {
            id: Uuid::new_v4(),
            color,
            line_width: 0.05,
            points,
            auto_clear_after_seconds: None,
        }),
    )
}

/// The clip the layout test exports: a pillarboxed blue source with two
/// strokes on it, `show_pip` as given, and a line for the bar.
fn laid_out_job(dir: &Path, show_pip: bool) -> (ExportJob, PathBuf) {
    gst::init().unwrap();
    // 4:3, so the picture is pillarboxed to (160, 0, 960, 720) and a stroke
    // rasterized at the output size would land in the wrong place.
    let source = fixtures::solid_video(&dir.join("src.webm"), 640, 480, 30, 30, BLUE, false);
    let recording = fixtures::solid_video(&dir.join("rec.webm"), 640, 360, 30, 30, GREEN, true);
    let translucent = Rgba {
        a: 0.5,
        ..Rgba::RED
    };
    let clip = Clip {
        show_pip,
        inset: Inset::Camera,
        events: vec![stroke(0.5, Rgba::RED), stroke(0.75, translucent)],
        ..clip(0.0, 0.2, Vec::new())
    };
    let frames = (0..6)
        .map(|_| FrameSpec {
            entry: 0,
            source_time: 0.2,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let path = dir.join(format!("out-{show_pip}.mp4"));
    (
        ExportJob {
            tags: FileTags::default(),
            compilation: one_entry(&clip, frames, "1 / 2 | Demo"),
            path: path.clone(),
            cues: None,
            render: Render::Encode(Encode {
                entries: vec![media(source, recording, clip)],
                audio: Vec::new(),
                resolution: Resolution::R720,
                quality: Quality::Medium,
            }),
        },
        path,
    )
}

/// Asserts the pixel at `(x, y)` is `expected` as `0xRRGGBB`, within
/// [`TOLERANCE`].
fn assert_rgb(frame: &fixtures::RgbFrame, what: &str, (x, y): (usize, usize), expected: u32) {
    let actual = frame.at(x, y);
    let want = [
        (expected >> 16) as u8,
        (expected >> 8) as u8,
        expected as u8,
    ];
    let off = (0..3).any(|c| (i32::from(actual[c]) - i32::from(want[c])).abs() > TOLERANCE);
    assert!(
        !off,
        "{what} at ({x}, {y}): expected #{expected:06x}, got {actual:?}"
    );
}

/// The three pads land where `core::layout` says, in z-order: the source
/// pillarboxed at the bottom, the overlay above it — the strokes mapped into
/// the picture, the bar over the whole width — and the PiP on top of both, in
/// the corner, over the bar it lands on.
#[test]
fn the_export_stacks_the_picture_the_pip_and_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let (job, path) = laid_out_job(dir.path(), true);
    export(job).unwrap();
    let out = fixtures::decode_rgb(&path);
    let frame = out.last().expect("frames out");
    assert_eq!(
        (frame.width, frame.height),
        (OUT_W as usize, OUT_H as usize)
    );

    // The 4:3 source is pillarboxed: bars left and right, picture between.
    assert_rgb(frame, "the left bar", (80, 360), 0x000000);
    assert_rgb(frame, "the right bar", (1200, 100), 0x000000);
    assert_rgb(frame, "the picture", (300, 200), BLUE);

    // The PiP is the recording, flush into the bottom-right corner in output
    // space -- overlapping the right pillarbox bar, which is the point of
    // putting it there -- and OVER the text bar rather than on it.
    let pip = pip_rect(f64::from(OUT_W), f64::from(OUT_H), 16.0 / 9.0);
    let bar = bar_rect(f64::from(OUT_W), f64::from(OUT_H));
    assert!(pip.y + pip.h > bar.y, "the PiP is not on the bar");
    let pip_centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    assert_rgb(frame, "the PiP", pip_centre, GREEN);
    assert_rgb(
        frame,
        "just left of the PiP",
        (pip.x as usize - 20, pip_centre.1),
        BLUE,
    );
    // And the bar's tint is under it rather than over it: the inset pad is the
    // top layer, so its bottom row is the recording's own green and not green
    // washed 60% black. This is the assertion that fails if the z-order goes
    // back to the overlay on top.
    assert_rgb(
        frame,
        "the PiP over the bar",
        (pip_centre.0, OUT_H as usize - 8),
        GREEN,
    );

    // The overlay's strokes are mapped into the picture rect, so the middle of
    // a stroke drawn at x = 0.5 is at 160 + 0.5*960, not 0.5*1280.
    assert_rgb(frame, "the stroke", (640, 360), 0xff3333);
    assert_rgb(frame, "past the stroke's end", (1000, 360), BLUE);
    assert_rgb(
        frame,
        "inside the left bar at the stroke's height",
        (80, 360),
        0,
    );

    // Premultiplied-over: red at half alpha over blue keeps half of the red.
    // With the source blend function left at `src-alpha` the red would be
    // halved twice, to about 64.
    let translucent = frame.at(640, 540);
    assert!(
        (100..=160).contains(&i32::from(translucent[0])),
        "the translucent stroke reads {translucent:?}; straight alpha would be ~64"
    );

    // The bar tints the bottom strip and nothing above it, and its glyphs are
    // the only thing in there brighter than the tint.
    let bar_mid = (bar.y + bar.h / 2.0) as usize;
    assert_rgb(frame, "the bar's tint", (900, bar_mid), 0x000066);
    assert_rgb(frame, "just above the bar", (900, bar.y as usize - 8), BLUE);
    let glyphs = (bar.y as usize..OUT_H as usize)
        .flat_map(|y| (0..640).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.at(x, y)[0] > 150)
        .count();
    assert!(glyphs > 50, "only {glyphs} glyph pixels in the bar");
}

/// A pixel inside `cell`, clear of the label centred in it.
fn cell_corner(cell: &LayoutRect) -> (usize, usize) {
    ((cell.x + 4.0) as usize, (cell.y + cell.h - 4.0) as usize)
}

/// A club in its kit, from two `0xRRGGBB` colours.
fn team(name: &str, primary: u32, secondary: u32) -> TeamConfig {
    let rgb = |c: u32| Rgba {
        r: f64::from((c >> 16) as u8) / 255.0,
        g: f64::from((c >> 8) as u8) / 255.0,
        b: f64::from(c as u8) / 255.0,
        a: 1.0,
    };
    TeamConfig::new(name, rgb(primary), rgb(secondary))
}

/// How many pixels inside `cell` are the white its label is drawn in. The
/// count follows the glyphs, so two different scores in the same cell read
/// differently.
fn label_pixels(frame: &fixtures::RgbFrame, cell: &LayoutRect) -> usize {
    ((cell.y as usize)..(cell.y + cell.h) as usize)
        .flat_map(|y| ((cell.x as usize)..(cell.x + cell.w) as usize).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.at(x, y).iter().all(|&c| c > 200))
        .count()
}

/// How many pixels inside `cell` differ between the two frames, beyond
/// [`TOLERANCE`].
fn differing_pixels(a: &fixtures::RgbFrame, b: &fixtures::RgbFrame, cell: &LayoutRect) -> usize {
    ((cell.y as usize)..(cell.y + cell.h) as usize)
        .flat_map(|y| ((cell.x as usize)..(cell.x + cell.w) as usize).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            a.at(x, y)
                .iter()
                .zip(b.at(x, y))
                .any(|(&l, r)| i32::from(l).abs_diff(i32::from(r)) as i32 > TOLERANCE)
        })
        .count()
}

/// A match kicked off at the top of its one `duration`-second source video,
/// with `home_goals` scored a quarter of a second in, played by `board`'s
/// clubs. The names are invented; the footage is a fixture.
fn match_with_board(duration: f64, board: ScoreboardConfig, home_goals: usize) -> Project {
    let mut project = Project::new("m");
    project.source_videos.push(SourceRef {
        relative_path: "src".into(),
        display_name: "src".into(),
        duration_seconds: duration,
        display_aspect: 16.0 / 9.0,
    });
    project.scoreboard = Some(board);
    project.append_match_event(MatchEventKind::StartStop, 0, 0.0);
    for _ in 0..home_goals {
        project.append_match_event(MatchEventKind::HomeGoal, 0, 0.25);
    }
    project
}

/// `project`'s match, as an entry of it carries it: its board and nothing else.
fn with_board(project: &Project) -> Arc<MatchMedia> {
    Arc::new(MatchMedia {
        scoreboard: ScoreboardContext::for_project(project),
        ..MatchMedia::default()
    })
}

/// The `ScoreboardContext` the bus will build reaches the overlay through the
/// export driver, and the board is burned into the file.
///
/// The clock it draws is the **displayed frame's** source time
/// (`state_at(entry.source_index, frame.source_time)`), never a per-clip
/// constant plus the record time — the macOS bug that put the match clock ahead
/// of the footage after every pause (BACKLOG #27). The arithmetic itself is
/// pinned in core; this pins the wiring.
#[test]
fn the_export_burns_in_the_scoreboard() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = fixtures::solid_video(&dir.path().join("src.webm"), 640, 360, 30, 60, BLUE, false);
    let mut project = Project::new("p");
    project.source_videos.push(SourceRef {
        relative_path: "src.webm".into(),
        display_name: "src".into(),
        duration_seconds: 2.0,
        display_aspect: 16.0 / 9.0,
    });
    project.scoreboard = Some(ScoreboardConfig {
        // Magenta at home, not the source's own blue: the board reaches the
        // frame's edge now, so a home cell the colour of the footage would make
        // the checks below unable to tell the two apart.
        home: team("HOME", 0xff00ff, 0xffff00),
        away: team("AWAY", 0xff0000, 0x00ffff),
        format: MatchFormat::default(),
        auto_back_anchor_p1: false,
    });
    // Kick-off at the top of the source, a home goal a quarter-second in, and
    // every exported frame showing 0.5 s: the board reads 1 - 0, running.
    project.append_match_event(MatchEventKind::StartStop, 0, 0.0);
    project.append_match_event(MatchEventKind::HomeGoal, 0, 0.25);

    let clip = clip(0.0, 0.2, Vec::new());
    let frames = (0..6)
        .map(|_| FrameSpec {
            entry: 0,
            source_time: 0.5,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let path = dir.path().join("out.mp4");
    export(ExportJob {
        tags: FileTags::default(),
        compilation: one_entry(&clip, frames, ""),
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            entries: vec![EntryMedia {
                match_media: Arc::new(MatchMedia {
                    scoreboard: ScoreboardContext::for_project(&project),
                    ..MatchMedia::default()
                }),
                ..media(source, PathBuf::new(), clip)
            }],
            audio: Vec::new(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    let out = fixtures::decode_rgb(&path);
    let frame = out.last().expect("frames out");
    let rects = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));
    assert_rgb(frame, "the home cell", cell_corner(&rects.home), 0xff00ff);
    assert_rgb(frame, "the away cell", cell_corner(&rects.away), 0xff0000);
    // The board is locked to the frame's own corner, so a pixel a few in from
    // both edges is the home cell and not the picture -- which is where this
    // used to read the board's margin, the strip the coach asked to lose.
    assert_rgb(frame, "the frame's corner", (2, 8), 0xff00ff);
    // And the board is the only thing up there: the picture below it is the
    // source's own blue.
    assert_rgb(frame, "the picture", (640, 400), BLUE);
    assert_rgb(frame, "under the board", (2, 120), BLUE);
}

/// Two pieces from two matches in one film, and **each draws its own match's
/// board** (spec J3) over its own game video.
///
/// The board is reached through the entry's own `match_media`, so nothing on
/// the way to it can be a per-job value. One board for the run and both halves
/// would read the same score and the same kit; one flat source list indexed by
/// `PlanEntry::source_index` and both halves would read the same file, since
/// each clip is source 0 **of its own project**.
#[test]
fn two_matches_draw_their_own_boards() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 16:9 then 4:3, so the picture is re-letterboxed at the join too.
    let wide = fixtures::solid_video(&dir.path().join("a.webm"), 640, 360, 30, 60, BLUE, false);
    let narrow = fixtures::solid_video(&dir.path().join("b.webm"), 480, 360, 30, 60, GREEN, false);
    // A goal in the first match only, so at the same output time the two
    // boards read 1 - 0 and 0 - 0.
    let first = match_with_board(
        2.0,
        ScoreboardConfig {
            home: team("ROVERS", 0x0000ff, 0xffff00),
            away: team("ATHLETIC", 0xff0000, 0x00ffff),
            format: MatchFormat::default(),
            auto_back_anchor_p1: false,
        },
        1,
    );
    let second = match_with_board(
        2.0,
        ScoreboardConfig {
            home: team("CITY", 0x00ff00, 0xff00ff),
            away: team("UNITED", 0xffff00, 0x0000ff),
            format: MatchFormat::default(),
            auto_back_anchor_p1: false,
        },
        0,
    );

    // Two clips, each source 0 of its own match: the entry's file is the only
    // thing that tells the two apart.
    let clips: Vec<Clip> = (0..2)
        .map(|i| Clip {
            id: Uuid::new_v4(),
            sort_index: i,
            ..clip(0.5, 0.4, Vec::new())
        })
        .collect();
    let compilation = compilation(&clips, &[2.0]);
    let per_entry = compilation.plan.entries[0].frames;
    let total = compilation.frames.len();
    let path = dir.path().join("out.mp4");
    export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            entries: vec![
                EntryMedia {
                    match_media: with_board(&first),
                    ..media(wide, PathBuf::new(), clips[0].clone())
                },
                EntryMedia {
                    match_media: with_board(&second),
                    ..media(narrow, PathBuf::new(), clips[1].clone())
                },
            ],
            audio: Vec::new(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    // One output geometry for the whole film, whatever its pieces came from.
    duration_is_the_schedule_s(&path, total);
    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), total, "one output frame per schedule frame");
    let (a, b) = (&out[per_entry / 2], &out[per_entry + per_entry / 2]);
    let rects = scoreboard_rects(f64::from(OUT_W), f64::from(OUT_H));

    // Each piece's own clubs, in their own kit: the config travels with the
    // state, so the names and the colours redraw at the join.
    assert_rgb(a, "Rovers' cell", cell_corner(&rects.home), 0x0000ff);
    assert_rgb(a, "Athletic's cell", cell_corner(&rects.away), 0xff0000);
    assert_rgb(b, "City's cell", cell_corner(&rects.home), 0x00ff00);
    assert_rgb(b, "United's cell", cell_corner(&rects.away), 0xffff00);

    // And each piece's own score, which is the whole point: "1 - 0" against
    // "0 - 0", in the same cell at the same output time. Counted as the pixels
    // that moved rather than as glyph weight — the label is centred, so a
    // different score shifts most of the cell — and the cell's own fill is
    // opaque, so nothing but the glyphs can move in it.
    let (lit_a, lit_b) = (label_pixels(a, &rects.score), label_pixels(b, &rects.score));
    assert!(
        lit_a > 20 && lit_b > 20,
        "no score drawn: {lit_a} and {lit_b}"
    );
    let moved = differing_pixels(a, b, &rects.score);
    assert!(
        moved > lit_a.min(lit_b) / 2,
        "both pieces drew the same score: {moved} pixels moved, of ~{lit_a} lit"
    );

    // The join re-letterboxes: the 4:3 piece is pillarboxed, so where the
    // first piece's picture reached the frame edge the second has the mixer's
    // black — read below the board, clear of it.
    assert_rgb(a, "the first piece at the frame edge", (40, 600), BLUE);
    assert_rgb(b, "the second piece's pillarbox", (40, 600), 0x000000);
    assert_rgb(b, "the second piece's picture", (640, 600), GREEN);
}

/// The zero-copy diagnostic still names a decoder after a run whose first
/// entry's decoder was closed at the join (spec J2).
///
/// It is read as that decoder is opened rather than after the loop, which is
/// where the bounded cache would have left nothing to read.
#[test]
fn the_diagnostics_survive_a_dropped_first_decoder() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let first = fixtures::solid_video(&dir.path().join("a.webm"), 320, 180, 30, 30, BLUE, false);
    let second = fixtures::solid_video(&dir.path().join("b.webm"), 320, 180, 30, 30, GREEN, false);
    let clips: Vec<Clip> = (0..2)
        .map(|i| Clip {
            id: Uuid::new_v4(),
            sort_index: i,
            ..clip(0.0, 0.2, Vec::new())
        })
        .collect();
    let compilation = compilation(&clips, &[1.0]);
    let done = export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: dir.path().join("out.mp4"),
        cues: None,
        render: Render::Encode(Encode {
            entries: vec![
                media(first, PathBuf::new(), clips[0].clone()),
                media(second, PathBuf::new(), clips[1].clone()),
            ],
            audio: Vec::new(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();
    assert!(
        done.diagnostics.decoder.is_some(),
        "the run reported no decode path: {:?}",
        done.diagnostics
    );
}

/// An entry whose game video isn't there fails the run and leaves nothing
/// behind — no output and no `.part`.
///
/// The entry carries the file rather than an index into a list, so this is the
/// only shape the failure has left.
#[test]
fn an_entry_whose_source_is_missing_fails_and_leaves_no_part() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.mp4");
    let job = job(
        dir.path().join("gone.webm"),
        (0..6)
            .map(|_| FrameSpec {
                entry: 0,
                source_time: 0.0,
                zoom: Zoom::IDENTITY,
            })
            .collect(),
        path.clone(),
    );
    let err = export(job).expect_err("a missing game video exported");
    assert!(matches!(err, ExportError::Failed(_)), "{err:?}");
    assert!(!path.exists(), "a failed export left an output file");
    assert!(
        !dir.path().join("out.mp4.part").exists(),
        "a failed export left its .part"
    );
}

/// Two matches sharing one avatar image both draw it, and a piece whose match
/// has no image takes the filler without stalling the run (spec J5).
///
/// The gate is per entry — its clip's `shows_avatar` **and** its match's image
/// — so the three pads alternate avatar, avatar, filler, which is a caps
/// change on a pad whose caps feature never changes. A system-memory filler
/// would break the next entry's `glupload`.
#[test]
fn two_matches_share_an_avatar_and_a_third_takes_the_filler() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = fixtures::solid_video(&dir.path().join("src.webm"), 640, 360, 30, 60, BLUE, false);
    let avatar = fixtures::solid_png(dir.path(), "avatar.png", 96, 96, RED, 0xff);
    // The same image behind two different matches: one texture, two records.
    let shared = [with_avatar(avatar.clone()), with_avatar(avatar)];
    let clips: Vec<Clip> = (0..3)
        .map(|i| Clip {
            id: Uuid::new_v4(),
            sort_index: i,
            show_pip: true,
            inset: Inset::Avatar,
            ..clip(0.0, 0.3, Vec::new())
        })
        .collect();
    let compilation = compilation(&clips, &[2.0]);
    let per_entry = compilation.plan.entries[0].frames;
    let total = compilation.frames.len();
    let path = dir.path().join("out.mp4");
    export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            entries: clips
                .iter()
                .enumerate()
                .map(|(i, clip)| EntryMedia {
                    // The third match has no avatar at all.
                    match_media: shared.get(i).cloned().unwrap_or_default(),
                    // No recording, so the pulse is flat and the avatar rests.
                    ..media(source.clone(), PathBuf::new(), clip.clone())
                })
                .collect(),
            audio: Vec::new(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), total, "an unfed pad stalled the run");
    let box_rect = avatar_box(pip_rect(f64::from(OUT_W), f64::from(OUT_H), 1.0));
    let centre = (
        (box_rect.x + box_rect.w / 2.0) as usize,
        (box_rect.y + box_rect.h / 2.0) as usize,
    );
    for entry in 0..2 {
        assert_rgb(
            &out[entry * per_entry + per_entry / 2],
            "an avatar match's inset",
            centre,
            RED,
        );
    }
    assert_rgb(
        &out[2 * per_entry + per_entry / 2],
        "the match with no avatar",
        centre,
        BLUE,
    );
}

/// With `show_pip` off the pad takes a 1×1 transparent filler, which is
/// invisible — and, being fed at all, is what keeps the mixer running: an
/// unfed pad produces no output frames whatever (measured).
#[test]
fn show_pip_off_leaves_the_inset_empty_and_the_export_running() {
    let dir = tempfile::tempdir().unwrap();
    let (job, path) = laid_out_job(dir.path(), false);
    let frames = job.compilation.frames.len();
    export(job).unwrap();
    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), frames, "the export produced no frames");
    let frame = out.last().expect("frames out");

    let pip = pip_rect(f64::from(OUT_W), f64::from(OUT_H), 16.0 / 9.0);
    let centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    // The inset's centre falls in the right pillarbox bar, so with no PiP it
    // is the mixer's black background.
    assert_rgb(frame, "where the PiP would be", centre, 0x000000);
    // And the picture is untouched.
    assert_rgb(frame, "the picture", (300, 200), BLUE);
}

const RED: u32 = 0x00ff_0000;

/// How wide the avatar is across the inset's centre row, in pixels.
///
/// The avatar is one flat red and every source here is blue, so the count of
/// red pixels along that row is the drawn circle's diameter — which is the
/// pulse, and nothing else. It scans a little either side of `pip` so a circle
/// that grew past its rect would be counted rather than clipped — to the right
/// only as far as the frame goes, since the inset is flush with its edge.
fn avatar_width(frame: &fixtures::RgbFrame, pip: &LayoutRect) -> usize {
    let y = (pip.y + pip.h / 2.0) as usize;
    let red = |x: usize| {
        let px = frame.at(x, y);
        i32::from(px[0]) - i32::from(px[2]) > 80
    };
    ((pip.x as usize - 16)..((pip.x + pip.w) as usize + 16).min(frame.width))
        .filter(|&x| red(x))
        .count()
}

/// The avatar rides the inset pad and **the commentary sizes it**: the image
/// spans its box — `avatar_box` of the square `pip_rect` — while the coach is
/// talking and settles to `1 / PULSE_GROWTH` of it when they stop (spec E1,
/// E3).
///
/// The recording is loud for its first second and silent for its second, so
/// one export carries both ends of the pulse and the same frames prove it
/// never exceeds that box, which is itself inside the rect a webcam would have
/// had (spec I4, A5).
#[test]
fn an_avatar_clip_pulses_in_the_export() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = fixtures::solid_video(&dir.path().join("src.webm"), 640, 360, 30, 60, BLUE, false);
    // An avatar take's own file has no video track. This one has one and it is
    // never opened: `shows_avatar` takes the pad before the probe would.
    let recording = fixtures::tone_video(
        &dir.path().join("rec.mkv"),
        64,
        64,
        30,
        60,
        fixtures::Tone {
            freq: 440.0,
            amplitude: 0.9,
            window: Some((0.0, 1.0)),
        },
    );
    let avatar = fixtures::solid_png(dir.path(), "avatar.png", 96, 96, RED, 0xff);
    let clip = Clip {
        show_pip: true,
        inset: Inset::Avatar,
        ..clip(0.0, 2.0, Vec::new())
    };
    let frames: Vec<FrameSpec> = (0..60)
        .map(|_| FrameSpec {
            entry: 0,
            source_time: 0.5,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let path = dir.path().join("out.mp4");
    let compilation = one_entry(&clip, frames, "");
    let total = compilation.frames.len();
    export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            entries: vec![EntryMedia {
                match_media: with_avatar(avatar),
                ..media(source, recording, clip)
            }],
            audio: Vec::new(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), total, "one output frame per schedule frame");
    // A square image, so the inset is the square `pip_rect` cut down to the
    // avatar's box, and the circle inscribed in that spans the whole of it at
    // full size.
    let pip = avatar_box(pip_rect(f64::from(OUT_W), f64::from(OUT_H), 1.0));
    // Frame 25 is 0.83 s in, well inside the tone; frame 59 is 0.97 s after
    // it stopped, which is four release constants.
    let loud = avatar_width(&out[25], &pip);
    let quiet = avatar_width(&out[59], &pip);
    let full = pip.w.round() as usize;
    // **The ratio, not the two widths.** Each width is a count of red pixels
    // across a row, and the circle's edge is softened by the encoder's chroma
    // subsampling — by a pixel or two, and by more on CI's llvmpipe than here.
    // The ratio of the same measurement at the two ends is what the pulse
    // actually claims, and it divides that softening out.
    let growth = loud as f64 / quiet as f64;
    assert!(
        (growth - video_coach_core::avatar::PULSE_GROWTH).abs() < 0.04,
        "the avatar grows x{growth:.3} from rest to full, not x{:.3} \
         ({loud} px loud against {quiet} px quiet)",
        video_coach_core::avatar::PULSE_GROWTH
    );
    // And it is centred where a webcam's inset would be, never wider.
    let centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    assert_rgb(&out[25], "the avatar", centre, RED);
    assert_rgb(&out[59], "the resting avatar", centre, RED);
    assert!(loud <= full + 2, "the avatar is wider than its box");
    // And that box is the smaller one the coach asked for, not the webcam's.
    let webcam = pip_rect(f64::from(OUT_W), f64::from(OUT_H), 1.0).w.round() as usize;
    assert!(
        loud < webcam - 2,
        "the avatar is {loud} px across, no smaller than the {webcam} px inset          a camera take would fill"
    );
}

/// One compilation, an avatar entry and then a camera one: the inset pad
/// carries the avatar's texture and then the recording's frames, a caps change
/// on a pad whose caps **feature** never changes (spec I2, E2). A
/// system-memory avatar would break the second entry's `glupload` here.
#[test]
fn an_avatar_and_a_camera_clip_export_together() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = fixtures::solid_video(&dir.path().join("src.webm"), 640, 360, 30, 90, BLUE, false);
    let spoken = fixtures::tone_video(
        &dir.path().join("spoken.mkv"),
        64,
        64,
        30,
        30,
        fixtures::Tone {
            freq: 440.0,
            amplitude: 0.9,
            window: None,
        },
    );
    let webcam = fixtures::solid_video(&dir.path().join("cam.webm"), 640, 360, 30, 30, GREEN, true);
    let avatar = fixtures::solid_png(dir.path(), "avatar.png", 96, 96, RED, 0xff);
    let seconds = 0.6;
    let clips: Vec<Clip> = [Inset::Avatar, Inset::Camera]
        .into_iter()
        .map(|inset| Clip {
            id: Uuid::new_v4(),
            show_pip: true,
            inset,
            ..clip(0.0, seconds, Vec::new())
        })
        .collect();
    let compilation = compilation(&clips, &[3.0]);
    let per_entry = compilation.plan.entries[0].frames;
    let total = compilation.frames.len();
    let path = dir.path().join("out.mp4");
    let audio = audio_regions(&compilation, &Preferences::default());
    // One match behind both entries, as every single-project export has.
    let match_media = with_avatar(avatar);
    export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            audio,
            entries: clips
                .iter()
                .zip([spoken, webcam])
                .map(|(clip, recording)| EntryMedia {
                    match_media: match_media.clone(),
                    ..media(source.clone(), recording, clip.clone())
                })
                .collect(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), total, "one output frame per schedule frame");
    // The avatar's inset is square and the webcam's is 16:9, so each entry's
    // own centre is the honest place to read it.
    let at = |aspect: f64| {
        let pip = pip_rect(f64::from(OUT_W), f64::from(OUT_H), aspect);
        (
            (pip.x + pip.w / 2.0) as usize,
            (pip.y + pip.h / 2.0) as usize,
        )
    };
    assert_rgb(
        &out[per_entry / 2],
        "the avatar entry's inset",
        at(1.0),
        RED,
    );
    assert_rgb(
        &out[per_entry + per_entry / 2],
        "the camera entry's inset",
        at(16.0 / 9.0),
        GREEN,
    );
}

/// Exports 24 frames of a one-entry avatar compilation over a blue source,
/// with `avatar` as the project's image and `recording` as the clip's
/// commentary, and returns the frames it produced.
fn avatar_export(
    dir: &std::path::Path,
    avatar: PathBuf,
    recording: PathBuf,
) -> Vec<fixtures::RgbFrame> {
    gst::init().unwrap();
    let source = fixtures::solid_video(&dir.join("src.webm"), 640, 360, 30, 30, BLUE, false);
    let clip = Clip {
        show_pip: true,
        inset: Inset::Avatar,
        ..clip(0.0, 0.8, Vec::new())
    };
    let frames: Vec<FrameSpec> = (0..24)
        .map(|_| FrameSpec {
            entry: 0,
            source_time: 0.5,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let path = dir.join("out.mp4");
    let compilation = one_entry(&clip, frames, "");
    let total = compilation.frames.len();
    export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            entries: vec![EntryMedia {
                match_media: with_avatar(avatar),
                ..media(source, recording, clip)
            }],
            audio: Vec::new(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();
    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), total, "the export lost frames");
    out
}

/// The avatar image having gone under the project costs the inset and not the
/// run (spec A4): the pad takes the filler, and the export produces its file.
#[test]
fn a_missing_avatar_image_costs_the_inset_not_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let out = avatar_export(
        dir.path(),
        dir.path().join("gone.png"),
        dir.path().join("rec.mkv"),
    );
    let pip = avatar_box(pip_rect(f64::from(OUT_W), f64::from(OUT_H), 1.0));
    let centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    assert_rgb(
        out.last().expect("frames out"),
        "the empty inset",
        centre,
        BLUE,
    );
}

/// The **recording** having gone costs the pulse and not the run (spec D2):
/// there is no sound to read, so every level is 0 and the avatar is drawn at
/// rest, the same size on every frame.
#[test]
fn an_avatar_clip_whose_recording_is_gone_holds_still() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let avatar = fixtures::solid_png(dir.path(), "avatar.png", 96, 96, RED, 0xff);
    let out = avatar_export(dir.path(), avatar, dir.path().join("gone.mkv"));

    let pip = avatar_box(pip_rect(f64::from(OUT_W), f64::from(OUT_H), 1.0));
    let rest = (pip.w / video_coach_core::avatar::PULSE_GROWTH).round() as usize;
    for n in [4, 23] {
        let width = avatar_width(&out[n], &pip);
        assert!(
            width.abs_diff(rest) <= 4,
            "frame {n}: the avatar is {width} px wide, not the resting {rest}"
        );
    }
}

/// A one-clip export of `clip` from `source`, with its sound: the audio edit
/// core derives from the same compilation, at the default volumes.
fn sounded_job(
    clip: Clip,
    source: PathBuf,
    recording: PathBuf,
    source_duration: f64,
    path: PathBuf,
) -> ExportJob {
    let compilation = fixtures::one_clip(&clip, source_duration);
    let audio = audio_regions(&compilation, &Preferences::default());
    ExportJob {
        tags: FileTags::default(),
        compilation,
        path,
        cues: None,
        render: Render::Encode(Encode {
            audio,
            entries: vec![media(source, recording, clip)],
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    }
}

/// The largest absolute value in `window`.
fn peak(window: &[f32]) -> f64 {
    window.iter().fold(0.0, |m, &v| f64::from(v).abs().max(m))
}

/// How much of `freq` is in `samples`: a single-bin DFT at the decode rate,
/// scaled so a pure sine of amplitude `a` reads `a`.
///
/// Two tones at different frequencies is what makes the two tracks tellable
/// apart once they have been mixed into one.
fn tone_level(samples: &[f32], freq: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &s) in samples.iter().enumerate() {
        let angle = std::f64::consts::TAU * freq * i as f64 / 48_000.0;
        re += f64::from(s) * angle.cos();
        im -= f64::from(s) * angle.sin();
    }
    2.0 * re.hypot(im) / samples.len() as f64
}

/// A tone at a known time in the game video comes back out of the file at that
/// time, to within a millisecond.
///
/// That is what dropping the first 1024 samples of the mixed stream buys.
/// Shifting the timestamps instead measurably does nothing — `avenc_aac`
/// re-derives its output times by counting samples from the first buffer — and
/// leaves every sound 21.3 ms late (spec E3).
#[test]
fn a_tone_lands_where_the_picture_does() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 2 s of video, silent but for a 50 ms burst starting at exactly 1.000 s.
    let source = fixtures::tone_video(
        &dir.path().join("src.mkv"),
        640,
        360,
        30,
        60,
        fixtures::Tone {
            freq: 1000.0,
            amplitude: 0.5,
            window: Some((1.0, 1.05)),
        },
    );
    // Silent, and at 44.1 kHz: the commentary is resampled into the mix and
    // contributes nothing to where the burst lands.
    let recording =
        fixtures::solid_video(&dir.path().join("rec.webm"), 320, 180, 30, 60, GREEN, true);
    let path = dir.path().join("out.mp4");
    export(sounded_job(
        clip(0.0, 2.0, Vec::new()),
        source,
        recording,
        2.0,
        path.clone(),
    ))
    .unwrap();

    let samples = fixtures::decode_audio(&path);
    let onset = samples
        .iter()
        .position(|v| v.abs() > 0.1)
        .expect("the burst is somewhere in the file");
    let at = onset as f64 / 48_000.0;
    assert!(
        (at - 1.0).abs() < 0.001,
        "the burst decodes back at {at:.4} s, not 1.000"
    );
}

/// The game track is heard only while the source plays — a freeze is silent —
/// while the commentary runs through the whole entry; and the mix opens on a
/// fade rather than a click.
#[test]
fn the_game_track_is_gated_to_play_and_the_mix_fades_in() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // The two tracks are tellable apart by frequency, so "the game went quiet"
    // is a statement about the game track and not about the total level.
    const GAME: f64 = 1000.0;
    const MIC: f64 = 300.0;
    let tone = |freq, amplitude| fixtures::Tone {
        freq,
        amplitude,
        window: None,
    };
    let source = fixtures::tone_video(
        &dir.path().join("src.mkv"),
        640,
        360,
        30,
        60,
        tone(GAME, 0.6),
    );
    let recording = fixtures::tone_video(
        &dir.path().join("rec.mkv"),
        320,
        180,
        30,
        60,
        tone(MIC, 0.2),
    );
    // Play 0–0.5 s, freeze 0.5–1.0 s on 0.5, play 1.0–1.5 s from 0.5.
    let clip = clip(
        0.0,
        1.5,
        vec![
            CommentaryEvent::new(0.5, EventKind::Pause { source_time: 0.5 }),
            CommentaryEvent::new(1.0, EventKind::Play { source_time: 0.5 }),
        ],
    );
    let path = dir.path().join("out.mp4");
    let job = sounded_job(clip, source, recording, 2.0, path.clone());
    assert_eq!(job.compilation.frames.len(), 45);
    export(job).unwrap();

    let samples = fixtures::decode_audio(&path);
    let window = |from: f64, to: f64| {
        &samples[(from * 48_000.0) as usize..((to * 48_000.0) as usize).min(samples.len())]
    };
    let playing = window(0.15, 0.45);
    let frozen = window(0.6, 0.9);
    assert!(
        tone_level(playing, GAME) > 0.4,
        "the game is only {:.3} loud while it plays",
        tone_level(playing, GAME)
    );
    assert!(
        tone_level(frozen, GAME) < 0.05,
        "the game is still {:.3} loud over a freeze",
        tone_level(frozen, GAME)
    );
    for (what, samples) in [("play", playing), ("the freeze", frozen)] {
        assert!(
            tone_level(samples, MIC) > 0.1,
            "the commentary is only {:.3} loud over {what}",
            tone_level(samples, MIC)
        );
    }

    // The 5 ms fade at the head of both regions: a millisecond in, the mix is
    // a fraction of what it is once the ramp is past. Found from the first
    // audible sample rather than a fixed index, so this says nothing about
    // where the decoder puts the priming.
    let start = samples
        .iter()
        .position(|v| v.abs() > 0.02)
        .expect("the file has sound");
    let early = peak(&samples[start..start + 48]);
    let settled = peak(&samples[start + 240..start + 480]);
    assert!(
        early < 0.3 * settled,
        "the mix opens at {early:.3} against {settled:.3} once settled: no fade"
    );
}

/// An entry with no clip — a goals-reel entry — exports as game video alone:
/// the game's sound, the filler where the inset would be, and every frame the
/// plan counts.
#[test]
fn an_entry_with_no_media_exports_game_audio_only_with_a_filler_pip() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 2 s of dark grey video, silent but for a burst starting at 1.000 s.
    let source = fixtures::tone_video(
        &dir.path().join("src.mkv"),
        640,
        360,
        30,
        60,
        fixtures::Tone {
            freq: 1000.0,
            amplitude: 0.5,
            window: Some((1.0, 1.05)),
        },
    );
    // The plan of a clip over the whole source, with the clip taken away: its
    // segments are what a reel entry's play segment looks like.
    let mut compilation = fixtures::one_clip(&clip(0.0, 2.0, Vec::new()), 2.0);
    compilation.plan.entries[0].clip_id = None;
    let frames = compilation.plan.total_frames();
    let path = dir.path().join("out.mp4");
    let audio = audio_regions(&compilation, &Preferences::default());
    export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            audio,
            entries: vec![EntryMedia {
                source,
                clip: None,
                match_media: Arc::default(),
            }],
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    duration_is_the_schedule_s(&path, frames);
    let out = fixtures::decode_rgb(&path);
    assert_eq!(out.len(), frames);
    let pip = pip_rect(f64::from(OUT_W), f64::from(OUT_H), 16.0 / 9.0);
    let centre = (
        (pip.x + pip.w / 2.0) as usize,
        (pip.y + pip.h / 2.0) as usize,
    );
    assert_rgb(
        out.last().expect("frames out"),
        "where the PiP would be",
        centre,
        0x202020,
    );

    let samples = fixtures::decode_audio(&path);
    let onset = samples
        .iter()
        .position(|v| v.abs() > 0.1)
        .expect("the burst is somewhere in the file");
    let at = onset as f64 / 48_000.0;
    assert!(
        (at - 1.0).abs() < 0.001,
        "the burst decodes back at {at:.4} s, not 1.000"
    );
}

/// A game video with no audio track exports a silent track rather than
/// failing. Footage filmed without sound is still footage, and an audio pad
/// that never sees a buffer leaves the muxer a track it cannot finish.
#[test]
fn a_source_with_no_audio_track_exports_silence() {
    gst::init().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let silent =
        |name: &str| fixtures::solid_video(&dir.path().join(name), 640, 360, 30, 30, BLUE, false);
    let path = dir.path().join("out.mp4");
    export(sounded_job(
        clip(0.0, 0.5, Vec::new()),
        silent("src.webm"),
        silent("rec.webm"),
        1.0,
        path.clone(),
    ))
    .unwrap();

    duration_is_the_schedule_s(&path, 15);
    let samples = fixtures::decode_audio(&path);
    assert!(
        samples.len() >= 15 * 1600,
        "only {} samples for 15 frames of video",
        samples.len()
    );
    assert!(
        peak(&samples) < 0.01,
        "the track peaks at {}",
        peak(&samples)
    );
}

/// Cancelling mid-export leaves no `.part`, and the file already at the path
/// untouched.
#[test]
fn cancel_leaves_nothing_and_keeps_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), CounterKind::Vp8WebmWithAudio);
    let path = dir.path().join("out.mp4");
    let sidecar = dir.path().join("out.srt");
    std::fs::write(&path, b"the previous export").unwrap();
    std::fs::write(&sidecar, b"the previous scoreboard").unwrap();
    let frames = (0..60)
        .map(|n| FrameSpec {
            entry: 0,
            source_time: 1.0 + f64::from(n) / 30.0,
            zoom: Zoom::IDENTITY,
        })
        .collect();

    // The export thread stops in its progress callback until the test has
    // cancelled, so the cancel lands mid-export however fast the machine.
    let (reached_tx, reached_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let mut stopped = false;
    let result = export_with(
        ExportJob {
            tags: FileTags::default(),
            // A target that carries a sidecar, so the cancel is what leaves
            // the old one alone rather than the job never having one.
            cues: Some(Vec::new()),
            ..job(src.path, frames, path.clone())
        },
        move |frames_done| {
            // A third of the 60 frames.
            if frames_done >= 20 && !stopped {
                stopped = true;
                let _ = reached_tx.send(frames_done);
                let _ = go_rx.recv_timeout(TIMEOUT);
            }
        },
        |exporter| {
            // An export that fails first is reported by the assert below.
            if reached_rx.recv_timeout(TIMEOUT).is_ok() {
                exporter.cancel();
                let _ = go_tx.send(());
            }
        },
    );
    assert_eq!(result, Err(ExportError::Cancelled));
    assert!(!dir.path().join("out.mp4.part").exists());
    assert_eq!(std::fs::read(&path).unwrap(), b"the previous export");
    // The sidecar is written after the rename, so a cancel neither writes one
    // nor takes the last good export's away.
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"the previous scoreboard");
}

/// `path`'s chapters as `ffprobe` reads them: `(start in seconds, title)`.
///
/// The independent reader, because GStreamer's `qtdemux` doesn't read `chpl`
/// and no released Rust MP4 crate parses it.
fn ffprobe_chapters(path: &Path) -> Vec<(f64, String)> {
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

/// A compilation gets a chapter per entry, titled with its bar line and
/// starting on its first output frame, in a file that still decodes whole.
///
/// The entries are 0.51 s long, so each takes 16 frames rather than 15.3:
/// a chapter placed by summing durations would drift by most of a frame per
/// entry, well past the millisecond this allows.
///
/// **It also carries an empty cue slot, and so clears the sidecar** — the
/// encoded path's half of the rule, and the one a coach hits by switching the
/// scoreboard picker back to burned in and exporting over the same file.
#[test]
fn a_compilation_gets_a_chapter_per_entry() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), CounterKind::H264Mp4BFrames);
    let clips: Vec<Clip> = ["Build-up", "Café press", "Finish"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| Clip {
            name: name.into(),
            sort_index: i as i64,
            ..clip(i as f64, 0.51, Vec::new())
        })
        .collect();
    let compilation = compilation(&clips, &[f64::from(src.frames) / f64::from(src.fps)]);
    let expected_counters: Vec<u32> = compilation
        .frames
        .iter()
        .map(|f| oracle(f.source_time, src.fps, src.frames))
        .collect();
    let expected: Vec<(f64, String)> = compilation.plan.chapters.clone();
    assert_eq!(expected.len(), 3);
    assert_eq!(compilation.plan.entries[1].start_frame, 16);
    let path = dir.path().join("out.mp4");
    let sidecar = dir.path().join("out.srt");
    std::fs::write(&sidecar, "1\n00:00:00,000 --> 00:00:01,000\nold score\n\n").unwrap();
    // Three chapters half a second apart: a list YouTube would ignore, so
    // none is written and the one left by an earlier run goes.
    let chapter_list = dir.path().join("out.chapters.txt");
    std::fs::write(&chapter_list, "0:00 an older cut\n").unwrap();
    let done = export(ExportJob {
        tags: FileTags::default(),
        compilation,
        path: path.clone(),
        cues: Some(Vec::new()),
        render: Render::Encode(Encode {
            // A silent track, which is still `avenc_aac`'s.
            audio: Vec::new(),
            entries: clips
                .into_iter()
                // The recording is unread: `show_pip` is off and there is no
                // audio edit.
                .map(|clip| media(src.path.clone(), PathBuf::new(), clip))
                .collect(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();
    assert_eq!(done.chapters, ChapterOutcome::Written(3));
    assert!(
        !sidecar.exists(),
        "a burned export left the old scoreboard beside it"
    );
    assert_eq!(done.chapter_list, None);
    assert!(
        !chapter_list.exists(),
        "an export with no pasteable list left the old one beside it"
    );

    let got = ffprobe_chapters(&path);
    assert_eq!(got.len(), expected.len(), "chapters read back: {got:?}");
    for ((at, title), (want_at, want_title)) in got.iter().zip(&expected) {
        assert_eq!(title, want_title);
        assert!(
            (at - want_at).abs() < 0.001,
            "{title:?} starts at {at}, not {want_at}"
        );
    }
    counters_match(&decode_counters(&path), &expected_counters);
    duration_is_the_schedule_s(&path, expected_counters.len());
}

/// The encoded export tags its file, and chaptering it afterwards still works.
///
/// **The two share the `moov`**, which is why they are checked together: the
/// tags go into `moov/udta` when the muxer lays the reserved header out, and
/// `chapters::splice` then appends `chpl` to that same `udta` out of the
/// `free` box behind it. Measured, the tags cost that `free` box nothing —
/// `mp4mux` grows the reserved header to fit them — and this is the standing
/// proof of it.
#[test]
fn an_encoded_export_tags_its_file_and_still_chapters_it() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), CounterKind::H264Mp4BFrames);
    let clips: Vec<Clip> = ["Build-up", "Finish"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| Clip {
            name: name.into(),
            sort_index: i as i64,
            ..clip(i as f64, 0.51, Vec::new())
        })
        .collect();
    let compilation = compilation(&clips, &[f64::from(src.frames) / f64::from(src.fps)]);
    let expected: Vec<(f64, String)> = compilation.plan.chapters.clone();
    assert_eq!(expected.len(), 2);

    let path = dir.path().join("out.mp4");
    let done = export(ExportJob {
        tags: sample_export_tags(),
        compilation,
        path: path.clone(),
        cues: None,
        render: Render::Encode(Encode {
            audio: Vec::new(),
            entries: clips
                .into_iter()
                .map(|clip| media(src.path.clone(), PathBuf::new(), clip))
                .collect(),
            resolution: Resolution::R720,
            quality: Quality::Medium,
        }),
    })
    .unwrap();

    assert_export_tags(&path);
    assert_eq!(done.chapters, ChapterOutcome::Written(2));
    assert!(
        done.reserve_remaining > 0.0,
        "the tags ate the moov reserve"
    );
    let got = ffprobe_chapters(&path);
    assert_eq!(got.len(), expected.len(), "chapters read back: {got:?}");
    for ((at, title), (want_at, want_title)) in got.iter().zip(&expected) {
        assert_eq!(title, want_title);
        assert!(
            (at - want_at).abs() < 0.001,
            "{title:?} starts at {at}, not {want_at}"
        );
    }
}

/// The pasteable chapter list lands beside the encoded file, in the form a
/// YouTube description parses.
///
/// **The chapters are set by hand and the film is a second long.** The list
/// is a pure function of `plan.chapters`
/// (`video_coach_core::chapters::chapter_list`), so what matters here is that
/// media writes what core returns, at the right path, beside the video —
/// rendering half an hour of footage to space three chapters ten seconds
/// apart would buy the test nothing.
#[test]
fn a_chapter_list_lands_beside_the_encode() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path(), CounterKind::H264Mp4BFrames);
    let path = dir.path().join("out.mp4");
    let frames = (0..30)
        .map(|n| FrameSpec {
            entry: 0,
            source_time: f64::from(n) / 30.0,
            zoom: Zoom::IDENTITY,
        })
        .collect();
    let mut job = job(src.path.clone(), frames, path.clone());
    job.compilation.plan.chapters = vec![
        (0.0, "Kick-off".into()),
        (845.4, "Rovers goal 1-0".into()),
        (1651.9, "Half time".into()),
    ];
    let done = export(job).unwrap();

    let chapter_list = dir.path().join("out.chapters.txt");
    assert_eq!(done.chapter_list, Some(chapter_list.clone()));
    assert_eq!(
        std::fs::read_to_string(&chapter_list).unwrap(),
        "0:00 Kick-off\n14:05 Rovers goal 1-0\n27:31 Half time\n"
    );
    // Beside the video it belongs to, and nowhere near the `.srt`'s name.
    assert!(path.exists());
    assert!(!dir.path().join("out.srt").exists());
}
