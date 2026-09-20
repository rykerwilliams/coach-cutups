//! The composite graph (spec X2–X4, P1): one clip's frame schedule rendered
//! on the GPU, with an [`export`] tail or a [`preview`] tail over the same
//! decode side, pump and geometry.
//!
//! ```text
//! decode: filesrc ! decodebin3 ! [gl_bin: glupload ! glcolorconvert] ! appsink
//! pump:   per output frame, the last decoded frame at or before its source
//!         time, re-stamped n/30
//! head:   appsrc ! gltransformation ! glvideomixer ! <out_w>x<out_h> 30/1
//! export: ! glcolorconvert ! NV12 ! gldownload ! queue ! <encoder>
//!         ! h264parse ! mp4mux ! filesink <path>.part
//! preview:! glcolorconvert ! RGBA GL ! appsink sync=true -> the FrameMailbox
//! ```
//!
//! **One graph everywhere.** Export runs on a surfaceless EGL display of its
//! own (one per process), so it doesn't depend on the UI's. Without a GPU,
//! Mesa's llvmpipe runs the same graph — CI included — so the tests exercise
//! the shipping zoom and letterbox. Without EGL at all it fails loudly; there
//! is no software variant. Preview takes Slint's display and context instead
//! (measured: a private one is ~4x worse at the UI's p95), which is why the
//! GL context is a parameter rather than a rule.
//!
//! **Never block without a bound.** A blocking `appsrc` push never returns
//! after a downstream error, and a long pull waits out its whole timeout after
//! a decode error (both measured). So `appsrc` doesn't block, pulls use short
//! timeouts, and every wait polls the cancel flag and the first error either
//! pipeline posted.

mod decode;
pub mod export;
pub mod preview;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_gl as gst_gl;
use gstreamer_gl_egl as gst_gl_egl;
use gstreamer_video as gst_video;
use video_coach_core::export::{FrameSpec, OUTPUT_FPS};
use video_coach_core::zoom::Zoom;

use crate::player::answer_need_context;

/// How long any wait goes between checks of the cancel flag and errors.
const POLL: gst::ClockTime = gst::ClockTime::from_mseconds(10);

/// Frames an `appsrc` may hold before the pump waits for room.
const QUEUED: u64 = 4;

/// Why a composite stopped early. Export presents it as
/// [`ExportError`](export::ExportError).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum CompositeError {
    #[error("the export was cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

/// What every wait checks between polls: the cancel flag, and the first
/// `ERROR` either pipeline posted.
struct Watch<'a> {
    cancel: &'a AtomicBool,
    /// Written by the pipelines' sync handlers ([`Gl::install`]) as the error
    /// is posted.
    error: Arc<Mutex<Option<String>>>,
}

impl Watch<'_> {
    fn check(&self) -> Result<(), CompositeError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(CompositeError::Cancelled);
        }
        match &*self.error.lock().expect("the error slot isn't poisoned") {
            Some(error) => Err(CompositeError::Failed(error.clone())),
            None => Ok(()),
        }
    }

    /// Why a step failed: the posted error if there is one, else `fallback`.
    fn failure(&self, fallback: impl Into<String>) -> CompositeError {
        self.check()
            .err()
            .unwrap_or_else(|| CompositeError::Failed(fallback.into()))
    }
}

/// The GL display and context a composite's pipelines share, so the GL memory
/// crossing appsink → appsrc belongs to one share group.
#[derive(Clone)]
pub struct Gl {
    display: gst_gl::GLDisplay,
    context: gst_gl::GLContext,
}

impl Gl {
    /// The UI's wrapped display and context, as `video.rs` takes them from
    /// Slint. Preview composites on these: sharing them measured about 4x
    /// better at the UI's p95 than a private surfaceless display, and it
    /// saves a copy into system memory besides (spec P1).
    pub fn wrapped(display: gst_gl::GLDisplay, context: gst_gl::GLContext) -> Gl {
        Gl { display, context }
    }

    /// The process's own surfaceless display and context, created on first
    /// success; a failure is retried by the next caller. Export always runs
    /// here, and so does a preview with no UI — tests and the harness.
    ///
    /// **One per process, never dropped.** Every surfaceless `GLDisplayEGL`
    /// wraps the same `EGLDisplay`, and finalizing one calls `eglTerminate` on
    /// it for all: with a display per export, exports running side by side
    /// (the tests) failed to import frames, or to create a context, as soon as
    /// one finished.
    pub fn shared() -> Result<Gl, CompositeError> {
        static GL: Mutex<Option<Gl>> = Mutex::new(None);
        let mut gl = GL.lock().expect("the GL slot isn't poisoned");
        if gl.is_none() {
            *gl = Some(Gl::new_surfaceless().map_err(CompositeError::Failed)?);
        }
        Ok(gl.clone().expect("created above"))
    }

    /// A surfaceless EGL display: it needs no display server, and stays
    /// zero-copy. `GLDisplayEGL::new()` fails without one, and a plain
    /// `GLDisplay::new()` picks GLX on X11, which copies every frame.
    fn new_surfaceless() -> Result<Gl, String> {
        let display = gst_gl_egl::GLDisplayEGL::new_surfaceless()
            .map_err(|e| {
                format!("this needs a surfaceless EGL display (EGL_MESA_platform_surfaceless): {e}")
            })?
            .upcast::<gst_gl::GLDisplay>();
        let context = {
            let lock = display.object_lock();
            gst_gl::GLDisplay::create_context(&lock, None::<&gst_gl::GLContext>)
        }
        .map_err(|e| format!("could not create an EGL context: {e}"))?;
        Ok(Gl { display, context })
    }

    /// Answers `pipeline`'s GL context requests with this display and
    /// context, records its first `ERROR` in `watch`, and returns a flag set
    /// at its `EOS`. `watched` sees every other message, on GStreamer's
    /// threads. Every message is then dropped, so nothing piles up.
    fn install(
        &self,
        pipeline: &gst::Pipeline,
        watch: &Watch,
        watched: impl Fn(&gst::Message) + Send + Sync + 'static,
    ) -> Arc<AtomicBool> {
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
                    _ => watched(msg),
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

/// The head every composite shares: the pumped source `appsrc` through the
/// zoom into a mixer pinned to `out_w`×`out_h` at 30 fps.
///
/// The mixer's output size is its pads' bounding box and its rate the input's,
/// so both are pinned after it. Each tail appends its own conversion.
fn head(out_w: i32, out_h: i32) -> String {
    format!(
        "appsrc name=src format=time is-live=false block=false \
           max-buffers={QUEUED} max-bytes=0 max-time=0 \
         ! gltransformation name=zoom ortho=true \
         ! glvideomixer name=mix background=black \
         ! video/x-raw(memory:GLMemory),width={out_w},height={out_h},\
           framerate={OUTPUT_FPS}/1,pixel-aspect-ratio=1/1"
    )
}

/// Waits until `appsrc` has room for another buffer, in [`POLL`]/5 steps.
///
/// `block=false` never waits, so the wait is here, where the pipeline's errors
/// and the cancel flag are seen. Preview waits on both its appsrcs *before* it
/// takes the pump's lock, so a seek arriving on another thread never queues
/// behind the wait.
fn wait_for_room(appsrc: &gst_app::AppSrc, watch: &Watch) -> Result<(), CompositeError> {
    while appsrc.current_level_buffers() >= QUEUED {
        watch.check()?;
        std::thread::sleep(Duration::from(POLL) / 5);
    }
    Ok(())
}

/// Pushes `buffer` into `appsrc` once it has room. `what` names the buffer in
/// a failure.
fn push_buffer(
    appsrc: &gst_app::AppSrc,
    buffer: gst::Buffer,
    what: &str,
    watch: &Watch,
) -> Result<(), CompositeError> {
    wait_for_room(appsrc, watch)?;
    appsrc
        .push_buffer(buffer)
        .map_err(|e| watch.failure(format!("pushing {what}: {e:?}")))?;
    Ok(())
}

/// Output frame `n`'s time, `n/30` s, floored to the nanosecond.
fn frame_time(n: u64) -> gst::ClockTime {
    gst::ClockTime::SECOND
        .mul_div_floor(n, u64::from(OUTPUT_FPS))
        .expect("no overflow")
}

/// `sample`'s buffer as output frame `n`. A reference, not a pixel copy: a
/// freeze (and every held source frame) sends the same texture out again.
fn stamp(sample: &gst::Sample, n: u64) -> gst::Buffer {
    let mut buffer = sample
        .buffer()
        .expect("the decoder keeps only samples with a buffer")
        .copy();
    stamp_buffer(&mut buffer, n);
    buffer
}

/// The PTS contract both tails push on: output frame `n` is at `n/30` and one
/// frame long, with no DTS. The overlay takes the source frame's own stamp,
/// which is what keeps the mixer's pads together.
fn stamp_buffer(buffer: &mut gst::Buffer, n: u64) {
    let buffer = buffer.get_mut().expect("a buffer of our own is writable");
    buffer.set_pts(frame_time(n));
    buffer.set_dts(gst::ClockTime::NONE);
    buffer.set_duration(frame_time(n + 1) - frame_time(n));
}

/// The output frame at `t`, to the nearest frame: [`frame_time`] inverted,
/// which is how a seek's position and a buffer's PTS name a frame.
fn frame_index(t: gst::ClockTime) -> u64 {
    t.nseconds()
        .saturating_mul(u64::from(OUTPUT_FPS))
        .saturating_add(gst::ClockTime::SECOND.nseconds() / 2)
        / gst::ClockTime::SECOND.nseconds()
}

/// Sets each buffer's zoom on `transform` as it arrives, keyed on its PTS, so
/// the value matches the frame whatever is queued.
///
/// And makes `transform` render the zoom itself. Offered the choice (by
/// `glvideomixer`), `gltransformation` passes frames through with an affine
/// transformation meta, and the mixer draws the transformed quad unclipped:
/// a zoomed 4:3 source spills into its pillarbox bars (measured). Rendered
/// into its own source-sized texture, the zoom is clipped to the picture.
fn install_zoom(transform: &gst::Element, frames: &[FrameSpec]) {
    let zooms: Vec<Zoom> = frames.iter().map(|f| f.zoom).collect();
    transform
        .static_pad("src")
        .expect("gltransformation has a src pad")
        .add_probe(
            gst::PadProbeType::QUERY_DOWNSTREAM | gst::PadProbeType::PULL,
            |_, info| {
                if let Some(gst::PadProbeData::Query(query)) = &mut info.data {
                    if let gst::QueryViewMut::Allocation(allocation) = query.view_mut() {
                        while let Some(i) = allocation
                            .find_allocation_meta::<gst_video::VideoAffineTransformationMeta>()
                        {
                            allocation.remove_nth_allocation_meta(i);
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            },
        );
    let weak = transform.downgrade();
    transform
        .static_pad("sink")
        .expect("gltransformation has a sink pad")
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let (Some(pts), Some(transform)) =
                (info.buffer().and_then(|b| b.pts()), weak.upgrade())
            else {
                return gst::PadProbeReturn::Ok;
            };
            if let Some(zoom) = zooms.get(frame_index(pts) as usize) {
                let (s, tx, ty) = zoom_params(*zoom);
                transform.set_property("scale-x", s);
                transform.set_property("scale-y", s);
                transform.set_property("translation-x", tx);
                transform.set_property("translation-y", ty);
            }
            gst::PadProbeReturn::Ok
        });
}

/// `gltransformation`'s `(scale, translation-x, translation-y)` for `zoom`,
/// placed before the mixer and rendering into a texture of the source's own
/// size (see [`install_zoom`]).
///
/// The visible centre is the source point `(0.5 + pan_x, 0.5 + pan_y)` (y
/// down). Translation is applied after the scale, in units of the picture's
/// width and height, and positive `translation-y` moves the image **down**
/// (measured at s = 0.25, 0.5 and 2 on 4:3 and 16:9). The spec's mapping,
/// with y the other way, was measured on the affine-meta path, which doesn't
/// clip.
fn zoom_params(zoom: Zoom) -> (f32, f32, f32) {
    let s = zoom.scale;
    (s as f32, (-zoom.pan_x * s) as f32, (-zoom.pan_y * s) as f32)
}

/// `info`'s display aspect: width ÷ height with the pixel aspect ratio
/// applied. A missing or nonsense PAR counts as square.
fn display_aspect(info: &gst_video::VideoInfo) -> f64 {
    let par = info.par();
    let (par_n, par_d) = if par.numer() > 0 && par.denom() > 0 {
        (par.numer(), par.denom())
    } else {
        (1, 1)
    };
    f64::from(info.width()) * f64::from(par_n) / (f64::from(info.height()) * f64::from(par_d))
}

/// The source's picture rect inside an `out_w`×`out_h` output,
/// `(x, y, width, height)`: its display aspect letterboxed or pillarboxed.
///
/// The overlay pad takes this same rect, so drawings land on the picture
/// rather than across the bars (spec P4).
fn fit_rect(info: &gst_video::VideoInfo, out_w: i32, out_h: i32) -> (i32, i32, i32, i32) {
    let aspect = display_aspect(info);
    let (ow, oh) = (f64::from(out_w), f64::from(out_h));
    let (w, h) = if aspect >= ow / oh {
        (out_w, (ow / aspect).round() as i32)
    } else {
        ((oh * aspect).round() as i32, out_h)
    };
    ((out_w - w) / 2, (out_h - h) / 2, w, h)
}

/// Places `pad` at `(x, y, w, h)` in the mixer's output, at `zorder`.
fn place(pad: &gst::Pad, (x, y, w, h): (i32, i32, i32, i32), zorder: u32) {
    pad.set_property("xpos", x);
    pad.set_property("ypos", y);
    pad.set_property("width", w);
    pad.set_property("height", h);
    pad.set_property("zorder", zorder);
}
