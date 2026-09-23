//! Commentary capture: device enumeration, the camera-mode and encoder
//! choices (spec R2–R4), the recorder (R1, R6) with its live self-view, and
//! the one clock every recording timestamp is read from (R5).

mod devices;
mod recorder;
mod self_view;

use gstreamer as gst;
use gstreamer::prelude::*;

pub use devices::{list_devices, resolve_camera, resolve_mic, Camera, Devices, Mic};
pub use recorder::{CaptureSources, Recorder, RecorderMessage, StopOutcome, LEVEL_INTERVAL_NS};

/// Now on `GstSystemClock` (CLOCK_MONOTONIC), in nanoseconds. The only clock
/// for an event's `host_ns` and a recording's t0 (R5): the recorder runs its
/// pipeline on this clock, so its `base_time` is on the same timeline.
pub fn now_ns() -> u64 {
    gst::SystemClock::obtain().time().nseconds()
}
