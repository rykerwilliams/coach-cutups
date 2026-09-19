//! Commentary capture: device enumeration, the camera-mode and encoder
//! choices (spec R2–R4), the recorder (R1, R6), and the one clock every
//! recording timestamp is read from (R5).

mod devices;
mod recorder;

use gstreamer as gst;
use gstreamer::prelude::*;

pub use devices::{
    choose_camera_mode, choose_encoder, list_devices, resolve_camera, resolve_mic, Camera,
    CameraMode, Devices, EncoderChain, Input, Mic,
};
pub use recorder::{CaptureSources, Recorder, RecorderMessage, StopOutcome};

/// Now on `GstSystemClock` (CLOCK_MONOTONIC), in nanoseconds. The only clock
/// for an event's `host_ns` and a recording's t0 (R5): the recorder runs its
/// pipeline on this clock, so its `base_time` is on the same timeline.
pub fn now_ns() -> u64 {
    gst::SystemClock::obtain().time().nseconds()
}
