//! The bridge between Slint's renderer and the player's GL sink (spec D2, D3).
//!
//! Slint renders with Skia over EGL. When its context comes up, the bridge
//! wraps it for GStreamer and hands it to the bus, which only then lets the
//! pipeline leave NULL (the startup gate). Before each redraw it takes the
//! newest frame from the mailbox and gives Slint the frame's texture to draw,
//! with no copy. On teardown it waits for the bus to release the context.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gstreamer_gl as gst_gl;
use gstreamer_gl::prelude::*;
use gstreamer_gl_egl as gst_gl_egl;
use slint::ComponentHandle;

use video_coach_app::bus::{BusHandle, Command};

use crate::AppWindow;

/// The UI's own frame time, collected while `COACH_FRAME_STATS=1` is set and
/// printed at teardown. It is the budget the clip preview has to stay inside:
/// a p95 of 4 ms, against an idle control of about 1.3 ms (Phase 7 "Gates").
/// A measuring tool, not a log, so it is off unless asked for.
#[derive(Default)]
struct FrameStats {
    /// `BeforeRendering`'s moment, read again at `AfterRendering`.
    started: Option<Instant>,
    /// Every frame's milliseconds, bar the first (see [`FrameStats::record`]).
    frames: Vec<f64>,
    warmed: bool,
}

impl FrameStats {
    /// The window's very first draw compiles Skia's shaders and uploads its
    /// atlases -- hundreds of milliseconds, once, before anything is playing.
    /// It is startup, not a frame time, so it is not one of the samples.
    fn record(&mut self, ms: f64) {
        if self.frames.is_empty() && !self.warmed {
            self.warmed = true;
            return;
        }
        self.frames.push(ms);
    }

    fn report(&mut self) {
        if self.frames.is_empty() {
            return;
        }
        // No NaN reaches this: every value is an elapsed duration.
        self.frames.sort_by(f64::total_cmp);
        let last = self.frames.len() - 1;
        let at = |q: usize| self.frames[self.frames.len() * q / 100];
        eprintln!(
            "video: UI frame time over {} frames: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
            self.frames.len(),
            at(50),
            at(95),
            self.frames[last],
        );
    }
}

/// The mapped frame Slint is drawing from. Mapping holds the texture; it stays
/// mapped until the next frame replaces it.
type MappedFrame = gst_gl::GLVideoFrame<gst_gl::gl_video_frame::Readable>;

/// Asks for a redraw whenever the mailbox receives a frame, and installs the
/// rendering notifier that draws it.
pub fn install(window: &AppWindow, bus: Rc<RefCell<BusHandle>>) {
    let weak = window.as_weak();
    // Runs on the appsink's streaming thread; the redraw is queued onto the
    // UI thread.
    bus.borrow().mailbox().set_redraw(move || {
        let _ = weak.upgrade_in_event_loop(|w| w.window().request_redraw());
    });

    let weak = window.as_weak();
    let mut context: Option<gst_gl::GLContext> = None;
    let mut current: Option<MappedFrame> = None;
    let mut stats = std::env::var_os("COACH_FRAME_STATS")
        .is_some_and(|v| v == "1")
        .then(FrameStats::default);
    window
        .window()
        .set_rendering_notifier(move |state, api| match state {
            slint::RenderingState::RenderingSetup => {
                let (display, wrapped) = wrap_slint_egl_context(api);
                context = Some(wrapped.clone());
                bus.borrow().send(Command::GlReady {
                    display: display.upcast(),
                    context: wrapped,
                });
            }
            slint::RenderingState::BeforeRendering => {
                if let Some(stats) = &mut stats {
                    stats.started = Some(Instant::now());
                }
                let Some(context) = &context else {
                    return;
                };
                let Some(frame) = bus.borrow().mailbox().take() else {
                    return;
                };
                if let Some(sync) = frame.buffer.meta::<gst_gl::GLSyncMeta>() {
                    sync.wait(context);
                }
                let Ok(mapped) =
                    gst_gl::GLVideoFrame::from_buffer_readable(frame.buffer, &frame.info)
                else {
                    eprintln!("video: could not map the GL frame");
                    return;
                };
                let Some(texture) = mapped
                    .texture_id(0)
                    .ok()
                    .and_then(std::num::NonZeroU32::new)
                else {
                    eprintln!("video: the GL frame has no texture");
                    return;
                };
                // SAFETY: the texture belongs to `mapped`, which is kept in
                // `current` until a later frame has replaced this image.
                let image = unsafe {
                    slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                        texture,
                        [mapped.width(), mapped.height()].into(),
                    )
                    .build()
                };
                // D3: the display shape comes from the caps, including the
                // pixel aspect ratio, never a forced 1:1.
                let par = frame.info.par();
                let display_w = mapped.width() as f64 * par.numer() as f64 / par.denom() as f64;
                if let Some(w) = weak.upgrade() {
                    w.set_frame_width(display_w as f32);
                    w.set_frame_height(mapped.height() as f32);
                    w.set_frame(image);
                }
                current.replace(mapped);
            }
            // Everything Skia drew this frame sits between the two, so this
            // is the whole of the UI's draw, not just taking the frame.
            slint::RenderingState::AfterRendering => {
                if let Some(stats) = &mut stats {
                    if let Some(started) = stats.started.take() {
                        stats.record(started.elapsed().as_secs_f64() * 1e3);
                    }
                }
            }
            slint::RenderingState::RenderingTeardown => {
                if let Some(stats) = &mut stats {
                    stats.report();
                }
                // GStreamer must stop using the shared context before Slint
                // destroys it: the bus takes the pipeline to NULL and acks.
                bus.borrow_mut().shutdown();
                current.take();
                if let Some(context) = context.take() {
                    let _ = context.activate(false);
                }
            }
            _ => {}
        })
        .expect("no rendering notifier: the renderer is not OpenGL (need skia-opengl)");
}

/// Wraps Slint's current EGL display and context for GStreamer. Fails loudly
/// if there is no EGL context: the app never continues on a copying path (D2).
fn wrap_slint_egl_context(
    api: &slint::GraphicsAPI<'_>,
) -> (gst_gl_egl::GLDisplayEGL, gst_gl::GLContext) {
    let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
        panic!("the skia-opengl renderer did not provide a native OpenGL API");
    };
    let egl = glutin_egl_sys::egl::Egl::load_with(|symbol| {
        get_proc_address(&std::ffi::CString::new(symbol).unwrap())
    });
    let platform = gst_gl::GLPlatform::EGL;
    // SAFETY: called on the UI thread inside Slint's rendering notifier, where
    // Slint's context is current; the wrapped handles outlive neither it nor
    // the bus (teardown shuts the bus down first).
    unsafe {
        let native_context = egl.GetCurrentContext();
        assert!(
            !native_context.is_null(),
            "eglGetCurrentContext() is null: the skia-opengl renderer is not on EGL \
             (or fell back to software). Refusing to continue on a copying path."
        );
        let display = gst_gl_egl::GLDisplayEGL::with_egl_display(egl.GetCurrentDisplay() as usize)
            .expect("wrap the EGL display");
        let context = gst_gl::GLContext::new_wrapped(
            &display,
            native_context as usize,
            platform,
            gst_gl::GLContext::current_gl_api(platform).0,
        )
        .expect("wrap the EGL context");
        context
            .activate(true)
            .expect("activate the wrapped context");
        context
            .fill_info()
            .expect("fill the wrapped context's info");
        eprintln!(
            "video: Slint GL context: platform {:?}, api {:?}",
            context.gl_platform(),
            context.gl_api()
        );
        (display, context)
    }
}
