//! The single-slot handoff from an appsink's streaming thread to whoever draws
//! (spec D3, and P1).
//!
//! There is **one mailbox per app**, owned by the bus and cloned into the
//! source player and into every preview: both fill the same slot, so the UI
//! draws whichever is playing without knowing which that is. The bus
//! guarantees only one of them is ever PLAYING, and empties the slot when it
//! swaps them, so no frame of the one just stopped stays on screen.
//!
//! The recorder's live self-view has a second mailbox of its own, with small
//! RGBA frames in system memory rather than GL textures.

use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer_gl as gst_gl;
use gstreamer_video as gst_video;

/// One decoded frame: the buffer, the negotiated `VideoInfo` it is laid out
/// by, which carries the pixel aspect ratio, and where it is in its stream.
///
/// From a GL appsink the buffer is GL memory carrying a `GLSyncMeta` whose
/// sync point is already set; wait on it before sampling the texture.
#[derive(Debug, Clone)]
pub struct Frame {
    pub buffer: gst::Buffer,
    pub info: gst_video::VideoInfo,
    /// Seconds into the stream that produced it, from the sample's segment,
    /// by the rule export picks its frames by ([`stream_time`]): a time
    /// inside this frame, so export's decoder picks this frame for it. It is
    /// the frame's start, except for the frame a seek lands inside: the
    /// decoder clips that one to the seek's target. A preview's frame is in
    /// output time. `None` without a PTS or a time segment.
    pub stream_time: Option<f64>,
    /// Where the frame ends, in the same time: which frame this is, even when
    /// its start was clipped. `None` without a duration too.
    pub stream_end: Option<f64>,
}

impl Frame {
    /// `sample` as a frame, with a GL sync point set on GL memory so the
    /// drawing context can wait for the producing pipeline to finish. System
    /// memory needs nothing.
    pub(crate) fn from_sample(sample: gst::Sample) -> Result<Frame, gst::FlowError> {
        let seconds = |t: gst::ClockTime| t.nseconds() as f64 / 1e9;
        let (stream_time, stream_end) = (
            stream_time(&sample).map(seconds),
            stream_end(&sample).map(seconds),
        );
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
        Ok(Frame {
            buffer,
            info,
            stream_time,
            stream_end,
        })
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

/// `sample`'s **stream** time, not its PTS: an MP4 edit list (B-frame delay)
/// starts the segment after 0, and raw PTS then runs two frames ahead of the
/// time the player shows. `None` without a PTS or a time segment.
///
/// The one rule for "where is this frame": export's decoder picks frames by
/// it and the scan player's frames carry it, so the two can be compared.
pub(crate) fn stream_time(sample: &gst::Sample) -> Option<gst::ClockTime> {
    to_stream_time(sample, sample.buffer()?.pts()?)
}

/// Where `sample`'s frame ends, in [`stream_time`]. A seek's clipping moves a
/// frame's start and never its end, so this names the frame.
pub(crate) fn stream_end(sample: &gst::Sample) -> Option<gst::ClockTime> {
    let buffer = sample.buffer()?;
    to_stream_time(sample, buffer.pts()? + buffer.duration()?)
}

/// Timestamp `t` in `sample`'s segment as stream time. A time before the
/// segment's start (a frame straddling it) reads 0, which orders that frame
/// correctly as the first.
fn to_stream_time(sample: &gst::Sample, t: gst::ClockTime) -> Option<gst::ClockTime> {
    let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
    Some(match segment.to_stream_time_full(t) {
        Some(gst::Signed::Positive(t)) => t,
        _ => gst::ClockTime::ZERO,
    })
}
