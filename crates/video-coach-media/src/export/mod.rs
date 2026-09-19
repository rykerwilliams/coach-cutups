//! Passthrough export (spec X2–X4): one clip's frame schedule rendered to an
//! H.264 MP4 by two pipelines and a pump between them, on a thread of its own.
//!
//! ```text
//! decode: filesrc ! decodebin3 ! [gl_bin: glupload ! glcolorconvert] ! appsink
//! pump:   per output frame, the last decoded frame at or before its source
//!         time, re-stamped n/30
//! encode: appsrc ! gltransformation ! glvideomixer ! 1920x1080 30/1
//!         ! glcolorconvert ! NV12 ! gldownload ! queue ! <encoder>
//!         ! h264parse ! mp4mux ! filesink <path>.part
//! ```
//!
//! **One graph everywhere.** Exports share a surfaceless EGL display of their
//! own (one per process), so they don't depend on the UI's. Without a GPU, Mesa's llvmpipe runs the same
//! graph — CI included — so the tests exercise the shipping zoom and
//! letterbox. Without EGL at all the export fails loudly; there is no
//! software variant.
//!
//! **Never block without a bound.** A blocking `appsrc` push never returns
//! after a downstream error, and a long pull waits out its whole timeout after
//! a decode error (both measured). So `appsrc` doesn't block, pulls use short
//! timeouts, and every wait polls the cancel flag and the first error either
//! pipeline posted.

mod decode;
mod encode;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_gl as gst_gl;
use gstreamer_gl_egl as gst_gl_egl;
use video_coach_core::export::FrameSpec;

use crate::player::{answer_need_context, seconds_to_clock, Diagnostics};
use decode::Decoder;
use encode::Encoder;

/// How long any wait goes between checks of the cancel flag and errors.
const POLL: gst::ClockTime = gst::ClockTime::from_mseconds(10);

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

/// Why an export produced no file.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    #[error("the export was cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
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
    let gl = SharedGl::get()?;
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

/// What every wait checks between polls: the cancel flag, and the first
/// `ERROR` either pipeline posted.
struct Watch<'a> {
    cancel: &'a AtomicBool,
    /// Written by the pipelines' sync handlers ([`SharedGl::install`]) as the
    /// error is posted.
    error: Arc<Mutex<Option<String>>>,
}

impl Watch<'_> {
    fn check(&self) -> Result<(), ExportError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(ExportError::Cancelled);
        }
        match &*self.error.lock().expect("the error slot isn't poisoned") {
            Some(error) => Err(ExportError::Failed(error.clone())),
            None => Ok(()),
        }
    }

    /// Why a step failed: the posted error if there is one, else `fallback`.
    fn failure(&self, fallback: impl Into<String>) -> ExportError {
        self.check()
            .err()
            .unwrap_or_else(|| ExportError::Failed(fallback.into()))
    }
}

/// The GL display and context every export pipeline shares, so the GL memory
/// crossing appsink → appsrc belongs to one share group.
///
/// **One per process, never dropped.** Every surfaceless `GLDisplayEGL` wraps
/// the same `EGLDisplay`, and finalizing one calls `eglTerminate` on it for
/// all: with a display per export, exports running side by side (the tests)
/// failed to import frames, or to create a context, as soon as one finished.
#[derive(Clone)]
struct SharedGl {
    display: gst_gl::GLDisplay,
    context: gst_gl::GLContext,
}

impl SharedGl {
    /// The process's display and context, created on first success. A
    /// failure is retried by the next export.
    fn get() -> Result<SharedGl, ExportError> {
        static GL: Mutex<Option<SharedGl>> = Mutex::new(None);
        let mut gl = GL.lock().expect("the GL slot isn't poisoned");
        if gl.is_none() {
            *gl = Some(SharedGl::new().map_err(ExportError::Failed)?);
        }
        Ok(gl.clone().expect("created above"))
    }

    /// A surfaceless EGL display: it needs no display server, and stays
    /// zero-copy. `GLDisplayEGL::new()` fails without one, and a plain
    /// `GLDisplay::new()` picks GLX on X11, which copies every frame.
    fn new() -> Result<SharedGl, String> {
        let display = gst_gl_egl::GLDisplayEGL::new_surfaceless()
            .map_err(|e| {
                format!(
                    "export needs a surfaceless EGL display (EGL_MESA_platform_surfaceless): {e}"
                )
            })?
            .upcast::<gst_gl::GLDisplay>();
        let context = {
            let lock = display.object_lock();
            gst_gl::GLDisplay::create_context(&lock, None::<&gst_gl::GLContext>)
        }
        .map_err(|e| format!("could not create an EGL context: {e}"))?;
        Ok(SharedGl { display, context })
    }

    /// Answers `pipeline`'s GL context requests with this display and
    /// context, records its first `ERROR` in `watch`, and returns a flag set
    /// at its `EOS`. Every message is then dropped, so nothing piles up.
    fn install(&self, pipeline: &gst::Pipeline, watch: &Watch) -> Arc<AtomicBool> {
        let (display, context) = (self.display.clone(), self.context.clone());
        let (error, eos) = (watch.error.clone(), Arc::new(AtomicBool::new(false)));
        let flag = eos.clone();
        pipeline
            .bus()
            .expect("a pipeline has a bus")
            .set_sync_handler(move |_, msg| {
                match msg.view() {
                    gst::MessageView::NeedContext(need) => {
                        answer_need_context(msg, need, &display, &context);
                    }
                    gst::MessageView::Error(err) => {
                        let mut error = error.lock().expect("the error slot isn't poisoned");
                        error.get_or_insert_with(|| crate::error_text(err));
                    }
                    gst::MessageView::Eos(_) => flag.store(true, Ordering::SeqCst),
                    _ => {}
                }
                gst::BusSyncReply::Drop
            });
        eos
    }
}

/// A pipeline taken to NULL when dropped, on every exit path.
struct Stopper(gst::Pipeline);

impl std::ops::Deref for Stopper {
    type Target = gst::Pipeline;

    fn deref(&self) -> &gst::Pipeline {
        &self.0
    }
}

impl Drop for Stopper {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}
