//! The live self-view: the camera, small, while it records, so the coach sees
//! their framing and lighting before the take is over.
//!
//! **The recording is the product and this is a convenience, so nothing here
//! can reach the recording.** The self-view is a **second pipeline**, fed from
//! a pad probe on the recorder's camera caps (the tee) into an `appsrc` that
//! holds one buffer and drops the oldest (the leaky queue). The probe only
//! takes a reference and hands it over, and neither can block:
//!
//! - A slow or stopped display drops frames at the `appsrc`. The push never
//!   waits (`block=false`), so the camera's thread never does.
//! - The display's flow returns, caps and allocation queries, preroll, EOS
//!   and errors all stay in its own pipeline. On a `tee` in the recording's
//!   pipeline, a display element's error or `not-negotiated` would come back
//!   through the tee on the next frame and stop the camera, a display
//!   element that failed would post the ERROR that ends the recording, and
//!   `Recorder::stop` would wait for EOS to reach the display's sink too.
//! - The recording's buffers are shared, never written: a display element
//!   that wanted one writable would copy it first. `v4l2src` copies out of
//!   its own pool when downstream holds too many, so the one or two frames
//!   held here can't starve the camera.
//!
//! An error here is logged and the frames stop; the UI hides a self-view
//! that has gone quiet.

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::devices::Input;
use crate::error_text;
use crate::mailbox::{Frame, FrameMailbox};

/// The frames' width. The inset is 22% of the output's width: 422 px at
/// 1080p, and less than that on screen. The height follows the camera.
const WIDTH: i32 = 480;

/// Starts showing what reaches `camera` (the recorder's camera caps: JPEG or
/// raw, per `input`) in `mailbox`, as small RGBA frames in system memory, and
/// returns the self-view's pipeline, PLAYING. Its owner sets it to NULL
/// after the camera's: until then the probe may still push into it.
///
/// The probe holds only the `appsrc`, nothing of the recording's, so it
/// makes no reference cycle with `camera`: it goes when the recording's
/// pipeline does.
pub(super) fn start(
    camera: &gst::Pad,
    input: Input,
    mailbox: FrameMailbox,
) -> Result<gst::Pipeline, String> {
    let (pipeline, src) =
        build(input, mailbox).map_err(|e| format!("could not build the self-view: {e}"))?;
    pipeline
        .bus()
        .expect("a pipeline has a bus")
        .set_sync_handler(|_, msg| {
            if let gst::MessageView::Error(err) = msg.view() {
                eprintln!("recorder: the self-view failed: {}", error_text(err));
            }
            // Nothing pops this bus.
            gst::BusSyncReply::Drop
        });
    if pipeline.set_state(gst::State::Playing).is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return Err("the self-view failed to start".into());
    }

    // The tee: a reference, into a queue that drops rather than waits. The
    // caps go with every buffer, from the pad it crossed, so no buffer can
    // reach the appsrc ahead of them; the appsrc ignores caps equal to its
    // last. What the push returns (FLUSHING once the self-view has stopped)
    // is the self-view's business.
    camera.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        if let Some(buffer) = info.buffer() {
            src.set_caps(pad.current_caps().as_ref());
            let _ = src.push_buffer(buffer.clone());
        }
        gst::PadProbeReturn::Ok
    });
    Ok(pipeline)
}

/// `appsrc ! [jpegdec] ! videoconvertscale ! RGBA, WIDTH wide ! appsink`, in
/// NULL, and its `appsrc`.
fn build(
    input: Input,
    mailbox: FrameMailbox,
) -> Result<(gst::Pipeline, gst_app::AppSrc), glib::BoolError> {
    let pipeline = gst::Pipeline::with_name("self-view");
    let src = gst_app::AppSrc::builder()
        .name("self-view-src")
        .format(gst::Format::Time)
        .is_live(true)
        // The leaky queue: one frame, the newest. Bounded by buffers alone,
        // so no byte or time limit can make it hold more, or less.
        .max_buffers(1)
        .max_bytes(0)
        .leaky_type(gst_app::AppLeakyType::Downstream)
        .block(false)
        .build();
    let scale = gst::ElementFactory::make("videoconvertscale").build()?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .field("width", WIDTH)
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build(),
        )
        .build()?;
    let sink = gst_app::AppSink::builder()
        .name("self-view-sink")
        // Shown on arrival: the display isn't a clock, and a late frame is
        // still the newest there is.
        .sync(false)
        .max_buffers(1)
        .drop(true)
        .enable_last_sample(false)
        .callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    mailbox.put(Frame::from_sample(sample)?);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        )
        .build();

    let mut chain: Vec<gst::Element> = vec![src.clone().upcast()];
    if input == Input::Mjpeg {
        // A corrupt JPEG from the webcam skips a frame rather than ending
        // the self-view.
        chain.push(
            gst::ElementFactory::make("jpegdec")
                .property("max-errors", -1i32)
                .build()?,
        );
    }
    chain.extend([scale, caps, sink.upcast()]);
    pipeline.add_many(&chain)?;
    gst::Element::link_many(&chain)?;
    Ok((pipeline, src))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gstreamer_video as gst_video;

    use super::*;

    /// A webcam's MJPEG (the recorder's usual input) comes out decoded,
    /// RGBA and WIDTH wide, keeping its 16:9.
    #[test]
    fn mjpeg_is_decoded_to_small_rgba() {
        gst::init().unwrap();
        let camera = gst::parse::launch(
            "videotestsrc is-live=true ! video/x-raw,width=1280,height=720 \
             ! jpegenc name=jpeg ! fakesink",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let pad = camera.by_name("jpeg").unwrap().static_pad("src").unwrap();
        let mailbox = FrameMailbox::default();
        let view = start(&pad, Input::Mjpeg, mailbox.clone()).unwrap();
        camera.set_state(gst::State::Playing).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let frame = loop {
            if let Some(frame) = mailbox.take() {
                break frame;
            }
            assert!(Instant::now() < deadline, "no self-view frame");
            std::thread::sleep(Duration::from_millis(10));
        };
        camera.set_state(gst::State::Null).unwrap();
        view.set_state(gst::State::Null).unwrap();

        assert_eq!((frame.info.width(), frame.info.height()), (480, 270));
        assert_eq!(frame.info.format(), gst_video::VideoFormat::Rgba);
    }
}
