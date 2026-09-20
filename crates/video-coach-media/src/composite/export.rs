//! The export tail (spec X2–X4): the composite read back as NV12, encoded to
//! H.264 and muxed into an MP4, driven by the pump on a thread of its own.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use video_coach_core::export::{FrameSpec, OUTPUT_FPS};
use video_coach_core::zoom::Zoom;

use super::decode::Decoder;
use super::{
    fit_rect, frame_time, head, install_zoom, place, push_buffer, CompositeError, Gl, Stopper,
    Watch, POLL,
};
use crate::player::{seconds_to_clock, Diagnostics};

/// The output frame size.
const OUTPUT_WIDTH: i32 = 1920;
const OUTPUT_HEIGHT: i32 = 1080;
/// The one quality setting until Phase 8's picker: a quantizer, since the
/// hardware encoder is CQP-only. QP 24 is ~11 Mbps on camera footage.
const QP: u32 = 24;

/// Why an export produced no file. The composite's error under export's name,
/// which the bus and the UI have always used.
pub type ExportError = CompositeError;

/// What to export.
#[derive(Debug, Clone)]
pub struct ExportJob {
    /// The source video: a snapshot taken when the export starts.
    pub source: PathBuf,
    /// The clip's frame schedule (`video_coach_core::export::frame_schedule`).
    pub frames: Vec<FrameSpec>,
    /// The output file. Written as `<path>.part` and renamed on success, so a
    /// failed export never touches a file already there.
    pub path: PathBuf,
}

/// What a running export reports, on its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportMessage {
    /// Whole percent of the frames pushed, sent each time it changes.
    Progress(u8),
    /// Sent exactly once, last.
    Finished(Result<ExportDone, ExportError>),
}

/// A finished export.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportDone {
    pub path: PathBuf,
    /// Factory name of the H.264 encoder, e.g. `vah264lpenc`.
    pub encoder: String,
    /// The decode side's path, as the player logs it.
    pub diagnostics: Diagnostics,
}

/// A running export. It owns its thread, and every GStreamer object it
/// creates lives and dies on that thread. Dropping it cancels and joins.
pub struct Exporter {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Exporter {
    /// Starts exporting `job`. `on_message` is called on the export thread:
    /// [`ExportMessage::Progress`] as frames go out, then exactly one
    /// [`ExportMessage::Finished`]. `job` must have frames: the bus refuses
    /// an empty clip.
    pub fn start(
        job: ExportJob,
        on_message: impl FnMut(ExportMessage) + Send + 'static,
    ) -> Exporter {
        Self::spawn(job, on_message, None)
    }

    /// [`Exporter::start`], with `inject` (a launch-string element) spliced in
    /// before the encoder. Tests use it to fail the graph mid-stream.
    fn spawn(
        job: ExportJob,
        mut on_message: impl FnMut(ExportMessage) + Send + 'static,
        inject: Option<&'static str>,
    ) -> Exporter {
        debug_assert!(!job.frames.is_empty(), "an export needs frames");
        let cancel = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("export".into())
            .spawn({
                let cancel = cancel.clone();
                move || {
                    let result = run(&job, &cancel, inject, &mut on_message);
                    on_message(ExportMessage::Finished(result));
                }
            })
            .expect("spawn the export thread");
        Exporter {
            cancel,
            thread: Some(thread),
        }
    }

    /// Asks the export to stop. It notices within about 10 ms, deletes its
    /// `.part` and finishes with [`ExportError::Cancelled`] — unless it had
    /// already finished, in which case its own result stands.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Exporter {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `<path>.part`: where the output is written until it is complete.
fn part_path(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// Exports to the `.part` file and renames it into place, or deletes it.
fn run(
    job: &ExportJob,
    cancel: &AtomicBool,
    inject: Option<&str>,
    on_message: &mut impl FnMut(ExportMessage),
) -> Result<ExportDone, ExportError> {
    let part = part_path(&job.path);
    // The pipelines are NULL by the time `export` returns, so nothing holds
    // the file open.
    let result = export(job, &part, cancel, inject, on_message).and_then(|done| {
        std::fs::rename(&part, &job.path)
            .map(|()| done)
            .map_err(|e| ExportError::Failed(format!("could not move the export into place: {e}")))
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn export(
    job: &ExportJob,
    part: &Path,
    cancel: &AtomicBool,
    inject: Option<&str>,
    on_message: &mut impl FnMut(ExportMessage),
) -> Result<ExportDone, ExportError> {
    let gl = Gl::shared()?;
    let watch = Watch {
        cancel,
        error: Arc::default(),
    };
    let mut decoder = Decoder::start(&job.source, &gl, &watch)?;
    let mut encoder = None;

    let total = job.frames.len();
    let mut percent = 0;
    for (n, frame) in job.frames.iter().enumerate() {
        let sample = decoder.frame_at(seconds_to_clock(frame.source_time), &watch)?;
        // The first frame's caps shape the encode side: its size, PAR and
        // memory.
        if encoder.is_none() {
            let zooms = job.frames.iter().map(|f| f.zoom).collect();
            encoder = Some(Encoder::start(sample, part, zooms, &gl, inject, &watch)?);
        }
        let encoder = encoder.as_ref().expect("started above");
        encoder.push(n as u64, sample, &watch)?;
        let now = ((n + 1) * 100 / total) as u8;
        if now != percent {
            percent = now;
            on_message(ExportMessage::Progress(percent));
        }
    }
    let encoder = encoder.ok_or_else(|| ExportError::Failed("the clip has no frames".into()))?;
    encoder.finish(&watch)?;
    Ok(ExportDone {
        path: job.path.clone(),
        encoder: encoder.name().to_owned(),
        diagnostics: decoder.diagnostics(),
    })
}

/// The H.264 encoders export can use, in preference order, with their
/// launch-string settings. Only encoders someone has run are listed (spec X3).
fn encoders() -> [(&'static str, String); 2] {
    [
        (
            "vah264lpenc",
            format!("rate-control=cqp qpi={QP} qpp={QP} key-int-max=60"),
        ),
        // Constant quality: smaller than constant QP at the same quality.
        // `medium` runs at 0.39x realtime; `veryfast` keeps up.
        (
            "x264enc",
            format!("pass=qual quantizer={QP} speed-preset=veryfast key-int-max=60"),
        ),
    ]
}

struct Encoder {
    /// Held to go to NULL with the encoder.
    _pipeline: Stopper,
    appsrc: gst_app::AppSrc,
    name: &'static str,
    /// Set when the file is complete.
    eos: Arc<AtomicBool>,
}

impl Encoder {
    /// Builds the graph for frames shaped like `first` (the decoder's),
    /// writing to `part`, and sets it PLAYING. Output frame `n` gets
    /// `zooms[n]`. Its errors reach `watch`. `inject` is spliced in before the
    /// encoder (tests).
    ///
    /// The encoder is the first of [`encoders`] installed. A presence check
    /// only: one that fails at start fails the export (BACKLOG #39).
    fn start(
        first: &gst::Sample,
        part: &Path,
        zooms: Vec<Zoom>,
        gl: &Gl,
        inject: Option<&str>,
        watch: &Watch,
    ) -> Result<Encoder, ExportError> {
        let (name, settings) = encoders()
            .into_iter()
            .find(|(name, _)| gst::ElementFactory::find(name).is_some())
            .ok_or_else(|| {
                ExportError::Failed(
                    "no H.264 encoder: install gst-plugins-ugly (x264enc) or VA drivers".into(),
                )
            })?;
        let inject = inject.map(|i| format!("{i} ! ")).unwrap_or_default();
        // The readback before the encoder is required, and so is the queue.
        let description = format!(
            "{head} \
             ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 \
             ! gldownload ! video/x-raw,format=NV12 ! queue \
             ! {inject}{name} {settings} \
             ! h264parse ! video/x-h264,profile=high,stream-format=avc,alignment=au \
             ! mp4mux name=mux ! filesink name=out",
            head = head(OUTPUT_WIDTH, OUTPUT_HEIGHT)
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| ExportError::Failed(format!("could not build the export graph: {e}")))?
            .downcast::<gst::Pipeline>()
            .expect("a multi-element launch string yields a pipeline");
        let by_name = |n: &str| pipeline.by_name(n).expect("named in the launch string");

        let caps = first
            .caps()
            .ok_or_else(|| ExportError::Failed("a decoded frame has no caps".into()))?;
        let info = gst_video::VideoInfo::from_caps(caps)
            .map_err(|e| ExportError::Failed(format!("unusable decoded caps {caps}: {e}")))?;
        let mut caps = caps.to_owned();
        caps.make_mut()
            .set("framerate", gst::Fraction::new(OUTPUT_FPS as i32, 1));
        let appsrc = by_name("src")
            .downcast::<gst_app::AppSrc>()
            .expect("`src` is an appsrc");
        appsrc.set_caps(Some(&caps));

        let mix_pad = by_name("mix")
            .static_pad("sink_0")
            .expect("the mixer's first pad is linked");
        place(&mix_pad, fit_rect(&info, OUTPUT_WIDTH, OUTPUT_HEIGHT), 0);
        // `moov` goes first, in space reserved up front, with no temp file
        // (`faststart` writes the whole `mdat` to `$TMPDIR`, which a crash
        // leaks). The reserve must cover the whole file, so it gets a margin.
        let duration = frame_time(zooms.len() as u64);
        install_zoom(&by_name("zoom"), zooms);
        by_name("mux").set_property(
            "reserved-max-duration",
            (duration + duration / 10 + gst::ClockTime::SECOND).nseconds(),
        );
        by_name("out").set_property("location", part);

        let eos = gl.install(&pipeline, watch, |_| {});
        let pipeline = Stopper(pipeline);
        if pipeline.set_state(gst::State::Playing).is_err() {
            return Err(watch.failure("could not start the encoder"));
        }
        Ok(Encoder {
            _pipeline: pipeline,
            appsrc,
            name,
            eos,
        })
    }

    /// The encoder's factory name.
    fn name(&self) -> &'static str {
        self.name
    }

    /// Pushes `sample`'s buffer as output frame `n`. A reference, not a pixel
    /// copy: the same GL texture may go out many times.
    fn push(&self, n: u64, sample: &gst::Sample, watch: &Watch) -> Result<(), ExportError> {
        let mut out = sample
            .buffer()
            .expect("the decoder keeps only samples with a buffer")
            .copy();
        {
            let out = out.get_mut().expect("a fresh copy is writable");
            out.set_pts(frame_time(n));
            out.set_dts(gst::ClockTime::NONE);
            out.set_duration(frame_time(n + 1) - frame_time(n));
        }
        push_buffer(&self.appsrc, out, &format!("frame {n}"), watch)
    }

    /// Ends the stream and waits for the muxer to finish the file.
    fn finish(&self, watch: &Watch) -> Result<(), ExportError> {
        let _ = self.appsrc.end_of_stream();
        loop {
            // Read before the check: an error is recorded before any EOS.
            let done = self.eos.load(Ordering::SeqCst);
            watch.check()?;
            if done {
                return Ok(());
            }
            std::thread::sleep(POLL.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::fixtures::{self, CounterKind};

    #[test]
    fn part_path_appends_to_the_file_name() {
        assert_eq!(
            part_path(Path::new("/a/b c.mp4")),
            PathBuf::from("/a/b c.mp4.part")
        );
    }

    /// An element erroring mid-stream, downstream of `appsrc`, ends the
    /// export: a blocking push would hang here forever.
    #[test]
    fn a_mid_stream_error_fails_the_export_without_hanging() {
        gst::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = fixtures::counter_video(
            &dir.path().join("src.webm"),
            640,
            360,
            25,
            75,
            CounterKind::Vp8WebmWithAudio,
        );
        let path = dir.path().join("out.mp4");
        let frames = (0..60)
            .map(|n| FrameSpec {
                source_time: f64::from(n) / 30.0,
                zoom: Zoom::IDENTITY,
            })
            .collect();
        let job = ExportJob {
            source,
            frames,
            path: path.clone(),
        };
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let _exporter = Exporter::spawn(
            job,
            move |msg| {
                let _ = tx.send(msg);
            },
            Some("identity error-after=10"),
        );

        let deadline = Duration::from_secs(60);
        let result = loop {
            match rx.recv_timeout(deadline.saturating_sub(started.elapsed())) {
                Ok(ExportMessage::Finished(result)) => break result,
                Ok(ExportMessage::Progress(_)) => {}
                Err(_) => panic!("no Finished within {deadline:?}"),
            }
        };
        assert!(
            matches!(result, Err(ExportError::Failed(_))),
            "expected a failure, got {result:?}"
        );
        assert!(!path.exists());
        assert!(!part_path(&path).exists());
    }
}
