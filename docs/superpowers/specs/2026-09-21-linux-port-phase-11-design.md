# Linux Port — Phase 11: Packaging

**Date:** 2026-09-21
**Status:** Draft, pre-review.
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phase 11 — which is one line; open questions 3 and 4; risks 4, 5, 6)
**Builds on:** Phase 10 (the model path this phase learns to fill), Phase 4 (capture's device requirements), Phase 5 (export's encoder fallbacks)
**Evidence:** measured on the reference laptop — Linux Mint 22.1 (Ubuntu 24.04 base), glibc 2.39, GStreamer 1.24.2, Intel UHD (i7-10610U). Every plugin-to-package mapping below was resolved with `gst-inspect-1.0` → `dpkg -S`; the VA-API and `-march=native` findings were reproduced, not reasoned.

---

## Goal

The coach installs Coach Cuts on their laptop, finds it in the applications menu, and it runs — with hardware decode, the camera, the microphone, and their own folders, exactly as it does from `cargo run` today.

## Done when

1. **`sudo apt install ./coach-cuts_<version>_amd64.deb` works** on the reference laptop, and pulls what it needs.
2. **It is in the menu**, with an icon, and launching it from there behaves like launching it from a terminal.
3. **Hardware decode still works** from the installed copy — `bus: loaded …` reports `vah265dec` / `memory:DMABuf` / `egl`, not a software decoder.
4. **The model downloads on first use**, with a prompt naming the size and visible progress, verified against its sha256.
5. **CI builds the package** on a tag, and the binary runs on a machine that did not build it.
6. **The README describes this app**, not the macOS one.

---

## Decisions

### S0. `.deb` only, for Ubuntu 24.04 / Mint 22, x86_64

**User decision (2026-09-21), after the evidence below changed the answer.** The parent spec's open question 4 recommended AppImage, and BACKLOG #24 recorded the coach's `.app` mental model pointing the same way. The research says otherwise, and the deciding facts are not preferences:

- **Bundling creates a licence obligation that depending does not.** An AppImage ships GStreamer, ffmpeg and x264 *inside* it. Those are LGPL and GPL works, so distributing it means distributing their corresponding source. A `.deb` with `Depends:` distributes none of them and owes nothing. (No *incompatibility* either way — AGPLv3 §13 and GPLv3 §13 grant reciprocal permission, and x264 is GPL-2.0-**or-later** so it upgrades to v3. The tripwire to watch is a future GPL-2.0-**only** dependency, which would not be compatible. Nothing in the tree is that today.)
- **AppImage does not actually deliver the `.app` experience.** One file to double-click, yes — but also `chmod +x`, and no menu entry without hand-placing a `.desktop` or installing AppImageLauncher. `dpkg` does both for free.
- **VA-API is *least* guaranteed in the bundle.** `libva` and `iHD_drv_video.so` are host- and kernel-coupled and cannot be bundled, so an AppImage can only hope the host has them. A `.deb` can `Recommends:` the driver; Flatpak's runtime installs it automatically.
- **Three bundle-specific traps**, all measured: the GStreamer registry is built per-machine from detected hardware (ship a stale one and VA-API is silently dead forever); setting `GST_PLUGIN_SYSTEM_PATH_1_0` at an empty path makes GStreamer **rewrite the user's shared `~/.cache/gstreamer-1.0/registry.x86_64.bin` as empty, breaking every other GStreamer app on the machine** (Tauri ships this bug today); and Skia links host `libfontconfig`/`libfreetype`, the classic AppImage breakage the parent spec already named at line 416.
- **The tooling is alpha.** `linuxdeploy` has never cut 1.0; its GStreamer plugin has an open path-canonicalization bug; `cargo-appimage` auto-links everything, which is the over-bundling failure mode to avoid.

**Flatpak is out for different reasons, recorded so nobody re-derives them.** Flathub's linter treats `--filesystem=host` as a hard **error**, and this app stores source paths *relative to the project folder, possibly climbing out with `..`* (`bus/sources.rs:239-246`, with a test asserting `"../../media/cam/a.mp4"`), re-resolving them by path on every launch. There is also **no audio-input portal at all** — the microphone is a static filesystem hole (`--filesystem=xdg-run/pipewire-0`), not a portal. And the camera portal hands back a PipeWire node, not a device path, so `devices.rs:224` (which drops any camera lacking `api.v4l2.path`) would return an **empty camera list**, and the `exposure_dynamic_framerate=0` control that stops the measured 30→7.5 fps drop in low light **cannot be set through it**. Flatpak is a rewrite of the capture layer, not a packaging format.

**Scope, stated plainly:** one architecture, one distro family, the machine the coach owns. Both alternatives are backlogged with this evidence attached.

### S1. `-march=native` must go — this is a correctness bug, not a packaging preference

Measured: `target/release/build/whisper-rs-sys-*/output` contains `-- Adding CPU backend variant ggml-cpu: -march=native`. whisper.cpp's `ggml/CMakeLists.txt` defaults `GGML_NATIVE` **ON** unless cross-compiling or `SOURCE_DATE_EPOCH` is set, and `whisper-rs-sys`'s `build.rs` never overrides it.

**A binary built on one machine can `SIGILL` on another.** That applies to any distributed artifact, `.deb` included, the moment CI builds it instead of the coach's own laptop.

The fix is free: `build.rs` passes any `GGML_*` environment variable through as a cmake define, so **`GGML_NATIVE=OFF` in the release build** is enough, with no patch and no fork. Set it in CI and document it as a property of every release build.

### S2. Depend, don't bundle

```
Depends: libgstreamer1.0-0 (>= 1.24), gstreamer1.0-plugins-base,
         gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad,
         gstreamer1.0-plugins-ugly, gstreamer1.0-libav, gstreamer1.0-gl,
         gstreamer1.0-pipewire, libva2, libegl1, libgl1, libfontconfig1,
         libfreetype6, libxkbcommon0, libx11-6, libstdc++6
Recommends: intel-media-va-driver | va-driver-all
```

Every element the code names resolves into that set, verified element by element. Notes that matter:

- **`gstreamer1.0-plugins-bad` is the price of VA-API**: `libgstva.so` (the `vah265dec`/`vah264lpenc` the zero-copy path needs) and `libgstvideoparsersbad.so` (`h264parse`) live there, and it carries **86 `Depends:`** on noble. There is no finer-grained package. It is still far less work than bundling.
- **`avenc_aac` is rank none**, so it is never auto-plugged and the export path names it explicitly — `gstreamer1.0-libav` is not optional.
- **`x264enc` is the software encoder fallback** (`gstreamer1.0-plugins-ugly`), reached when no VA encoder exists.
- **`gstreamer1.0-pipewire` is the microphone** *and* the device enumerator. CI does not install it today, which is why capture tests use `CaptureKind::Test`.
- **The VA driver is `Recommends:`, not `Depends:`** — the app runs without it, just slowly, and the existing export error already says what to install. `apt` installs recommends by default, so the coach gets it.

**Nothing is bundled, and that is the point:** no registry to stale, no `LD_LIBRARY_PATH` to get wrong, no fontconfig ABI to match, no corresponding-source obligation.

### S3. The model downloader, at last

Phase 10 decided this and deferred the implementation here (its S3). The decision stands: **download on first use, prompt first, `small.en` default**, per-model sha256, cached at `$XDG_CACHE_HOME/coach-cuts/models/`.

**Use `souphttpsrc ! filesink`, not an HTTP crate.** The workspace has **no network dependency of any kind** today — no `reqwest`, `ureq`, `rustls`, `ring`, `sha2` — and adding a TLS tree for one download would be larger than everything Phases 5–9 added combined. `souphttpsrc` is already installed, rank primary, already in CI's plugin set, follows the Hugging Face redirect, and gives byte progress off the sink pad. **Zero new Rust dependencies**, in a crate that is already GStreamer-native.

- **Both sha256s are already in the code**, measured, beside `WhisperModel` — `base.en` `a03779c8…c6d002` (147,964,211 bytes) and `small.en` `c6138d6d…c41e5d` (487,614,201 bytes). The verification needs a SHA-256 implementation; that is ~80 lines of pure Rust or one small no-dep crate, and is the *only* new code the download genuinely requires.
- **`.part` then rename**, as the export path already does. A truncated model otherwise surfaces as a confusing whisper error much later — and Phase 10's load failure already prints the file's size for exactly this reason.
- **It reuses the queue's shapes:** a `Downloading` state alongside `Queued`/`Running`, the whole-state event, and the existing progress relay. Do not invent a second long-running-job mechanism.
- **A second launch mid-download** writes the same `.part`. Use a distinct temp name, or state that the collision is accepted.
- **Disk full, `$XDG_CACHE_HOME` unset, sha mismatch** each need an answer. A corrupted cache that fails forever is the worst outcome: delete and retry once, then fail with the path.

### S4. Desktop integration, and the application-ID gap

Ship a `.desktop` file, an icon, and an AppStream `metainfo.xml` (cheap, and it is what any future Flatpak would need anyway).

**There is a real gap here that is not packaging's fault.** The app has **no application ID**: the binary is `video-coach-app`, the window title is `"Coach Cuts"`, and Slint 1.18's winit backend exposes no way to set the Wayland `app_id` / X11 `WM_CLASS` — grepped and confirmed. A `.desktop` file whose name does not match the surface's `app_id` means **the running window is not associated with its launcher**: wrong icon in the dock, no grouping, no "pin to taskbar". This must be solved, and the options (a winit patch, a Slint upgrade, an env-var or startup-id workaround) need investigating in the plan rather than assumed.

Also name the launch quirks the app already has: `vblank_mode=0` when the monitor is off (BACKLOG #36) — decide whether the `.desktop` `Exec=` carries it or whether it stays a documented workaround.

### S5. Versioning and a release job

There are **no git tags, no CHANGELOG, and `version = "0.1.0"`** inherited by all four crates. Phase 11 has to invent the scheme; invent the smallest one that works.

- A tag drives the build; the `.deb` version comes from `Cargo.toml`'s workspace version.
- **Build in a container, not on a runner image.** `ubuntu-22.04` entered deprecation on 2026-09-17 with full removal in April 2027, so pinning a runner label ties the glibc floor to GitHub's image lifecycle. A container decouples them. For a `.deb` targeting 24.04 this matters less than it would for an AppImage, but it is free to do right.
- **`GGML_NATIVE=OFF`** (S1), non-negotiable.
- The build needs network, because `skia-bindings` downloads prebuilt binaries — confirmed, and it is why the workspace compiles C++ without needing clang locally.
- **Smoke-test the artifact**: install it in a clean container and check it starts and reports its decoder. A package that builds but does not run is the failure this phase exists to prevent.

### S6. The README is rewritten, not edited

BACKLOG #23 covers two false claims on line 5. The research found **at least eight**: "Native macOS app", "Built on Swift + SwiftUI + AVFoundation", "no network calls", "No FFmpeg", HEVC output (the port is H.264 High), the custom AVFoundation compositor, `recordings/` of `.mov` files (the port writes `.mkv`), format v6 (the port starts at v7), macOS 26 / Apple Silicon requirements, and a "Pre-built downloads" link to a Releases page **that does not exist**.

This is a rewrite. It should say what the app is, what it needs, how to install it, that transcription downloads a model once, and that export falls back to software encoding without a VA driver (parent spec risk 4 asks for exactly that).

### S7. Stale facts in our own documents, corrected here

Found while researching; each would mislead a future reader:

- The parent spec's locked-decisions table says **"PipeWire capture"**. Only the *microphone* is PipeWire. The **camera is `v4l2src`**, and `devices.rs:224` discards any camera without a v4l2 path. This matters: "we already use PipeWire so the Flatpak camera portal is close" is false, and it is the reason S0 rejects Flatpak.
- The same table says **"VA-API/NVENC encode"**. There is **no NVENC** — `grep` finds nothing, and the export path is `vah264lpenc` then `x264enc`.
- **`scripts/linux-gate-check.sh` tells you to install `gstreamer1.0-vaapi`** — the old, separate, deprecated plugin. The code uses the newer `va` plugin from `gstreamer1.0-plugins-bad`.
- **`CLAUDE.md` calls the reference laptop "Ubuntu 24.04"**; it is Linux Mint 22.1 on an Ubuntu 24.04 base. Cosmetic until this phase starts making distro claims.
- **BACKLOG #22 and the parent spec price bundling the model at "~140 MB"** — that is `base.en`. The default is `small.en` at **487.6 MB**, 3.3× the number both documents reason with. Moot now that downloading won, but the numbers should not stay wrong.

---

## Deliberately not in this phase

- **AppImage and Flatpak** — backlogged with S0's evidence. Revisit if the app is ever handed to another coach or a different distro.
- **Windows.** The parent spec puts it here, but nothing has been built toward it and CI only `cargo check`s core. It is its own phase.
- **An apt repository.** Updating means downloading a new `.deb`. Hosting a repo is real work for one user.
- **Auto-update**, and **code signing**.
- **The GStreamer 1.28 `whispertranscriber` migration** (Phase 10's note) — it needs a GStreamer newer than the target distro ships.
