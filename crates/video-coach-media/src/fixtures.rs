//! Synthetic media files for tests.
//!
//! Every function writes into a directory the caller supplies, so the caller
//! owns cleanup (normally a `tempfile::TempDir`) and this crate carries no
//! test-only dependency. The pipelines use only the GStreamer base and good
//! plugin sets, which is what CI installs.
//!
//! These are test helpers, so they **panic** on any failure — a pipeline that
//! fails to build, posts an `ERROR`, or does not reach EOS within
//! [`TIMEOUT`]. A fixture that silently came out short would make every
//! assertion built on it meaningless.

use std::path::{Path, PathBuf};

use gstreamer as gst;
use gstreamer::prelude::*;

/// How long a fixture pipeline may run before it is declared hung.
pub const TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(30);

/// Audio sample rate for fixtures that carry audio.
const AUDIO_RATE: u32 = 44_100;

/// A VP8 + Vorbis WebM of `secs` seconds at `w`×`h`, `fps` frames per second,
/// with a keyframe at least every `keyint` frames. Written to `dir/name`.
///
/// The audio is sized to exactly the video duration — one audio buffer of
/// `AUDIO_RATE / fps` samples per video frame. Left at `audiotestsrc`'s
/// default buffer size, the audio track runs long and the container duration
/// with it.
///
/// Panics if `fps` does not divide 44 100 (e.g. 25, 30 and 60 do).
pub fn webm(dir: &Path, name: &str, secs: u32, w: u32, h: u32, fps: u32, keyint: u32) -> PathBuf {
    assert!(
        fps > 0 && AUDIO_RATE.is_multiple_of(fps),
        "fps {fps} must divide {AUDIO_RATE} so the audio matches the video duration"
    );
    let frames = secs * fps;
    let samples_per_buffer = AUDIO_RATE / fps;
    let pipeline = format!(
        "videotestsrc num-buffers={frames} \
           ! video/x-raw,width={w},height={h},framerate={fps}/1 \
           ! vp8enc deadline=1 keyframe-max-dist={keyint} ! queue ! mux. \
         audiotestsrc num-buffers={frames} samplesperbuffer={samples_per_buffer} \
           ! audio/x-raw,rate={AUDIO_RATE},channels=1 \
           ! audioconvert ! vorbisenc ! queue ! mux. \
         webmmux name=mux ! filesink name=out"
    );
    run(&pipeline, &dir.join(name))
}

/// A one-second 320×180 MJPEG MP4 at `dir/rotated.mp4` whose global tag list
/// carries `image-orientation=rotate-90`, as a phone writes for portrait
/// footage.
///
/// MP4 rather than WebM because WebM cannot carry `image-orientation`.
pub fn rotated_mp4(dir: &Path) -> PathBuf {
    run(
        "videotestsrc num-buffers=30 \
           ! video/x-raw,width=320,height=180,framerate=30/1 \
           ! jpegenc \
           ! taginject scope=global tags=\"image-orientation=rotate-90\" \
           ! qtmux ! filesink name=out",
        &dir.join("rotated.mp4"),
    )
}

/// A one-second Vorbis-in-Ogg file at `dir/audio_only.ogg` with no video
/// stream.
pub fn audio_only(dir: &Path) -> PathBuf {
    run(
        "audiotestsrc num-buffers=30 samplesperbuffer=1470 \
           ! audio/x-raw,rate=44100,channels=1 \
           ! audioconvert ! vorbisenc ! oggmux ! filesink name=out",
        &dir.join("audio_only.ogg"),
    )
}

/// Runs `description` to EOS, with its `filesink name=out` writing to `path`.
///
/// The location is set as a property rather than spliced into the launch
/// string, so paths with spaces or quotes need no escaping.
fn run(description: &str, path: &Path) -> PathBuf {
    let pipeline = gst::parse::launch(description)
        .unwrap_or_else(|e| panic!("fixture pipeline failed to parse: {e}\n{description}"))
        .downcast::<gst::Pipeline>()
        .expect("a multi-element launch string yields a pipeline");
    pipeline
        .by_name("out")
        .expect("fixture pipeline has a filesink named `out`")
        .set_property("location", path);

    pipeline
        .set_state(gst::State::Playing)
        .unwrap_or_else(|e| panic!("fixture pipeline failed to start: {e}\n{description}"));
    let bus = pipeline.bus().expect("a pipeline has a bus");
    let msg = bus.timed_pop_filtered(TIMEOUT, &[gst::MessageType::Eos, gst::MessageType::Error]);
    let _ = pipeline.set_state(gst::State::Null);

    match msg.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => path.to_path_buf(),
        Some(gst::MessageView::Error(err)) => panic!(
            "fixture pipeline error from {:?}: {} ({:?})\n{description}",
            err.src().map(|s| s.path_string()),
            err.error(),
            err.debug()
        ),
        _ => panic!("fixture pipeline did not reach EOS within {TIMEOUT}\n{description}"),
    }
}
