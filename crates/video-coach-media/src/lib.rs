//! GStreamer-backed media for Coach Cuts: source player, capture, export
//! frame driver, and the vector-overlay rasterizer.
//!
//! Every entry point here assumes `gstreamer::init()` has already run.
//! See `docs/superpowers/specs/2026-09-19-linux-port-design.md`.

pub mod fixtures;
pub mod player;
pub mod probe;

pub use player::{
    video_sink, Diagnostics, Frame, FrameMailbox, Origin, PlayerEvent, PositionHandle, SinkKind,
    SourcePlayer,
};
pub use probe::{check_orientation, probe, Probe, ProbeError};
