//! GStreamer-backed media for Coach Cuts: source player, capture, export
//! frame driver, and the vector-overlay rasterizer.
//!
//! Every entry point here assumes `gstreamer::init()` has already run.
//! See `docs/superpowers/specs/2026-09-19-linux-port-design.md`.

pub mod capture;
pub mod export;
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
pub mod player;
pub mod probe;

pub use capture::{
    list_devices, now_ns, resolve_camera, resolve_mic, Camera, CaptureSources, Devices, Mic,
    Recorder, RecorderMessage, StopOutcome,
};
pub use export::{ExportDone, ExportError, ExportJob, ExportMessage, Exporter};
pub use player::{
    Diagnostics, Frame, FrameMailbox, Origin, PlayerEvent, PositionHandle, SinkKind, SourcePlayer,
};
pub use probe::{probe, Probe, ProbeError};

use gstreamer as gst;
use gstreamer::prelude::*;

/// An `ERROR` message as one line: the posting element, the error and its
/// debug detail.
pub(crate) fn error_text(err: &gst::message::Error) -> String {
    let from = err
        .src()
        .map(|s| format!("{}: ", s.name()))
        .unwrap_or_default();
    match err.debug() {
        Some(debug) => format!("{from}{} ({debug})", err.error()),
        None => format!("{from}{}", err.error()),
    }
}
