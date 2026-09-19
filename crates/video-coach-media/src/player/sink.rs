//! The injected video sink and the frame mailbox it fills (spec D1, D3).

use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_gl as gst_gl;
use gstreamer_video as gst_video;

/// Which video sink [`video_sink`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    /// `glupload ! glcolorconvert ! appsink` with GL-memory RGBA 2D caps: the
    /// zero-copy display path. A player built with it stays in NULL until
    /// [`SourcePlayer::set_gl_context`](super::SourcePlayer::set_gl_context).
    Gl,
    /// A system-memory `appsink`, for headless tests: no GL, no display.
    System,
}

/// One decoded frame: the buffer and the negotiated `VideoInfo` it is laid out
/// by, which carries the pixel aspect ratio.
///
/// For [`SinkKind::Gl`] the buffer is GL memory carrying a `GLSyncMeta` whose
/// sync point is already set; wait on it before sampling the texture.
#[derive(Debug, Clone)]
pub struct Frame {
    pub buffer: gst::Buffer,
    pub info: gst_video::VideoInfo,
}

type Redraw = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct MailboxInner {
    frame: Mutex<Option<Frame>>,
    redraw: Mutex<Option<Redraw>>,
}

/// Single-slot, latest-wins handoff from the appsink's streaming thread to
/// whoever draws. Both `new_sample` and `new_preroll` fill it, so a frame
/// reached by a seek while paused arrives too. Cheap to clone; clones share
/// the slot.
#[derive(Clone, Default)]
pub struct FrameMailbox {
    inner: Arc<MailboxInner>,
}

impl FrameMailbox {
    /// Takes the newest frame, leaving the slot empty.
    pub fn take(&self) -> Option<Frame> {
        self.inner.frame.lock().unwrap().take()
    }

    /// Called on the streaming thread after every new frame, e.g. to request
    /// a redraw. Replaces any earlier callback.
    pub fn set_redraw(&self, redraw: impl Fn() + Send + Sync + 'static) {
        *self.inner.redraw.lock().unwrap() = Some(Arc::new(redraw));
    }

    fn put(&self, frame: Frame) {
        *self.inner.frame.lock().unwrap() = Some(frame);
        // Cloned out so the callback never runs under the lock.
        let redraw = self.inner.redraw.lock().unwrap().clone();
        if let Some(redraw) = redraw {
            redraw();
        }
    }
}

/// Builds the video sink to inject into [`SourcePlayer::new`](super::SourcePlayer::new),
/// and the mailbox it delivers frames to.
///
/// Every sample is pulled, so the sink always reaches EOS (an appsink whose
/// samples are never pulled never posts it).
pub fn video_sink(kind: SinkKind) -> (gst::Element, FrameMailbox) {
    let mailbox = FrameMailbox::default();
    let appsink = gst_app::AppSink::builder()
        .caps(&match kind {
            SinkKind::Gl => gst_video::VideoCapsBuilder::new()
                .features([gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY])
                .format(gst_video::VideoFormat::Rgba)
                .field("texture-target", "2D")
                .build(),
            // Any system-memory layout: the decoder picks (NV12 from
            // `vavp8dec`, I420 from `vp8dec`), so tests never assert on it.
            SinkKind::System => gst::Caps::builder("video/x-raw").build(),
        })
        .enable_last_sample(false)
        .max_buffers(1u32)
        .build();
    install_callbacks(&appsink, mailbox.clone());

    let element = match kind {
        SinkKind::System => appsink.upcast(),
        SinkKind::Gl => gl_bin(&appsink),
    };
    (element, mailbox)
}

/// `glupload ! glcolorconvert ! appsink` as one bin, ghosting `glupload`'s
/// sink pad (spec D1).
fn gl_bin(appsink: &gst_app::AppSink) -> gst::Element {
    let make = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .unwrap_or_else(|e| panic!("{name} is missing (gst-plugins-base GL): {e}"))
    };
    let upload = make("glupload");
    let convert = make("glcolorconvert");
    let bin = gst::Bin::new();
    let chain = [&upload, &convert, appsink.upcast_ref()];
    bin.add_many(chain).expect("add GL sink elements");
    gst::Element::link_many(chain).expect("link GL sink elements");
    let pad = upload.static_pad("sink").expect("glupload has a sink pad");
    bin.add_pad(&gst::GhostPad::with_target(&pad).expect("ghost glupload sink"))
        .expect("add ghost pad");
    bin.upcast()
}

fn install_callbacks(appsink: &gst_app::AppSink, mailbox: FrameMailbox) {
    let deliver = move |sample: gst::Sample| -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
        let info = sample
            .caps()
            .and_then(|caps| gst_video::VideoInfo::from_caps(caps).ok())
            .ok_or(gst::FlowError::NotNegotiated)?;
        // GL memory: set a sync point so the drawing context can wait for
        // `glcolorconvert` to finish. System memory needs nothing.
        let gl_context = buffer
            .peek_memory(0)
            .downcast_memory_ref::<gst_gl::GLBaseMemory>()
            .map(|m| m.context().clone());
        if let Some(context) = gl_context {
            if let Some(meta) = buffer.meta::<gst_gl::GLSyncMeta>() {
                meta.set_sync_point(&context);
            } else {
                gst_gl::GLSyncMeta::add(buffer.make_mut(), &context).set_sync_point(&context);
            }
        }
        mailbox.put(Frame { buffer, info });
        Ok(gst::FlowSuccess::Ok)
    };
    let deliver = Arc::new(deliver);
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
