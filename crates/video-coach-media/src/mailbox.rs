//! The single-slot handoff from an appsink's streaming thread to whoever draws
//! (spec D3, and P1).
//!
//! There is **one mailbox per app**, owned by the bus and cloned into the
//! source player and into every preview: both fill the same slot, so the UI
//! draws whichever is playing without knowing which that is. The bus
//! guarantees only one of them is ever PLAYING, and empties the slot when it
//! swaps them, so no frame of the one just stopped stays on screen.

use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer_gl as gst_gl;
use gstreamer_video as gst_video;

/// One decoded frame: the buffer and the negotiated `VideoInfo` it is laid out
/// by, which carries the pixel aspect ratio.
///
/// From a GL appsink the buffer is GL memory carrying a `GLSyncMeta` whose
/// sync point is already set; wait on it before sampling the texture.
#[derive(Debug, Clone)]
pub struct Frame {
    pub buffer: gst::Buffer,
    pub info: gst_video::VideoInfo,
}

impl Frame {
    /// `sample` as a frame, with a GL sync point set on GL memory so the
    /// drawing context can wait for the producing pipeline to finish. System
    /// memory needs nothing.
    pub(crate) fn from_sample(sample: gst::Sample) -> Result<Frame, gst::FlowError> {
        let mut buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
        let info = sample
            .caps()
            .and_then(|caps| gst_video::VideoInfo::from_caps(caps).ok())
            .ok_or(gst::FlowError::NotNegotiated)?;
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
        Ok(Frame { buffer, info })
    }
}

type Redraw = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct MailboxInner {
    frame: Mutex<Option<Frame>>,
    redraw: Mutex<Option<Redraw>>,
}

/// Single-slot, latest-wins handoff from an appsink's streaming thread to
/// whoever draws. Every `new_sample` fills it. A preroll fills it only when it
/// is the first frame since a flush or a new stream (a seek or a load), so a
/// frame reached by a seek while paused arrives too. A pause's preroll is the
/// *next* frame while the position stays on the displayed one, so it is not
/// shown (spec R10). Cheap to clone; clones share the slot.
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

    pub(crate) fn put(&self, frame: Frame) {
        *self.inner.frame.lock().unwrap() = Some(frame);
        // Cloned out so the callback never runs under the lock.
        let redraw = self.inner.redraw.lock().unwrap().clone();
        if let Some(redraw) = redraw {
            redraw();
        }
    }
}
