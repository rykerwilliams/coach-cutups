# Coach Cuts

A desktop app for breaking down game film, for Linux. Open the game video,
record your commentary over it with a webcam and microphone while you play,
pause, scrub, zoom and draw on the picture, and export each clip as a video
with your voice, your drawings and a picture-in-picture of you burned in.

## What it does

- **Commentary clips.** Each clip is a recording of you talking over the game
  video: the webcam and microphone, plus everything you did to the picture
  while recording — play, pause, seek, zoom and pan, and freehand drawings. It
  all replays in step, in the preview and in the export.
- **Clips you can find again.** Name them, tag them, add notes, and filter the
  clip list by tag. Undo and redo with Ctrl+Z and Ctrl+Shift+Z.
- **Scoreboard and match clock.** Set up the two teams, their colours and the
  match format, then tag goals and the start and end of each period (Z, X and
  V). The clock and score are drawn into previews and exports. If your video
  starts after kick-off, the clock can still read correctly.
- **Transcripts.** Transcribe a clip's commentary on your own computer, with
  [whisper.cpp](https://github.com/ggml-org/whisper.cpp).
- **Export.** One H.264 `.mp4` per target — every clip, the clips with one tag,
  or a single clip — at 720p or 1080p, into the project's `exports/` folder.
  Each clip carries a caption bar with its number, name and tags, and the
  webcam inset can be turned off per clip.

A project is a folder: `project.json`, your commentary in `recordings/`, and
your exports in `exports/`. The game video stays where it is.

## Install

Coach Cuts ships as a `.deb` for **Ubuntu 24.04 and Linux Mint 22** on
x86-64. Download it from the
[releases page](https://github.com/rykerwilliams/coach-cutups/releases), then,
in the folder you downloaded it to:

```bash
sudo apt install ./coach-cuts_*_amd64.deb
```

Or [build the package yourself](#build-from-source), which is also how you get
a build newer than the last release.

It appears in the applications menu as **Coach Cuts**. From a terminal it is
`coach-cuts`, or `coach-cuts <folder>` to open (or create) a project there;
with no argument it reopens the last project.

### Requirements

- **GStreamer 1.24 or newer**, as Ubuntu 24.04 ships it. The package pulls in
  the plugin sets it needs.
- **An x86-64-v3 CPU** — Haswell (2013) or newer, with AVX2, FMA, F16C and
  BMI2. The speech recognizer is built for that instruction set.
- **A VA-API driver for hardware video decode and encode.** The package
  recommends `intel-media-va-driver` (or `va-driver-all`), and apt installs
  it by default. Without one, playback falls back to software decode, and
  recording and export to the `x264enc` software encoder, which is much
  slower.
- **An OpenGL (EGL) capable display**, X11 or Wayland.
- **A webcam and microphone** for recording: the camera is captured through
  V4L2 and the microphone through PipeWire.

### Transcription and the network

Transcription runs entirely on your computer. The first time you transcribe,
the app downloads the speech model you chose in the clip inspector —
`small.en` (488 MB, the default, more accurate) or `base.en` (148 MB, faster)
— into `~/.cache/coach-cuts/models/`, and keeps it. The Transcribe button
names the download before you press it. After that, nothing leaves your
machine.

### When something goes wrong

The app writes its diagnostics to stderr. Started from the applications menu
on an X11 session (Linux Mint's default), that lands in `~/.xsession-errors`;
or start it from a terminal as `coach-cuts` to see it directly.

## Build from source

On Ubuntu 24.04 / Linux Mint 22, with Rust 1.92 or newer from
[rustup](https://rustup.rs), install the packages listed (and explained) in
[`packaging/build-deps.txt`](packaging/build-deps.txt) — the same list CI
installs:

```bash
sudo apt install $(grep -o '^[^#]*' packaging/build-deps.txt)

cargo run --release -p video-coach-app        # run it
cargo test --workspace                        # test it
```

The first build compiles whisper.cpp and takes a few minutes.

To build the `.deb` (into `target/debian/`), also install `cargo-deb` and
`cargo-about`, then run the packaging script:

```bash
cargo install --locked cargo-deb
cargo install --locked cargo-about --features cli
packaging/build-deb.sh
```

The code is a Cargo workspace under `crates/`: `video-coach-core` (pure logic,
no media dependencies), `video-coach-media` (GStreamer), `video-coach-app` (the
Slint UI) and `video-coach-harness` (headless integration tests). The
developer conventions — the zero-copy decode path, the capture clock, the
export graph, how to run CI's GPU-less path locally — are in
[`CLAUDE.md`](CLAUDE.md), and the design is in
[`docs/superpowers/specs/`](docs/superpowers/specs/).

## The macOS original

[`apple/`](apple/) holds the original macOS app (Swift, SwiftUI and
AVFoundation), which this port replaces. It is kept as the reference for how
things should behave and is not maintained or built by CI.

## Licence

AGPL-3.0-or-later — see [`LICENSE`](LICENSE). The package installs the
licence notices for the third-party code built into the binary under
`/usr/share/doc/coach-cuts/`.
