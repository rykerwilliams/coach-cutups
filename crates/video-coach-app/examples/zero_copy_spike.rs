//! Zero-copy diagnostic: plays one file through the Phase 2 display path and
//! logs whether frames reach Slint without a CPU copy.
//!
//! ```text
//! GST_DEBUG=glupload:6 cargo run -p video-coach-app --example zero_copy_spike -- <file> \
//!     [--paused] [--x PX] [--width PX] [--drift PX_PER_SEC] [--quit-after SECS]
//! ```
//!
//! The path is the one the spec fixes (D1–D3, D12): Slint's Skia renderer over
//! EGL, its context wrapped and handed to GStreamer from a bus sync handler, and
//! `playbin3` feeding `glupload ! glcolorconvert ! appsink` with GL-memory caps.
//! Zero-copy holds when the decoder is hardware, the caps into `glupload` carry
//! `memory:DMABuf`, the GL platform is EGL, and the `glupload:6` log names
//! `DirectDmabufExternal`.
//!
//! `--x`, `--width` and `--drift` move the video by fractional pixels, for
//! checking with screenshots that Skia draws it at sub-pixel positions rather
//! than stepping. Based on Slint v1.18.0
//! `examples/gstreamer-player/slint_video_sink/egl_integration.rs`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use gst::prelude::*;
use gst_gl::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_gl as gst_gl;
use gstreamer_gl_egl as gst_gl_egl;
use gstreamer_video as gst_video;

slint::slint! {
    export component SpikeWindow inherits Window {
        in property <image> frame;
        in property <float> display-aspect: 16 / 9;
        in property <length> video-x: 0px;
        in property <length> video-width: self.width;
        preferred-width: 960px;
        preferred-height: 540px;
        background: black;

        Image {
            source: root.frame;
            x: root.video-x;
            width: root.video-width;
            height: self.width / root.display-aspect;
            y: (parent.height - self.height) / 2;
            image-fit: fill;
        }
    }
}

/// The wrapped Slint display and context, filled in `RenderingSetup` and read
/// by the bus sync handler when an element asks for a GL context.
type GlSlot = Arc<Mutex<Option<(gst_gl::GLDisplay, gst_gl::GLContext)>>>;

/// Single-slot, latest-wins frame mailbox between appsink and the UI thread.
type Mailbox = Arc<Mutex<Option<(gst_video::VideoInfo, gst::Buffer)>>>;

struct Args {
    path: std::path::PathBuf,
    paused: bool,
    x: f32,
    width: Option<f32>,
    drift: f32,
    quit_after: Option<f64>,
}

fn parse_args() -> Args {
    let usage = "usage: zero_copy_spike <file> [--paused] [--x PX] [--width PX] \
                 [--drift PX_PER_SEC] [--quit-after SECS]";
    let mut path = None;
    let mut args = Args {
        path: Default::default(),
        paused: false,
        x: 0.0,
        width: None,
        drift: 0.0,
        quit_after: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut number = |name: &str| -> f64 {
            it.next()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("{name} needs a number\n{usage}"))
        };
        match arg.as_str() {
            "--paused" => args.paused = true,
            "--x" => args.x = number("--x") as f32,
            "--width" => args.width = Some(number("--width") as f32),
            "--drift" => args.drift = number("--drift") as f32,
            "--quit-after" => args.quit_after = Some(number("--quit-after")),
            _ if path.is_none() => path = Some(arg.into()),
            _ => panic!("unexpected argument {arg}\n{usage}"),
        }
    }
    args.path = path.unwrap_or_else(|| panic!("{usage}"));
    args
}

fn main() {
    let args = parse_args();

    // D2: Skia over EGL. FemtoVG on X11 uses GLX, which GStreamer cannot
    // import DMABuf into.
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia-opengl".into())
        .require_opengl_es()
        .select()
        .expect("unable to select Slint winit backend with the skia-opengl renderer");

    gst::init().expect("gst::init");

    let window = SpikeWindow::new().expect("create window");
    window.set_video_x(args.x);
    if let Some(width) = args.width {
        window.set_video_width(width);
    }

    let glupload = gst::ElementFactory::make("glupload").build().unwrap();
    let appsink = gst_app::AppSink::builder()
        .caps(
            &gst_video::VideoCapsBuilder::new()
                .features([gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY])
                .format(gst_video::VideoFormat::Rgba)
                .field("texture-target", "2D")
                .build(),
        )
        .enable_last_sample(false)
        .max_buffers(1u32)
        .build();
    let video_sink = gl_sink_bin(&glupload, &appsink);

    let uri = gst::glib::filename_to_uri(
        std::fs::canonicalize(&args.path).expect("video file must exist"),
        None,
    )
    .unwrap();
    let pipeline = gst::ElementFactory::make("playbin3")
        .property("uri", uri.as_str())
        .property("video-sink", &video_sink)
        .build()
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline.set_property_from_str("flags", "video+audio+soft-volume+native-video");

    // One sync handler, installed before the pipeline leaves NULL. It answers
    // GL context requests from the shared slot and forwards everything else.
    let gl_slot: GlSlot = Default::default();
    let (msg_tx, msg_rx) = mpsc::channel::<gst::Message>();
    pipeline.bus().unwrap().set_sync_handler({
        let gl_slot = gl_slot.clone();
        move |_, msg| {
            match msg.view() {
                gst::MessageView::NeedContext(need) => answer_need_context(msg, need, &gl_slot),
                _ => {
                    let _ = msg_tx.send(msg.to_owned());
                }
            }
            gst::BusSyncReply::Drop
        }
    });
    spawn_message_thread(msg_rx, pipeline.clone(), glupload.clone());

    let mailbox: Mailbox = Default::default();
    install_appsink_callbacks(&appsink, mailbox.clone(), window.as_weak());

    install_rendering_notifier(
        &window,
        pipeline.clone(),
        gl_slot,
        mailbox,
        if args.paused {
            gst::State::Paused
        } else {
            gst::State::Playing
        },
    );

    let _drift_timer = (args.drift != 0.0).then(|| {
        let timer = slint::Timer::default();
        let weak = window.as_weak();
        let tick = Duration::from_millis(16);
        let step = args.drift * tick.as_secs_f32();
        timer.start(slint::TimerMode::Repeated, tick, move || {
            if let Some(w) = weak.upgrade() {
                w.set_video_x(w.get_video_x() + step);
            }
        });
        timer
    });
    if let Some(secs) = args.quit_after {
        slint::Timer::single_shot(Duration::from_secs_f64(secs), || {
            let _ = slint::quit_event_loop();
        });
    }

    window.run().unwrap();
    let _ = pipeline.set_state(gst::State::Null);
}

/// `glupload ! glcolorconvert ! appsink` as one bin (spec D1).
fn gl_sink_bin(glupload: &gst::Element, appsink: &gst_app::AppSink) -> gst::Element {
    let convert = gst::ElementFactory::make("glcolorconvert").build().unwrap();
    let bin = gst::Bin::new();
    bin.add_many([glupload, &convert, appsink.upcast_ref()])
        .unwrap();
    gst::Element::link_many([glupload, &convert, appsink.upcast_ref()]).unwrap();
    let pad = glupload.static_pad("sink").unwrap();
    bin.add_pad(&gst::GhostPad::with_target(&pad).unwrap())
        .unwrap();
    bin.upcast()
}

fn answer_need_context(msg: &gst::Message, need: &gst::message::NeedContext, gl_slot: &GlSlot) {
    let Some(element) = msg.src().and_then(|s| s.downcast_ref::<gst::Element>()) else {
        return;
    };
    let slot = gl_slot.lock().unwrap();
    let Some((display, context)) = slot.as_ref() else {
        eprintln!(
            "NeedContext {} from {} before the GL context is ready",
            need.context_type(),
            element.name()
        );
        return;
    };
    let ctx_type = need.context_type();
    if ctx_type == *gst_gl::GL_DISPLAY_CONTEXT_TYPE {
        let ctx = gst::Context::new(ctx_type, true);
        ctx.set_gl_display(display);
        element.set_context(&ctx);
    } else if ctx_type == "gst.gl.app_context" {
        let mut ctx = gst::Context::new(ctx_type, true);
        ctx.get_mut()
            .unwrap()
            .structure_mut()
            .set("context", context);
        element.set_context(&ctx);
    }
}

/// On `new_sample` and `new_preroll` (a paused seek delivers a preroll
/// sample): set a GL sync point, store the buffer, request a redraw.
fn install_appsink_callbacks(
    appsink: &gst_app::AppSink,
    mailbox: Mailbox,
    window: slint::Weak<SpikeWindow>,
) {
    let deliver = Arc::new(
        move |sample: gst::Sample| -> Result<gst::FlowSuccess, gst::FlowError> {
            let mut buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
            let context = buffer
                .peek_memory(0)
                .downcast_memory_ref::<gst_gl::GLBaseMemory>()
                .map(|m| m.context().clone())
                .ok_or_else(|| {
                    eprintln!("appsink got non-GL memory");
                    gst::FlowError::Error
                })?;
            if let Some(meta) = buffer.meta::<gst_gl::GLSyncMeta>() {
                meta.set_sync_point(&context);
            } else {
                gst_gl::GLSyncMeta::add(buffer.make_mut(), &context).set_sync_point(&context);
            }
            let info = sample
                .caps()
                .and_then(|caps| gst_video::VideoInfo::from_caps(caps).ok())
                .ok_or(gst::FlowError::NotNegotiated)?;
            *mailbox.lock().unwrap() = Some((info, buffer));
            let _ = window.upgrade_in_event_loop(|w| w.window().request_redraw());
            Ok(gst::FlowSuccess::Ok)
        },
    );
    let on_preroll = deliver.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                deliver(sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?)
            })
            .new_preroll(move |sink| {
                on_preroll(sink.pull_preroll().map_err(|_| gst::FlowError::Flushing)?)
            })
            .build(),
    );
}

fn install_rendering_notifier(
    window: &SpikeWindow,
    pipeline: gst::Pipeline,
    gl_slot: GlSlot,
    mailbox: Mailbox,
    target_state: gst::State,
) {
    let weak = window.as_weak();
    let mut app_context: Option<gst_gl::GLContext> = None;
    // Kept mapped until the next frame replaces it: Slint draws from its texture.
    let mut current: Option<gst_gl::GLVideoFrame<gst_gl::gl_video_frame::Readable>> = None;

    window
        .window()
        .set_rendering_notifier(move |state, api| match state {
            slint::RenderingState::RenderingSetup => {
                let (display, context) = wrap_slint_egl_context(api);
                app_context = Some(context.clone());
                *gl_slot.lock().unwrap() = Some((display.upcast(), context));
                // Startup gate (D3): the pipeline leaves NULL only now, so
                // GStreamer never creates its own (GLX, unshared) context.
                pipeline
                    .set_state(target_state)
                    .expect("pipeline failed to start");
            }
            slint::RenderingState::BeforeRendering => {
                let Some((info, buffer)) = mailbox.lock().unwrap().take() else {
                    return;
                };
                let context = app_context.as_ref().unwrap();
                buffer.meta::<gst_gl::GLSyncMeta>().unwrap().wait(context);
                let Ok(frame) = gst_gl::GLVideoFrame::from_buffer_readable(buffer, &info) else {
                    eprintln!("could not map GL frame");
                    return;
                };
                let texture = frame.texture_id(0).expect("RGBA frame has a texture");
                let image = unsafe {
                    slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                        texture.try_into().expect("non-zero texture id"),
                        [frame.width(), frame.height()].into(),
                    )
                    .build()
                };
                // D3: display size from the negotiated caps, not a forced PAR.
                let par = info.par();
                let aspect = frame.width() as f64 * par.numer() as f64
                    / (frame.height() as f64 * par.denom() as f64);
                let w = weak.unwrap();
                w.set_display_aspect(aspect as f32);
                w.set_frame(image);
                current.replace(frame);
            }
            slint::RenderingState::RenderingTeardown => {
                // GStreamer must stop using the shared context before it goes.
                let _ = pipeline.set_state(gst::State::Null);
                current.take();
                if let Some(context) = app_context.take() {
                    let _ = context.activate(false);
                }
            }
            _ => {}
        })
        .expect("rendering notifier (is the renderer OpenGL?)");
}

/// Wraps Slint's current EGL display and context for GStreamer. Fails loudly
/// if there is no EGL context: never continue on a copying path (D2).
fn wrap_slint_egl_context(
    api: &slint::GraphicsAPI<'_>,
) -> (gst_gl_egl::GLDisplayEGL, gst_gl::GLContext) {
    let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
        panic!("skia-opengl renderer did not provide a native OpenGL API");
    };
    let egl = glutin_egl_sys::egl::Egl::load_with(|symbol| {
        get_proc_address(&std::ffi::CString::new(symbol).unwrap())
    });
    let platform = gst_gl::GLPlatform::EGL;
    unsafe {
        let native_context = egl.GetCurrentContext();
        assert!(
            !native_context.is_null(),
            "eglGetCurrentContext() is null: the skia-opengl renderer is not on EGL \
             (or fell back to software). Refusing to continue on a copying path."
        );
        let display = gst_gl_egl::GLDisplayEGL::with_egl_display(egl.GetCurrentDisplay() as usize)
            .expect("wrap EGL display");
        let context = gst_gl::GLContext::new_wrapped(
            &display,
            native_context as usize,
            platform,
            gst_gl::GLContext::current_gl_api(platform).0,
        )
        .expect("wrap EGL context");
        context.activate(true).expect("activate wrapped context");
        context.fill_info().expect("fill wrapped context info");
        eprintln!(
            "[spike] Slint GL context: platform {:?}, api {:?}",
            context.gl_platform(),
            context.gl_api()
        );
        (display, context)
    }
}

/// Stands in for the bus thread: logs diagnostics on each `ASYNC_DONE`,
/// counts QoS (dropped-frame) messages, and quits on EOS or error.
fn spawn_message_thread(
    rx: mpsc::Receiver<gst::Message>,
    pipeline: gst::Pipeline,
    glupload: gst::Element,
) {
    let qos = AtomicU32::new(0);
    std::thread::spawn(move || {
        let quit = || {
            let _ = slint::invoke_from_event_loop(|| {
                let _ = slint::quit_event_loop();
            });
        };
        for msg in rx {
            match msg.view() {
                gst::MessageView::AsyncDone(_) => log_diagnostics(&pipeline, &glupload),
                gst::MessageView::Qos(q) => {
                    let n = qos.fetch_add(1, Ordering::Relaxed) + 1;
                    let (processed, dropped) = q.stats();
                    eprintln!(
                        "[spike] QoS #{n} from {}: processed {processed}, dropped {dropped}",
                        msg.src().map(|s| s.name()).unwrap_or_default()
                    );
                }
                gst::MessageView::Warning(w) => eprintln!(
                    "[spike] WARNING from {}: {} ({:?})",
                    msg.src().map(|s| s.path_string()).unwrap_or_default(),
                    w.error(),
                    w.debug()
                ),
                gst::MessageView::Error(e) => {
                    eprintln!(
                        "[spike] ERROR from {}: {} ({:?})",
                        msg.src().map(|s| s.path_string()).unwrap_or_default(),
                        e.error(),
                        e.debug()
                    );
                    quit();
                }
                gst::MessageView::Eos(_) => {
                    eprintln!("[spike] EOS; QoS messages: {}", qos.load(Ordering::Relaxed));
                    quit();
                }
                _ => {}
            }
        }
    });
}

/// D12: the selected decoder, the caps on `glupload`'s sink pad, and the GL
/// platform `glupload` actually uses.
fn log_diagnostics(pipeline: &gst::Pipeline, glupload: &gst::Element) {
    let decoders: Vec<String> = pipeline
        .iterate_recurse()
        .into_iter()
        .flatten()
        .filter_map(|e| e.factory())
        .filter(|f| {
            let klass = f.klass();
            klass.contains("Decoder") && klass.contains("Video")
        })
        .map(|f| f.name().to_string())
        .collect();
    eprintln!("[spike] video decoder: {decoders:?}");

    let caps = glupload
        .static_pad("sink")
        .and_then(|p| p.current_caps())
        .map(|c| c.to_string())
        .unwrap_or_else(|| "<none>".into());
    eprintln!("[spike] glupload sink caps: {caps}");

    match glupload.property::<Option<gst_gl::GLContext>>("context") {
        Some(ctx) => eprintln!(
            "[spike] glupload GL context: platform {:?}, display {:?}",
            ctx.gl_platform(),
            ctx.display().handle_type()
        ),
        None => eprintln!("[spike] glupload GL context: <none>"),
    }
}
