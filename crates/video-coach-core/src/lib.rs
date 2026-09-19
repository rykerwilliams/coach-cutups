//! Pure logic for Coach Cuts.
//!
//! This crate declares **no media dependency** — not GStreamer, not an image or
//! font crate, not a feature that pulls one in. CI runs its tests on a runner
//! with no GStreamer installed, so adding one fails the build rather than
//! passing silently. If you need a media type here, you need a different
//! design.

pub mod event;
pub mod plan;
pub mod project;
pub mod recording;
pub mod scoreboard_config;
pub mod skip;
pub mod store;
pub mod stroke;
pub mod stroke_replay;
pub mod tag;
pub mod timeline;
pub mod zoom;
