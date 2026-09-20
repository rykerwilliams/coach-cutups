//! Synthetic media files for tests, and the one-entry compilation that drives
//! an export of them.
//!
//! Every function writes into a directory the caller supplies, so the caller
//! owns cleanup (normally a `tempfile::TempDir`) and this crate carries no
//! test-only dependency. The pipelines use the GStreamer base, good and ugly
//! (`x264enc`) plugin sets; reading an export back also needs libav
//! (`avdec_aac`, and `avdec_h264` where it outranks `openh264dec`). CI
//! installs all of them.
//!
//! These are test helpers, so they **panic** on any failure — a pipeline that
//! fails to build, posts an `ERROR`, or does not reach EOS within
//! `TIMEOUT`. A fixture that silently came out short would make every
//! assertion built on it meaningless.

use std::path::{Path, PathBuf};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use video_coach_core::export::{compilation_schedule, Compilation, FrameSpec, OUTPUT_FPS};
use video_coach_core::plan::{CompilationPlan, ExportTarget, PlanEntry};
use video_coach_core::project::{Clip, Project, SourceRef};

/// How long a fixture pipeline may run before it is declared hung.
const TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(30);

/// Audio sample rate for fixtures that carry audio.
const AUDIO_RATE: u32 = 44_100;

/// Sample rate of a [`tone_video`]: the export's own mixing rate, so the tone
/// is neither resampled on its way in nor on its way out and its onset can be
/// read back to the sample.
const TONE_RATE: u32 = 48_000;

/// A one-entry compilation of `frames` for `clip`, with `text` on the bar: the
/// plan `compilation_schedule` builds for a single-clip target.
///
/// The media tests drive the export from frame lists no project can express —
/// a seek into a gap, a zoom held over three frames, a deliberate mid-stream
/// error — so they assemble the entry rather than going through a project.
/// `segments` is left empty: it is the audio edit's input (Phase 8 Task 4),
/// not the picture's.
pub fn one_entry(clip: &Clip, frames: Vec<FrameSpec>, text: &str) -> Compilation {
    let count = frames.len();
    Compilation {
        plan: CompilationPlan {
            total_duration_seconds: count as f64 / f64::from(OUTPUT_FPS),
            entries: vec![PlanEntry {
                clip_id: clip.id,
                source_index: clip.source_index,
                recording_filename: clip.recording_filename.clone(),
                show_pip: clip.show_pip,
                segments: Vec::new(),
                recording_duration: clip.recording_duration,
                start_frame: 0,
                frames: count,
                text: text.to_owned(),
            }],
        },
        frames,
    }
}

/// A VP8 + Vorbis WebM of `secs` seconds at `w`×`h`, `fps` frames per second,
/// with a keyframe at least every `keyint` frames. Written to `dir/name`.
///
/// The audio is sized to exactly the video duration — one audio buffer of
/// `AUDIO_RATE / fps` samples per video frame. Left at `audiotestsrc`'s
/// default buffer size, the audio track runs long and the container duration
/// with it.
///
/// Panics if `fps` does not divide 44 100 (e.g. 25, 30 and 60 do).
pub fn webm(dir: &Path, name: &str, secs: u32, w: u32, h: u32, fps: u32, keyint: u32) -> PathBuf {
    assert!(
        fps > 0 && AUDIO_RATE.is_multiple_of(fps),
        "fps {fps} must divide {AUDIO_RATE} so the audio matches the video duration"
    );
    let frames = secs * fps;
    let samples_per_buffer = AUDIO_RATE / fps;
    let pipeline = format!(
        "videotestsrc num-buffers={frames} \
           ! video/x-raw,width={w},height={h},framerate={fps}/1 \
           ! vp8enc deadline=1 keyframe-max-dist={keyint} ! queue ! mux. \
         audiotestsrc num-buffers={frames} samplesperbuffer={samples_per_buffer} \
           ! audio/x-raw,rate={AUDIO_RATE},channels=1 \
           ! audioconvert ! vorbisenc ! queue ! mux. \
         webmmux name=mux ! filesink name=out"
    );
    run(&pipeline, &dir.join(name))
}

/// A `frames`-frame VP8 WebM of one flat colour at `path`, `w`×`h` at `fps`,
/// with `rgb` as `0xRRGGBB`.
///
/// `with_audio` adds a **silent** Vorbis track sized to the video, as the
/// commentary recordings the preview plays natively have: silent so a test
/// that reaches a real sound card is inaudible, and sized to the video so the
/// container's duration is the video's.
///
/// A flat colour is what the composite tests measure geometry against: every
/// pixel of a pad's rect is the same value, so a rect's edges are the only
/// thing the assertions can be reading.
pub fn solid_video(
    path: &Path,
    w: u32,
    h: u32,
    fps: u32,
    frames: u32,
    rgb: u32,
    with_audio: bool,
) -> PathBuf {
    let audio = if with_audio {
        assert!(
            fps > 0 && AUDIO_RATE.is_multiple_of(fps),
            "fps {fps} must divide {AUDIO_RATE} so the audio matches the video duration"
        );
        format!(
            "audiotestsrc wave=silence num-buffers={frames} samplesperbuffer={} \
               ! audio/x-raw,rate={AUDIO_RATE},channels=1 \
               ! audioconvert ! vorbisenc ! queue ! mux. ",
            AUDIO_RATE / fps
        )
    } else {
        String::new()
    };
    run(
        &format!(
            "videotestsrc num-buffers={frames} pattern=solid-color \
               foreground-color=0x{:08x} \
               ! video/x-raw,format=I420,width={w},height={h},framerate={fps}/1 \
               ! vp8enc deadline=1 keyframe-max-dist={fps} ! queue ! mux. \
             {audio}webmmux name=mux ! filesink name=out",
            0xff00_0000u32 | (rgb & 0x00ff_ffff)
        ),
        path,
    )
}

/// A one-second 320×180 MJPEG MP4 at `dir/rotated.mp4` whose global tag list
/// carries `image-orientation=rotate-90`, as a phone writes for portrait
/// footage.
///
/// MP4 rather than WebM because WebM cannot carry `image-orientation`.
pub fn rotated_mp4(dir: &Path) -> PathBuf {
    run(
        "videotestsrc num-buffers=30 \
           ! video/x-raw,width=320,height=180,framerate=30/1 \
           ! jpegenc \
           ! taginject scope=global tags=\"image-orientation=rotate-90\" \
           ! qtmux ! filesink name=out",
        &dir.join("rotated.mp4"),
    )
}

/// The sound of a [`tone_video`].
#[derive(Debug, Clone, Copy)]
pub struct Tone {
    pub freq: f64,
    pub amplitude: f64,
    /// `(from, to)` in seconds: the tone sounds only in there, and the track
    /// is silent outside it. `None` sounds for the whole file.
    pub window: Option<(f64, f64)>,
}

/// A `frames`-frame video at `path`, `w`×`h` at `fps`, whose sound is `tone`.
///
/// The audio is **raw F32 in Matroska** at [`TONE_RATE`], not a codec: a lossy
/// one smears a burst's onset by up to its window (Vorbis: ~21 ms), which is
/// the very quantity the priming test measures. One buffer per video frame, so
/// the two tracks are the same length.
///
/// Panics if `fps` does not divide [`TONE_RATE`] (25, 30 and 60 do).
pub fn tone_video(path: &Path, w: u32, h: u32, fps: u32, frames: u32, tone: Tone) -> PathBuf {
    assert!(
        fps > 0 && TONE_RATE.is_multiple_of(fps),
        "fps {fps} must divide {TONE_RATE} so the tone matches the video duration"
    );
    let per_frame = (TONE_RATE / fps) as usize;
    let description = format!(
        "videotestsrc num-buffers={frames} pattern=solid-color foreground-color=0xff202020 \
           ! video/x-raw,format=I420,width={w},height={h},framerate={fps}/1 \
           ! vp8enc deadline=1 keyframe-max-dist={fps} ! queue ! mux.video_0 \
         appsrc name=src format=time \
           caps=audio/x-raw,format=F32LE,rate={TONE_RATE},channels=1,layout=interleaved \
           ! queue ! mux.audio_0 \
         matroskamux name=mux ! filesink name=out"
    );
    run_with(&description, path, |pipeline| {
        let src = pipeline
            .by_name("src")
            .and_downcast::<gst_app::AppSrc>()
            .expect("tone pipeline has an appsrc named `src`");
        let mut next = 0u32;
        src.set_callbacks(
            gst_app::AppSrcCallbacks::builder()
                .need_data(move |src, _| {
                    if next == frames {
                        let _ = src.end_of_stream();
                        return;
                    }
                    let first = next as usize * per_frame;
                    let data: Vec<u8> = (first..first + per_frame)
                        .flat_map(|i| {
                            let t = i as f64 / f64::from(TONE_RATE);
                            let on = tone.window.is_none_or(|(a, b)| t >= a && t < b);
                            let v = match on {
                                true => {
                                    tone.amplitude * (std::f64::consts::TAU * tone.freq * t).sin()
                                }
                                false => 0.0,
                            };
                            (v as f32).to_le_bytes()
                        })
                        .collect();
                    let mut buffer = gst::Buffer::from_mut_slice(data);
                    {
                        let at = |i: usize| {
                            gst::ClockTime::SECOND
                                .mul_div_floor(i as u64, u64::from(TONE_RATE))
                                .expect("no overflow")
                        };
                        let buffer = buffer.get_mut().expect("a new buffer is writable");
                        buffer.set_pts(at(first));
                        buffer.set_duration(at(first + per_frame) - at(first));
                    }
                    let _ = src.push_buffer(buffer);
                    next += 1;
                })
                .build(),
        );
    })
}

/// A one-second Vorbis-in-Ogg file at `dir/audio_only.ogg` with no video
/// stream.
pub fn audio_only(dir: &Path) -> PathBuf {
    run(
        "audiotestsrc num-buffers=30 samplesperbuffer=1470 \
           ! audio/x-raw,rate=44100,channels=1 \
           ! audioconvert ! vorbisenc ! oggmux ! filesink name=out",
        &dir.join("audio_only.ogg"),
    )
}

/// The container and codecs of a [`counter_video`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterKind {
    /// VP8 + Vorbis in WebM, like [`webm`]: millisecond timecodes, and an
    /// audio pad the export leaves unlinked.
    Vp8WebmWithAudio,
    /// H.264 with B-frames in MP4. `x264enc bframes=2 ! mp4mux` writes an
    /// edit list, so `qtdemux`'s segment starts after 0 and raw PTS runs
    /// ahead of stream time: the trap that makes export use stream time.
    H264Mp4BFrames,
}

/// Bits in a counter: 2 rows of [`COUNTER_COLUMNS`] blocks, up to frame 4095.
pub const COUNTER_BITS: u32 = 12;
/// The picture is a grid of this many columns and [`COUNTER_GRID_ROWS`]
/// rows; bit `b` sits in column `b % 6` of grid row `1 + b / 6`.
const COUNTER_COLUMNS: u32 = 6;
const COUNTER_GRID_ROWS: u32 = 4;
/// A block's size as a fraction of its grid cell.
const COUNTER_FILL: f64 = 0.6;
/// Video-range black and white, the levels the blocks are drawn in.
const BLACK: u8 = 16;
const WHITE: u8 = 235;

/// Centre of counter bit `bit`'s block, as a fraction of the picture's width
/// and height.
pub fn block_centre(bit: u32) -> (f64, f64) {
    let (row, col) = (1 + bit / COUNTER_COLUMNS, bit % COUNTER_COLUMNS);
    (
        (f64::from(col) + 0.5) / f64::from(COUNTER_COLUMNS),
        (f64::from(row) + 0.5) / f64::from(COUNTER_GRID_ROWS),
    )
}

/// A one-clip compilation built the way the app builds one: through a project
/// whose only source video runs `source_duration` seconds.
///
/// [`one_entry`] can't stand in here: its frame lists are synthetic, so it
/// leaves the entry's play/freeze segments empty, and those segments **are**
/// the game track's audio edit. Its entry's text is the real one too —
/// `1 / 1 | <name> | tags`, which is what preview's bar draws (spec E7).
pub fn one_clip(clip: &Clip, source_duration: f64) -> Compilation {
    let mut project = Project::new("p");
    project.source_videos.push(SourceRef {
        relative_path: "src".into(),
        display_name: "src".into(),
        duration_seconds: source_duration,
        display_aspect: 16.0 / 9.0,
    });
    project.clips = vec![clip.clone()];
    compilation_schedule(&project, &ExportTarget::AllClips)
}

/// A `frames`-frame video at `path`, `w`×`h` at `fps`, whose frame `i` shows
/// `i` as a binary counter of black and white blocks (see [`block_centre`]).
/// Frame `i` has PTS `floor(i·1e9/fps)`, pushed through `appsrc`.
///
/// The blocks are at least 32 px, so they survive VP8, x264, `openh264dec`,
/// and the export graph's scaling on llvmpipe with no bad frames. Panics if
/// `w`×`h` is too small for that, or `fps` doesn't divide 44 100 (the audio of
/// [`CounterKind::Vp8WebmWithAudio`] is sized to the video, as in [`webm`]).
pub fn counter_video(
    path: &Path,
    w: u32,
    h: u32,
    fps: u32,
    frames: u32,
    kind: CounterKind,
) -> PathBuf {
    counter_video_with(path, w, h, fps, frames, kind, CounterQuirks::default())
}

/// Irregular timing for a [`counter_video_with`], for export's edge cases.
#[derive(Debug, Clone, Copy, Default)]
pub struct CounterQuirks {
    /// `(after, slots)`: frames after frame `after` are pushed `slots` frame
    /// intervals late, so frame `after`'s duration doesn't reach the next
    /// frame's PTS.
    pub gap: Option<(u32, u32)>,
    /// Frame intervals of audio past the video's end
    /// ([`CounterKind::Vp8WebmWithAudio`] only).
    pub audio_tail: u32,
}

/// [`counter_video`] with `quirks`. Frame `i` still shows `i`.
pub fn counter_video_with(
    path: &Path,
    w: u32,
    h: u32,
    fps: u32,
    frames: u32,
    kind: CounterKind,
    quirks: CounterQuirks,
) -> PathBuf {
    let (block_w, block_h) = block_size(w, h);
    assert!(
        block_w >= 32 && block_h >= 32 && w.is_multiple_of(8) && h.is_multiple_of(2),
        "{w}x{h} gives {block_w}x{block_h} px counter blocks; they must be >= 32 px"
    );
    assert!(
        frames <= 1 << COUNTER_BITS,
        "{frames} frames overflow the counter"
    );
    let caps = format!("video/x-raw,format=I420,width={w},height={h},framerate={fps}/1");
    let gap_slots = quirks.gap.map_or(0, |(_, slots)| slots);
    // The frame interval frame `i` is pushed at.
    let slot = move |i: u32| match quirks.gap {
        Some((after, slots)) if i > after => i + slots,
        _ => i,
    };
    let description = match kind {
        CounterKind::Vp8WebmWithAudio => {
            assert!(
                fps > 0 && AUDIO_RATE.is_multiple_of(fps),
                "fps {fps} must divide {AUDIO_RATE} so the audio matches the video duration"
            );
            let samples_per_buffer = AUDIO_RATE / fps;
            let audio_buffers = frames + gap_slots + quirks.audio_tail;
            format!(
                "appsrc name=src format=time caps={caps} \
                   ! vp8enc deadline=1 keyframe-max-dist={fps} ! queue ! mux. \
                 audiotestsrc num-buffers={audio_buffers} samplesperbuffer={samples_per_buffer} \
                   ! audio/x-raw,rate={AUDIO_RATE},channels=1 \
                   ! audioconvert ! vorbisenc ! queue ! mux. \
                 webmmux name=mux ! filesink name=out"
            )
        }
        CounterKind::H264Mp4BFrames => {
            assert_eq!(quirks.audio_tail, 0, "the H.264 counter has no audio");
            format!(
                "appsrc name=src format=time caps={caps} \
               ! x264enc bframes=2 key-int-max={fps} ! mp4mux ! filesink name=out"
            )
        }
    };
    run_with(&description, path, |pipeline| {
        let src = pipeline
            .by_name("src")
            .and_downcast::<gst_app::AppSrc>()
            .expect("counter pipeline has an appsrc named `src`");
        let mut next = 0u32;
        src.set_callbacks(
            gst_app::AppSrcCallbacks::builder()
                .need_data(move |src, _| {
                    if next == frames {
                        let _ = src.end_of_stream();
                        return;
                    }
                    let mut buffer = gst::Buffer::from_mut_slice(counter_frame(w, h, next));
                    let at =
                        |i: u32| gst::ClockTime::SECOND.mul_div_floor(u64::from(i), u64::from(fps));
                    {
                        let buffer = buffer.get_mut().expect("a new buffer is writable");
                        let slot = slot(next);
                        buffer.set_pts(at(slot));
                        buffer.set_duration(at(slot + 1).zip(at(slot)).map(|(b, a)| b - a));
                    }
                    let _ = src.push_buffer(buffer);
                    next += 1;
                })
                .build(),
        );
    })
}

/// A counter block's size in pixels at `w`×`h`.
fn block_size(w: u32, h: u32) -> (u32, u32) {
    let size =
        |extent: u32, cells: u32| (f64::from(extent) / f64::from(cells) * COUNTER_FILL) as u32;
    (size(w, COUNTER_COLUMNS), size(h, COUNTER_GRID_ROWS))
}

/// One I420 frame showing `n`.
fn counter_frame(w: u32, h: u32, n: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut data = vec![BLACK; w * h];
    let (block_w, block_h) = block_size(w as u32, h as u32);
    for bit in (0..COUNTER_BITS).filter(|b| n >> b & 1 == 1) {
        let (cx, cy) = block_centre(bit);
        let x0 = (cx * w as f64) as usize - block_w as usize / 2;
        let y0 = (cy * h as f64) as usize - block_h as usize / 2;
        for row in data.chunks_exact_mut(w).skip(y0).take(block_h as usize) {
            row[x0..x0 + block_w as usize].fill(WHITE);
        }
    }
    // Neutral chroma: two quarter-size planes.
    data.resize(w * h * 3 / 2, 128);
    data
}

/// A decoded frame's luma, tightly packed.
#[derive(Debug, Clone)]
pub struct GrayFrame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl GrayFrame {
    /// Mean luma of the square of side `2·r + 1` around `(x, y)`, clipped to
    /// the frame.
    pub fn mean(&self, x: usize, y: usize, r: usize) -> f64 {
        let xs = x.saturating_sub(r)..(x + r + 1).min(self.width);
        let ys = y.saturating_sub(r)..(y + r + 1).min(self.height);
        let count = xs.len() * ys.len();
        let sum: u64 = ys
            .flat_map(|y| self.data[y * self.width..][xs.clone()].iter())
            .map(|&v| u64::from(v))
            .sum();
        sum as f64 / count as f64
    }

    /// The `w`×`h` rectangle at `(x, y)`.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> GrayFrame {
        let data = (y..y + h)
            .flat_map(|row| self.data[row * self.width + x..][..w].iter().copied())
            .collect();
        GrayFrame {
            width: w,
            height: h,
            data,
        }
    }
}

/// The number a [`counter_video`] frame shows, read by thresholding each
/// block's centre. The counter must fill `frame` (crop a letterboxed one to
/// its picture first).
pub fn read_counter(frame: &GrayFrame) -> u32 {
    let r = (frame.width.min(frame.height) / 60).max(2);
    (0..COUNTER_BITS)
        .filter(|&bit| {
            let (cx, cy) = block_centre(bit);
            let x = (cx * frame.width as f64) as usize;
            let y = (cy * frame.height as f64) as usize;
            frame.mean(x, y, r) > 128.0
        })
        .fold(0, |n, bit| n | 1 << bit)
}

/// Every frame of `path`'s video stream, in order, as read by
/// [`read_counter`].
pub fn decode_counters(path: &Path) -> Vec<u32> {
    let mut counters = Vec::new();
    for_each_gray(path, |frame| counters.push(read_counter(&frame)));
    counters
}

/// Every frame of `path`'s video stream, in order. Holds them all: for short
/// files.
pub fn decode_gray(path: &Path) -> Vec<GrayFrame> {
    let mut frames = Vec::new();
    for_each_gray(path, |frame| frames.push(frame));
    frames
}

/// A decoded frame's colour, tightly packed RGB.
///
/// The composite's own assertions need colour, not luma: a premultiplied
/// blend and a straight-alpha one differ by which channel is halved twice.
#[derive(Debug, Clone)]
pub struct RgbFrame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl RgbFrame {
    pub fn at(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        self.data[i..i + 3].try_into().expect("three channels")
    }
}

/// Every frame of `path`'s video stream, in order, as RGB. Holds them all:
/// for short files.
pub fn decode_rgb(path: &Path) -> Vec<RgbFrame> {
    let mut frames = Vec::new();
    for_each_frame(path, "RGB", 3, |width, height, data| {
        frames.push(RgbFrame {
            width,
            height,
            data,
        })
    });
    frames
}

/// Every audio sample of `path`, mixed down to mono at the export's own rate.
/// Holds them all: for short files.
///
/// Mono because what the assertions ask of a mixed track — where a tone
/// starts, how loud it is, whether it is silent — is the same in both
/// channels, and one channel of indices is easier to reason about than two.
pub fn decode_audio(path: &Path) -> Vec<f32> {
    let mut samples = Vec::new();
    decode_each(
        &format!(
            "decodebin3 name=dec ! audio/x-raw(ANY) ! audioconvert ! audioresample \
             ! audio/x-raw,format=F32LE,rate={TONE_RATE},channels=1 \
             ! appsink name=sink sync=false"
        ),
        path,
        |sample| {
            let buffer = sample.buffer().expect("sample has a buffer");
            let map = buffer.map_readable().expect("the decoded samples map");
            samples.extend(
                map.as_slice()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
        },
    );
    samples
}

/// Decodes `path` and calls `visit` with each video frame's luma.
fn for_each_gray(path: &Path, mut visit: impl FnMut(GrayFrame)) {
    for_each_frame(path, "GRAY8", 1, |width, height, data| {
        visit(GrayFrame {
            width,
            height,
            data,
        })
    })
}

/// Decodes `path` with whatever decoder the machine ranks first, and calls
/// `visit` with each video frame as `format` (`bytes` bytes a pixel, one
/// plane), tightly packed. The video stream is selected by caps, so an audio
/// stream is left alone.
fn for_each_frame(
    path: &Path,
    format: &str,
    bytes: usize,
    mut visit: impl FnMut(usize, usize, Vec<u8>),
) {
    let description = format!(
        "decodebin3 name=dec ! video/x-raw(ANY) ! videoconvert \
         ! video/x-raw,format={format} ! appsink name=sink sync=false"
    );
    decode_each(&description, path, |sample| {
        let info = sample
            .caps()
            .and_then(|caps| gst_video::VideoInfo::from_caps(caps).ok())
            .expect("decoded sample has video caps");
        let buffer = sample.buffer().expect("sample has a buffer");
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
            .expect("the decoded frame maps");
        let (width, height) = (info.width() as usize, info.height() as usize);
        let stride = frame.plane_stride()[0] as usize;
        let plane = frame.plane_data(0).expect("the format has one plane");
        let data = plane
            .chunks(stride)
            .take(height)
            .flat_map(|row| row[..width * bytes].iter().copied())
            .collect();
        visit(width, height, data);
    })
}

/// Runs `description` — a `decodebin3` named `dec` fed from `path`, ending in
/// an `appsink` named `sink` — and calls `visit` with every sample it yields.
///
/// The decoder is whatever the machine ranks first, and the stream is picked
/// by the caller's caps, so the other streams of the file are left alone.
fn decode_each(description: &str, path: &Path, mut visit: impl FnMut(&gst::Sample)) {
    let pipeline = gst::parse::launch(description)
        .expect("decode pipeline parses")
        .downcast::<gst::Pipeline>()
        .expect("a multi-element launch string yields a pipeline");
    // Located before it is linked: linking queries it, which starts it, and a
    // source with no location posts an error then.
    let filesrc = gst::ElementFactory::make("filesrc")
        .property("location", path)
        .build()
        .expect("filesrc is in gst core");
    pipeline.add(&filesrc).expect("add filesrc");
    filesrc
        .link(&pipeline.by_name("dec").expect("decode pipeline has `dec`"))
        .expect("link filesrc to decodebin3");
    let sink = pipeline
        .by_name("sink")
        .and_downcast::<gst_app::AppSink>()
        .expect("decode pipeline has an appsink named `sink`");
    pipeline
        .set_state(gst::State::Playing)
        .expect("decode pipeline starts");
    let bus = pipeline.bus().expect("a pipeline has a bus");
    while let Some(sample) = sink.try_pull_sample(TIMEOUT) {
        visit(&sample);
    }
    let error = bus.pop_filtered(&[gst::MessageType::Error]);
    let eos = sink.is_eos();
    let _ = pipeline.set_state(gst::State::Null);
    if let Some(msg) = error {
        panic!("decoding {} failed: {msg:?}", path.display());
    }
    assert!(eos, "decoding {} timed out after {TIMEOUT}", path.display());
}

/// Runs `description` to EOS, with its `filesink name=out` writing to `path`.
fn run(description: &str, path: &Path) -> PathBuf {
    run_with(description, path, |_| {})
}

/// [`run`], calling `setup` on the pipeline before it starts.
///
/// The location is set as a property rather than spliced into the launch
/// string, so paths with spaces or quotes need no escaping.
fn run_with(description: &str, path: &Path, setup: impl FnOnce(&gst::Pipeline)) -> PathBuf {
    let pipeline = gst::parse::launch(description)
        .unwrap_or_else(|e| panic!("fixture pipeline failed to parse: {e}\n{description}"))
        .downcast::<gst::Pipeline>()
        .expect("a multi-element launch string yields a pipeline");
    pipeline
        .by_name("out")
        .expect("fixture pipeline has a filesink named `out`")
        .set_property("location", path);
    setup(&pipeline);

    pipeline
        .set_state(gst::State::Playing)
        .unwrap_or_else(|e| panic!("fixture pipeline failed to start: {e}\n{description}"));
    let bus = pipeline.bus().expect("a pipeline has a bus");
    let msg = bus.timed_pop_filtered(TIMEOUT, &[gst::MessageType::Eos, gst::MessageType::Error]);
    let _ = pipeline.set_state(gst::State::Null);

    match msg.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => path.to_path_buf(),
        Some(gst::MessageView::Error(err)) => panic!(
            "fixture pipeline error from {:?}: {} ({:?})\n{description}",
            err.src().map(|s| s.path_string()),
            err.error(),
            err.debug()
        ),
        _ => panic!("fixture pipeline did not reach EOS within {TIMEOUT}\n{description}"),
    }
}
