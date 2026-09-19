//! The decode side: the source through the player's GL bin into a pull
//! `appsink`, and the lookup "last frame at or before this source time".

use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::{start_failure, ExportError, SharedGl, Stopper, Watch, POLL};
use crate::player::{diagnostics, gl_bin, gl_caps, Diagnostics};

/// A target at most this far ahead of the current frame is reached by
/// pulling forward (~1.3 ms a frame) rather than an accurate seek (12 ms on
/// camera footage, up to ~100 ms on a 2 s GOP). Measured.
const PULL_AHEAD: gst::ClockTime = gst::ClockTime::from_mseconds(500);

/// One decoded frame and its source time.
struct Decoded {
    buffer: gst::Buffer,
    /// **Stream** time, not PTS: an MP4 edit list (B-frame delay) starts the
    /// segment after 0, and raw PTS then runs two frames ahead of the time the
    /// player shows, which is what the schedule's times are.
    time: gst::ClockTime,
}

pub(super) struct Decoder {
    pipeline: Stopper,
    appsink: gst_app::AppSink,
    glupload: gst::Element,
    /// The latest sample's caps.
    caps: Option<gst::Caps>,
    /// The frame `frame_at` last answered with.
    current: Option<Decoded>,
    /// The frame after `current`, pulled to learn that `current` is still the
    /// last one at or before a target.
    next: Option<Decoded>,
    /// The stream ended after `current`.
    eos: bool,
}

impl Decoder {
    /// Builds the pipeline, prerolls it, and sets it PLAYING. Its bus joins
    /// `watch`.
    pub(super) fn start(
        source: &Path,
        gl: &SharedGl,
        watch: &mut Watch,
    ) -> Result<Decoder, ExportError> {
        let pipeline = gst::Pipeline::new();
        let make = |factory: &str| {
            gst::ElementFactory::make(factory)
                .build()
                .map_err(|e| ExportError::Failed(format!("{factory} is missing: {e}")))
        };
        let filesrc = make("filesrc")?;
        filesrc.set_property("location", source);
        let decodebin = make("decodebin3")?;
        let appsink = gst_app::AppSink::builder()
            .caps(&gl_caps())
            .sync(false)
            .max_buffers(2u32)
            .enable_last_sample(false)
            .build();
        let (gl_sink, glupload) = gl_bin(&appsink);
        pipeline
            .add_many([&filesrc, &decodebin, &gl_sink])
            .expect("add decode elements");
        filesrc
            .link(&decodebin)
            .expect("link filesrc to decodebin3");
        // The video stream only. Other pads (audio) stay unlinked, which
        // `decodebin3` tolerates, across seeks too (measured).
        decodebin.connect_pad_added(move |_, pad| {
            let sink = gl_sink.static_pad("sink").expect("the GL bin has a sink");
            if pad.name().starts_with("video_") && !sink.is_linked() {
                let _ = pad.link(&sink);
            }
        });
        gl.install(&pipeline, &[gst::MessageType::Error]);
        watch
            .buses
            .push(pipeline.bus().expect("a pipeline has a bus"));
        let pipeline = Stopper(pipeline);

        // Preroll first: a seek before the stream is up is dropped.
        if pipeline.0.set_state(gst::State::Paused).is_err() {
            return Err(start_failure(&pipeline.0, "could not open the source"));
        }
        loop {
            watch.check()?;
            match pipeline.0.state(POLL) {
                (Ok(_), gst::State::Paused, gst::State::VoidPending) => break,
                (Err(_), ..) => {
                    watch.check()?;
                    return Err(ExportError::Failed("could not read the source".into()));
                }
                _ => {}
            }
        }
        if pipeline.0.set_state(gst::State::Playing).is_err() {
            return Err(start_failure(&pipeline.0, "could not play the source"));
        }
        Ok(Decoder {
            pipeline,
            appsink,
            glupload,
            caps: None,
            current: None,
            next: None,
            eos: false,
        })
    }

    /// The last frame with stream time at or before `target` (or the first
    /// frame, for a target before it).
    ///
    /// Reuses the current frame while it still answers, pulls forward to a
    /// target up to [`PULL_AHEAD`] ahead, and seeks otherwise. It never seeks
    /// backwards to a target at or after the current frame: a freeze, or a
    /// 60 → 30 fps drop, costs nothing.
    pub(super) fn frame_at(
        &mut self,
        target: gst::ClockTime,
        watch: &Watch,
    ) -> Result<gst::Buffer, ExportError> {
        let behind = self.current.as_ref().is_none_or(|c| target < c.time);
        let answered = !behind && (self.eos || self.next.as_ref().is_some_and(|n| n.time > target));
        if !answered {
            let near = self
                .current
                .as_ref()
                .is_some_and(|c| target >= c.time && target - c.time <= PULL_AHEAD);
            if !near {
                self.seek(target)?;
            }
            loop {
                if self.next.is_none() {
                    self.next = self.pull(watch)?;
                    if self.next.is_none() {
                        self.eos = true;
                        break;
                    }
                }
                let next_time = self.next.as_ref().map(|n| n.time);
                if self.current.is_none() || next_time.is_some_and(|t| t <= target) {
                    self.current = self.next.take();
                } else {
                    break;
                }
            }
        }
        self.current
            .as_ref()
            .map(|c| c.buffer.clone())
            .ok_or_else(|| ExportError::Failed(format!("the source has no frame at {target}")))
    }

    /// The latest decoded frame's caps: once `frame_at` has answered, those
    /// of the frames it returns.
    pub(super) fn caps(&self) -> &gst::Caps {
        self.caps
            .as_ref()
            .expect("caps are known once frame_at has returned a frame")
    }

    pub(super) fn diagnostics(&self) -> Diagnostics {
        diagnostics(&self.pipeline.0, Some(&self.glupload))
    }

    /// An accurate, flushing seek. The frames held are from before it.
    fn seek(&mut self, target: gst::ClockTime) -> Result<(), ExportError> {
        self.current = None;
        self.next = None;
        self.eos = false;
        self.pipeline
            .0
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, target)
            .map_err(|_| ExportError::Failed(format!("the source refused a seek to {target}")))
    }

    /// The next decoded frame, or `None` at the end of the stream. Waits in
    /// [`POLL`] steps, checking `watch` between them.
    fn pull(&mut self, watch: &Watch) -> Result<Option<Decoded>, ExportError> {
        loop {
            if let Some(sample) = self.appsink.try_pull_sample(POLL) {
                let buffer = sample
                    .buffer_owned()
                    .ok_or_else(|| ExportError::Failed("a decoded sample has no buffer".into()))?;
                let pts = buffer.pts().ok_or_else(|| {
                    ExportError::Failed("a decoded frame has no timestamp".into())
                })?;
                let segment = sample
                    .segment()
                    .and_then(|s| s.downcast_ref::<gst::ClockTime>())
                    .ok_or_else(|| {
                        ExportError::Failed("a decoded frame has no time segment".into())
                    })?;
                // Negative only for a frame straddling the segment start,
                // which then is the first frame: 0 orders it correctly.
                let time = match segment.to_stream_time_full(pts) {
                    Some(gst::Signed::Positive(t)) => t,
                    _ => gst::ClockTime::ZERO,
                };
                self.caps = sample.caps_owned();
                return Ok(Some(Decoded { buffer, time }));
            }
            if self.appsink.is_eos() {
                return Ok(None);
            }
            watch.check()?;
        }
    }
}
