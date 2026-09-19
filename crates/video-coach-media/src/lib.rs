//! GStreamer-backed media for Coach Cuts: source player, capture, export
//! frame driver, and the vector-overlay rasterizer.
//!
//! Every entry point here assumes `gstreamer::init()` has already run.
//! See `docs/superpowers/specs/2026-09-19-linux-port-design.md`.

pub mod capture;
pub mod fixtures;
pub mod player;
pub mod probe;

pub use capture::{
    list_devices, now_ns, resolve_camera, resolve_mic, Camera, CaptureSources, Devices, Mic,
    Recorder, RecorderMessage, StopOutcome,
};
pub use player::{
    Diagnostics, Frame, FrameMailbox, Origin, PlayerEvent, PositionHandle, SinkKind, SourcePlayer,
};
pub use probe::{probe, Probe, ProbeError};
