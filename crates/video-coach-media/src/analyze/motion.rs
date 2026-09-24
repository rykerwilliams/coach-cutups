//! The analysis pass's picture (spec D1): one source video read at five
//! frames a second, as a thumbnail small enough that what is left of it is
//! the framing.
//!
//! ```text
//! filesrc ! decodebin3 (the video stream only)
//!         ! videorate max-rate=5 ! glupload ! glcolorconvert ! glcolorscale
//!         ! RGBA 160x90 in GL memory ! gldownload ! appsink
//! ```
//!
//! **`videorate` before `glupload`**, so a frame that is going to be dropped
//! is never uploaded. **`decodebin3`**, so the hardware decoder's frames stay
//! in DMABuf memory to that point, and **[`Gl::shared`]**'s surfaceless EGL
//! display, never the UI's — this runs off the UI thread, and on CI it runs
//! on Mesa's llvmpipe with no GPU at all.
//!
//! **One pass, no seeking**, in file order from the first frame to the last.
//!
//! Two series come out of it, on the same decode:
//!
//! - **motion**, the mean absolute luma difference between consecutive
//!   thumbnails, five values a second — 8,500 `f32` for a 28-minute file;
//! - **thumbnails**, one a second on core's own small grid, which is about a
//!   megabyte for a half.
//!
//! Both are arithmetic over a 14,400-pixel buffer GL has already made, which
//! is analysis of a thumbnail rather than full-frame work, so the pixel rule
//! holds. Everything that reads either of them is in
//! [`motion`](video_coach_core::motion).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_gl as gst_gl;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::VideoFrameExt;
use video_coach_core::motion::{Thumbnail, MOTION_HZ, THUMBNAIL_HZ};

use super::AnalyzeError;
use crate::composite::{Gl, Stopper, Watch, POLL};
use crate::mailbox::stream_time;
use crate::player::seconds;

/// The thumbnail's width, as the spec's D1 sets it.
const THUMB_WIDTH: usize = 160;

/// The thumbnail's height, at 16:9 — the design footage's own shape, so
/// nothing here is stretched.
///
/// Footage of another shape **is** stretched into it, because the caps are
/// fixed and `glcolorscale` fills them. That costs nothing either rule reads:
/// both compare a file with itself, or one 16:9 file with another.
const THUMB_HEIGHT: usize = 90;

/// What one source's picture did.
#[derive(Debug, Clone, Default)]
pub struct Motion {
    /// The mean absolute luma difference (0–255 per pixel) between
    /// consecutive thumbnails, at [`MOTION_HZ`].
    pub motion: Vec<f32>,
    /// One thumbnail a second, at [`THUMBNAIL_HZ`], from the start of the
    /// file.
    pub thumbnails: Vec<Thumbnail>,
    /// How much picture there was: the last frame's time, in seconds.
    pub seconds: f64,
    /// How many frames the pass actually saw, which against `seconds` is the
    /// rate `videorate` really delivered.
    pub frames: usize,
}

/// Everything `path`'s picture did, one pass, no seeking.
pub fn series(path: &Path, cancel: &AtomicBool) -> Result<Motion, AnalyzeError> {
    series_reporting(path, cancel, &mut |_| {})
}

/// [`series`], reporting how far through the file it is as a percentage.
///
/// The cancel flag is looked at between every pull, so a cancelled pass stops
/// within a frame (spec B1) rather than at the end of the file.
pub fn series_reporting(
    path: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(u8),
) -> Result<Motion, AnalyzeError> {
    let watch = Watch {
        cancel,
        error: Arc::default(),
    };
    let gl = Gl::shared()?;
    let (pipeline, appsink) = start(path, &gl, &watch)?;
    // The file's own length, for the progress percentage. A file that won't
    // say is reported as zero throughout rather than refused: the percentage
    // is a comfort, not a result.
    let duration = pipeline.query_duration::<gst::ClockTime>().map(seconds);

    let mut out = Motion::default();
    let mut luma = vec![0.0f32; THUMB_WIDTH * THUMB_HEIGHT];
    let mut previous: Option<Vec<f32>> = None;
    let mut percent = 0u8;
    while let Some(sample) = pull(&appsink, &watch)? {
        let time = stream_time(&sample)
            .ok_or_else(|| AnalyzeError::Failed("a decoded frame has no timestamp".into()))?;
        let time = seconds(time);
        read_luma(&sample, &mut luma)?;

        if let Some(previous) = &previous {
            let sum: f64 = luma
                .iter()
                .zip(previous)
                .map(|(&a, &b)| f64::from((a - b).abs()))
                .sum();
            out.motion.push((sum / luma.len() as f64) as f32);
        }
        // A thumbnail for every whole second the file has reached. A second
        // with no frame of its own — a gap, or a file that starts late —
        // takes the next frame there is, so the grid stays exactly
        // [`THUMBNAIL_HZ`] and index `k` is second `k`.
        let slot = (time * THUMBNAIL_HZ).floor().max(0.0) as usize;
        if out.thumbnails.len() <= slot {
            let thumbnail = Thumbnail::from_luma(&luma, THUMB_WIDTH, THUMB_HEIGHT);
            out.thumbnails.resize(slot + 1, thumbnail);
        }
        match &mut previous {
            Some(previous) => previous.copy_from_slice(&luma),
            None => previous = Some(luma.clone()),
        }
        out.frames += 1;
        out.seconds = time;

        if let Some(duration) = duration.filter(|d| *d > 0.0) {
            let now = ((time / duration) * 100.0).clamp(0.0, 100.0) as u8;
            if now > percent {
                percent = now;
                on_progress(percent);
            }
        }
    }
    watch.check()?;
    drop(pipeline);
    Ok(out)
}

/// The pipeline, prerolled and playing, and the sink to pull from.
fn start(path: &Path, gl: &Gl, watch: &Watch) -> Result<(Stopper, gst_app::AppSink), AnalyzeError> {
    let pipeline = gst::Pipeline::new();
    let make = |factory: &str| {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|e| AnalyzeError::Failed(format!("{factory} is missing: {e}")))
    };
    let filesrc = make("filesrc")?;
    filesrc.set_property("location", path);
    let decodebin = make("decodebin3")?;
    let videorate = make("videorate")?;
    // `max-rate` implies drop-only, so nothing is ever duplicated: a file
    // that stalls leaves a gap in the series rather than a stretch of
    // stillness that never happened.
    videorate.set_property("max-rate", MOTION_HZ as i32);
    let upload = make("glupload")?;
    // `glcolorconvert` as well as `glcolorscale`, and not only for tidiness:
    // measured, `glcolorscale` alone refuses to negotiate an I420 decode to
    // an RGBA sink, and the link fails with `Noformat` before a frame moves.
    let convert = make("glcolorconvert")?;
    let scale = make("glcolorscale")?;
    let download = make("gldownload")?;
    let appsink = gst_app::AppSink::builder()
        .caps(
            &gst_video::VideoCapsBuilder::new()
                .format(gst_video::VideoFormat::Rgba)
                .width(THUMB_WIDTH as i32)
                .height(THUMB_HEIGHT as i32)
                .build(),
        )
        .sync(false)
        .max_buffers(4u32)
        .enable_last_sample(false)
        .build();
    let gl_caps = gst_video::VideoCapsBuilder::new()
        .features([gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY])
        .format(gst_video::VideoFormat::Rgba)
        .width(THUMB_WIDTH as i32)
        .height(THUMB_HEIGHT as i32)
        .build();
    let sized = gst::ElementFactory::make("capsfilter")
        .property("caps", &gl_caps)
        .build()
        .map_err(|e| AnalyzeError::Failed(format!("capsfilter is missing: {e}")))?;

    let tail = [
        &videorate,
        &upload,
        &convert,
        &scale,
        &sized,
        &download,
        appsink.upcast_ref(),
    ];
    pipeline
        .add_many([&filesrc, &decodebin])
        .expect("add the source elements");
    pipeline.add_many(tail).expect("add the thumbnail elements");
    filesrc
        .link(&decodebin)
        .expect("link filesrc to decodebin3");
    gst::Element::link_many(tail).expect("link the thumbnail elements");
    // The video stream only. `decodebin3` tolerates an unlinked audio pad,
    // and the audio is the other pass's (spec D1).
    let head = videorate.clone();
    decodebin.connect_pad_added(move |_, pad| {
        let sink = head.static_pad("sink").expect("videorate has a sink");
        if pad.name().starts_with("video_") && !sink.is_linked() {
            let _ = pad.link(&sink);
        }
    });
    gl.install(&pipeline, watch, |_| {});
    let pipeline = Stopper(pipeline);

    if pipeline.set_state(gst::State::Playing).is_err() {
        return Err(watch.failure("could not open the source"));
    }
    loop {
        watch.check()?;
        match pipeline.state(POLL) {
            (Ok(_), gst::State::Playing, gst::State::VoidPending) => break,
            (Err(_), ..) => {
                watch.check()?;
                return Err(AnalyzeError::Failed("could not read the source".into()));
            }
            _ => {}
        }
    }
    Ok((pipeline, appsink))
}

/// The next thumbnail, or `None` at the end of the file. Waits in [`POLL`]
/// steps, checking the cancel flag and the pipeline's errors between them.
fn pull(appsink: &gst_app::AppSink, watch: &Watch) -> Result<Option<gst::Sample>, AnalyzeError> {
    loop {
        if let Some(sample) = appsink.try_pull_sample(POLL) {
            return Ok(Some(sample));
        }
        if appsink.is_eos() {
            return Ok(None);
        }
        watch.check()?;
    }
}

/// `sample`'s luma, one `f32` a pixel on the 0–255 scale.
///
/// Through a [`VideoFrameRef`](gst_video::VideoFrameRef) rather than the raw
/// buffer: a downloaded frame's rows are padded to the platform's stride, and
/// reading it as a flat 160×90 block would shear the picture on any width
/// that isn't already aligned.
fn read_luma(sample: &gst::Sample, out: &mut [f32]) -> Result<(), AnalyzeError> {
    let caps = sample
        .caps()
        .ok_or_else(|| AnalyzeError::Failed("a thumbnail arrived with no caps".into()))?;
    let info = gst_video::VideoInfo::from_caps(caps)
        .map_err(|e| AnalyzeError::Failed(format!("a thumbnail's caps are not video: {e}")))?;
    let buffer = sample
        .buffer()
        .ok_or_else(|| AnalyzeError::Failed("a thumbnail arrived with no buffer".into()))?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|e| AnalyzeError::Failed(format!("could not read a thumbnail: {e}")))?;
    let stride = frame.plane_stride()[0] as usize;
    let data = frame.plane_data(0).expect("RGBA has one plane");
    for y in 0..THUMB_HEIGHT {
        let row = &data[y * stride..y * stride + THUMB_WIDTH * 4];
        for (x, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
            // Rec. 601 luma, which is what a difference of "how bright" means
            // at this size; the exact weights matter to nothing that reads it.
            out[y * THUMB_WIDTH + x] = 0.299 * f32::from(pixel[0])
                + 0.587 * f32::from(pixel[1])
                + 0.114 * f32::from(pixel[2]);
        }
    }
    Ok(())
}
