//! The injected video sink, which fills the bus's [`FrameMailbox`]
//! (spec D1, D3).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_gl as gst_gl;
use gstreamer_video as gst_video;

use crate::mailbox::{Frame, FrameMailbox};

/// Which sinks a [`SourcePlayer`](super::SourcePlayer) builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkKind {
    /// Production. Video is `glupload ! glcolorconvert ! appsink` with
    /// GL-memory RGBA 2D caps, the zero-copy display path; audio is
    /// `autoaudiosink`, which [`keep_pulsesink_out`] steers. A player built with it stays in NULL until
    /// [`SourcePlayer::set_gl_context`](super::SourcePlayer::set_gl_context).
    Gl,
    /// Headless tests. Video is a system-memory `appsink` (no GL, no
    /// display); audio is `fakesink sync=true`, so playback still runs in real
    /// time without a sound device.
    System,
}

/// A built video sink and the handles the player keeps into it.
pub(super) struct VideoSink {
    pub element: gst::Element,
    /// `glupload` inside a [`SinkKind::Gl`] sink; `None` for
    /// [`SinkKind::System`].
    pub glupload: Option<gst::Element>,
}

/// Builds the video sink for `kind`, delivering into `mailbox`.
///
/// Every sample is pulled, so the sink always reaches EOS (an appsink whose
/// samples are never pulled never posts it).
pub(super) fn video_sink(kind: SinkKind, mailbox: FrameMailbox) -> VideoSink {
    let appsink = gst_app::AppSink::builder()
        .caps(&match kind {
            SinkKind::Gl => gl_caps(),
            // Any system-memory layout: the decoder picks (NV12 from
            // `vavp8dec`, I420 from `vp8dec`), so tests never assert on it.
            SinkKind::System => gst::Caps::builder("video/x-raw").build(),
        })
        .enable_last_sample(false)
        .max_buffers(1u32)
        .build();
    fill_mailbox(&appsink, mailbox, || {});

    match kind {
        SinkKind::System => VideoSink {
            element: appsink.upcast(),
            glupload: None,
        },
        SinkKind::Gl => {
            let (element, glupload) = gl_bin(&appsink);
            VideoSink {
                element,
                glupload: Some(glupload),
            }
        }
    }
}

/// Keeps `pulsesink` out of `autoaudiosink`'s choice, process-wide, so that
/// on a desktop the speakers are `alsasink` on ALSA's default device — which
/// on the PipeWire desktop this targets is PipeWire itself (`pipewire-alsa`).
/// `autoaudiosink` keeps its fallback to a fake sink where nothing opens (CI).
/// Idempotent; call it after `gst::init`.
///
/// **Why.** `pulsesink` against PipeWire 1.0's pulse server (Ubuntu 24.04's)
/// wedges for good once flushing seeks come quickly while playing — a
/// dragged scrubber, a held skip key. The server stops asking for audio (it
/// logs `OVERFLOW`), the sink blocks with its ring buffer full, and the
/// picture and the clock stop with it; pause and play don't restart it. A
/// click, a toggle or a scrub while paused never did it. `pipewiresink` 1.0
/// stalls the same way; `alsasink` doesn't. Measured on the reference
/// laptop by `real_footage_keeps_playing_through_seeks_while_playing`.
pub fn keep_pulsesink_out() {
    if let Some(pulse) = gst::Registry::get().lookup_feature("pulsesink") {
        pulse.set_rank(gst::Rank::NONE);
    }
}

/// The audio sink that goes with `kind` (see [`SinkKind`]).
pub(super) fn audio_sink(kind: SinkKind) -> gst::Element {
    let builder = match kind {
        SinkKind::Gl => gst::ElementFactory::make("autoaudiosink"),
        SinkKind::System => gst::ElementFactory::make("fakesink").property("sync", true),
    };
    builder
        .build()
        .expect("audio sink is missing (gst-plugins-base/good)")
}

/// What a GL sink's appsink accepts: RGBA 2D textures in GL memory.
pub(crate) fn gl_caps() -> gst::Caps {
    gst_video::VideoCapsBuilder::new()
        .features([gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY])
        .format(gst_video::VideoFormat::Rgba)
        .field("texture-target", "2D")
        .build()
}

/// `glupload ! glcolorconvert ! appsink` as one bin, ghosting `glupload`'s
/// sink pad (spec D1). Returns the bin and its `glupload`. `appsink` should
/// accept [`gl_caps`]. Export's decode pipeline uses it too.
pub(crate) fn gl_bin(appsink: &gst_app::AppSink) -> (gst::Element, gst::Element) {
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
    (bin.upcast(), upload)
}

/// Makes `appsink` fill `mailbox`, calling `on_sample` for each sample
/// delivered while running — never for a preroll, which is a frame reached
/// while paused and not one the composite produced.
///
/// The preview's tail uses this too, so a scrub while paused puts the frame
/// it lands on up: without the preroll half, a paused seek shows nothing.
pub(crate) fn fill_mailbox(
    appsink: &gst_app::AppSink,
    mailbox: FrameMailbox,
    on_sample: impl Fn() + Send + Sync + 'static,
) {
    let deliver = move |sample: gst::Sample| -> Result<gst::FlowSuccess, gst::FlowError> {
        mailbox.put(Frame::from_sample(sample)?);
        Ok(gst::FlowSuccess::Ok)
    };
    let deliver = Arc::new(deliver);
    let on_preroll = deliver.clone();

    // Set by a flush or a new stream, cleared by the first sample after it.
    // PLAYING→PAUSED prerolls the frame after the displayed one, while a
    // flushing seek (even one while PLAYING) prerolls the frame it landed on:
    // only the latter is shown.
    let fresh = Arc::new(AtomicBool::new(true));
    appsink
        .static_pad("sink")
        .expect("appsink has a sink pad")
        .add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::EVENT_FLUSH,
            {
                let fresh = fresh.clone();
                move |_, info| {
                    if let Some(gst::PadProbeData::Event(ev)) = &info.data {
                        if matches!(
                            ev.type_(),
                            gst::EventType::FlushStop | gst::EventType::StreamStart
                        ) {
                            fresh.store(true, Ordering::SeqCst);
                        }
                    }
                    gst::PadProbeReturn::Ok
                }
            },
        );
    let preroll_fresh = fresh.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let result = deliver(sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?);
                fresh.store(false, Ordering::SeqCst);
                if result.is_ok() {
                    on_sample();
                }
                result
            })
            .new_preroll(move |sink| {
                if !preroll_fresh.load(Ordering::SeqCst) {
                    return Ok(gst::FlowSuccess::Ok);
                }
                on_preroll(sink.pull_preroll().map_err(|_| gst::FlowError::Flushing)?)
            })
            .build(),
    );
}
