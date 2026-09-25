# Developers

Coach Cuts is a Rust workspace under `crates/`, with a Slint user interface and
GStreamer doing the video work. The design of the Linux app is written up in the
[Linux port design spec](https://github.com/rykerwilliams/coach-cutups/blob/main/docs/superpowers/specs/2026-09-19-linux-port-design.md),
and the conventions every change is held to — the zero-copy decode path, the
capture clock, the export graph, the crate dependency rules — are in
[`CLAUDE.md`](https://github.com/rykerwilliams/coach-cutups/blob/main/CLAUDE.md).

## The crates

- **`crates/video-coach-core`** — pure logic: the project format, the playback
  timeline, zoom, and stroke replay. It declares no media dependency at all, and
  CI runs its tests on a machine with no GStreamer installed.
- **`crates/video-coach-media`** — everything GStreamer: the source player,
  capture, the export frame driver and the overlay rasterizer.
- **`crates/video-coach-app`** — the Slint user interface, the command bus and
  the event layer.
- **`crates/video-coach-harness`** — headless integration tests driven over the
  command bus.

## API documentation

The rustdoc for each crate, built with private items included:

- [`video_coach_core`](api/video_coach_core/index.html)
- [`video_coach_media`](api/video_coach_media/index.html)
- [`video_coach_app`](api/video_coach_app/index.html)

## Building and design history

- [Build from source](https://github.com/rykerwilliams/coach-cutups/blob/main/README.md#build-from-source)
  — the packages to install, and how to run the tests and build the `.deb`.
- [`docs/superpowers/`](https://github.com/rykerwilliams/coach-cutups/tree/main/docs/superpowers)
  — the specs, plans and measurement spikes behind every phase of the port, in
  date order.
