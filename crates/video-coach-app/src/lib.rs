//! Slint UI, command bus, and event layer for Coach Cuts.
//!
//! The [`bus`] is headless: it builds and runs without Slint or a display, so
//! the harness drives it end to end. See
//! `docs/superpowers/specs/2026-09-19-linux-port-phase-2-design.md`.

pub mod bus;
pub mod format;
pub mod zoom_input;
