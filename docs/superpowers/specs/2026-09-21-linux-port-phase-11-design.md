# Linux Port — Phase 11: Packaging

**Date:** 2026-09-21
**Status:** Reviewed (simplify and correctness passes applied)
**Parent spec:** `docs/superpowers/specs/2026-09-19-linux-port-design.md` (Phase 11 — which is one line; open questions 3 and 4; risks 4, 5, 6)
**Builds on:** Phase 10 (the model path this phase learns to fill), Phase 4 (capture's device requirements), Phase 5 (export's encoder fallbacks)
**Evidence:** measured on the reference laptop — Linux Mint 22.1 (Ubuntu 24.04 base), X11 Cinnamon, glibc 2.39, GStreamer 1.24.2, Intel UHD (i7-10610U). Every plugin-to-package mapping was resolved with `gst-inspect-1.0` → `dpkg -S`; the whisper SIMD, registry and `souphttpsrc` findings were **reproduced**, and where an earlier draft overstated a finding, this one says so.

---

## Goal

The coach installs Coach Cuts on their laptop, finds it in the applications menu, and it runs — with hardware decode, the camera, the microphone, and their own folders, exactly as it does from `cargo run` today.

## Done when

1. **`sudo apt install ./coach-cuts_<version>_amd64.deb` works in a clean `ubuntu:24.04` container** — not only on the reference laptop, which already has every dev package installed and so cannot prove the dependency list.
2. **It is in the menu**, with an icon, and the running window is associated with its launcher.
3. **Hardware decode still works** from the installed copy on the laptop — `bus: loaded …` reports `vah265dec` / `memory:DMABuf` / `egl`.
4. **The model downloads on first use**, with a prompt naming the size and visible progress, verified against its sha256.
5. **CI builds the package on a tag**, proves it installs and starts, and asserts the whisper build has its SIMD.
6. **The README describes this app**, not the macOS one.

---

## Decisions

### S0. `.deb` only, for Ubuntu 24.04 / Mint 22, x86_64

**User decisions (2026-09-21):** a `.deb`, after the evidence below changed the answer from AppImage; and a real CI release pipeline — tags, versioning, a built artifact — rather than a package built by hand on the laptop.

The parent spec's open question 4 recommended AppImage, and BACKLOG #24 recorded the coach's `.app` mental model pointing the same way. What changed the answer:

- **Bundling creates a licence obligation that depending does not.** An AppImage ships GStreamer, ffmpeg and x264 *inside* it — LGPL and GPL works — so distributing it means distributing their corresponding source. A `.deb` that `Depends:` on them distributes none of those. (No *incompatibility* either way: AGPLv3 §13 and GPLv3 §13 grant reciprocal permission, and x264 is GPL-2.0-**or-later**. The tripwire is a future GPL-2.0-**only** dependency. Nothing in the tree is that today.) **This is not "owes nothing" — see S6.**
- **AppImage does not actually deliver the `.app` experience.** One file to double-click, but also `chmod +x`, and no menu entry without hand-placing a `.desktop` or installing AppImageLauncher. `dpkg` does both for free.
- **VA-API is least guaranteed in a bundle.** `libva` and `iHD_drv_video.so` are host- and kernel-coupled and cannot be bundled, so an AppImage can only hope. A `.deb` can `Recommends:` the driver.
- **The bundle carries GStreamer-registry hazards**, which an earlier draft overstated and this one corrects: the `va` plugin registers **zero** elements when `/dev/dri` is absent at scan time, but it declares a dependency on `renderD*`, so the registry rescans and hardware decode **recovers** once the device appears — not "dead forever". Likewise an empty `GST_PLUGIN_SYSTEM_PATH_1_0` does rewrite the shared registry to one plugin, but the next normal run rebuilds it — a costly rescan, not "breaks every other GStreamer app". Real hazards, both avoided entirely by not bundling.
- **The tooling is alpha** (`linuxdeploy` has never cut 1.0) — not verified by this project, recorded as reported.

**Flatpak is out for sharper reasons.** Flathub's linter treats `--filesystem=host` as a hard **error**, and this app stores source paths relative to the project folder that climb out with `..` (`bus/sources.rs:239-246`; the test is at `:286`). There is **no audio-input portal**, so the microphone is a static filesystem hole. And although cameras are *enumerated* through PipeWire's device provider, they are *captured* with `v4l2src`: the camera portal returns a PipeWire node with no `api.v4l2.path`, so `devices.rs` would return an empty camera list, and the `exposure_dynamic_framerate=0` control that stops the measured 30→7.5 fps low-light drop cannot be set through it. **That is a capture rewrite, not a packaging format.**

Both alternatives are backlogged pointing here.

### S1. The whisper build's SIMD must be pinned, explicitly

`whisper-rs-sys` builds whisper.cpp with `-march=native` by default: whisper.cpp's `ggml/CMakeLists.txt` sets `GGML_NATIVE` **on** unless cross-compiling or `SOURCE_DATE_EPOCH` is set. A binary built by CI can `SIGILL` on the laptop — a runner with AVX-512 emits instructions this CPU lacks.

**An earlier draft called the fix "free". It is not, for three reproduced reasons:**

1. **Cargo does not notice the variable changing.** The build script emits `rerun-if-changed=wrapper.h` and no `rerun-if-env-changed=GGML_*`, and cargo's fingerprint tracks only `TARGET` and `BINDGEN_EXTRA_CLANG_ARGS*`. So `GGML_NATIVE=OFF cargo build` on a warm `target/` **silently reuses the native library**. CI's `rust-cache` caches exactly that output.
2. **`SOURCE_DATE_EPOCH` turns off all SIMD.** The CMake logic enables instruction sets only when `NOT (GGML_NATIVE OR NOT GGML_NATIVE_DEFAULT)`, and `SOURCE_DATE_EPOCH` forces the default off. Reproduced: `SOURCE_DATE_EPOCH=0` with `GGML_NATIVE=OFF` gives `ggml-cpu:` with **no flags at all — a scalar whisper**. Debian's packaging tooling exports `SOURCE_DATE_EPOCH`, as do many reproducible-build setups. On a transcriber that already runs at 0.73× realtime, that would be crippling.
3. **`OFF` is not a baseline.** Plain `GGML_NATIVE=OFF` yields `-msse4.2 -mf16c -mfma -mbmi2 -mavx -mavx2` — x86-64-v3, a Haswell-class floor. Fine for this laptop (AVX2/FMA/F16C/BMI2, no AVX-512), but it is a stated requirement, not nothing.

**The fix:** commit `.cargo/config.toml` with `[env]` setting `GGML_NATIVE = "OFF"` **and** `GGML_SSE42`, `GGML_AVX`, `GGML_AVX2`, `GGML_FMA`, `GGML_F16C`, `GGML_BMI2` all `"ON"` explicitly. Reproduced: the explicit flags survive `SOURCE_DATE_EPOCH`. Every build — dev, test, CI, release — then produces the same library, with no CI-only switch to forget. Run `cargo clean -p whisper-rs-sys` once when it lands. **And CI asserts it:** grep the build output for the `-mavx2` line and fail without it, since reason 1 means a cached build could otherwise slip through unnoticed. The package states its x86-64-v3 floor.

### S2. Depend, don't bundle — and let the tools compute what they can

Build with **`cargo-deb`**: a `[package.metadata.deb]` block in the app's `Cargo.toml`, the version taken from the workspace version, no hand-written control file or `dpkg-deb` tree.

`Depends:` is two parts:

- **`$auto`** — cargo-deb's `dpkg-shlibdeps`, which derives the *linked* libraries from the binary: `libc6 (>= 2.39)`, `libglib2.0-0t64`, `libgstreamer1.0-0`, `libgstreamer-gl1.0-0`, `libgstreamer-plugins-base1.0-0`, fontconfig, freetype, `libstdc++6`, `libgcc-s1`. An earlier draft hand-listed some of these and missed the rest.
- **Hand-listed, because nothing links them:**
  - the **GStreamer plugin packages** — `gstreamer1.0-plugins-base`, `-good`, `-bad`, `-ugly`, `gstreamer1.0-libav`, `gstreamer1.0-gl`, `gstreamer1.0-pipewire`. All 37 elements the code names resolve into this set, verified element by element. `avenc_aac` is rank none, so it is never auto-plugged and `libav` is not optional; `-bad` is the price of VA-API (`vah265dec`, `vah264lpenc`, `h264parse`), and carries 86 dependencies of its own with no finer-grained package available.
  - **libraries winit `dlopen`s on X11**, which `dpkg-shlibdeps` cannot see: **`libxcursor1`, `libxi6`, `libxkbcommon-x11-0`**. The coach's session is X11 Cinnamon, and winit's `XConnection::new` opens Xcursor and XInput2 with `?` — **without these the app does not start on a clean system.** Found only by reading winit's source; the laptop has them already, which is exactly why Done-when #1 insists on a clean container.
- **Floor:** `libgstreamer1.0-0 (>= 1.24)`, deliberately. `dpkg-shlibdeps` derives only `>= 1.20` because the `v1_24` features enable bindings the code doesn't call — but 1.24 is what is tested, and the constraint documents that.

`Recommends: intel-media-va-driver | va-driver-all` (in **universe** on noble; Mint enables it) and `zenity` (`rfd` uses the portal and falls back to it).

**Not listed, because the plugin packages already pull them:** `libva2` (a `NEEDED` of `libgstva`), `libegl1`, `libgl1`, `libx11-6` (all via `libgstreamer-gl1.0-0`).

### S3. The model downloader

Phase 10 decided this and parked the implementation here. The decision stands: **download on first use, prompt first, `small.en` default**, cached at `$XDG_CACHE_HOME/coach-cuts/models/` (the `~/.cache` fallback is already implemented and tested).

- **`souphttpsrc ! filesink`, not an HTTP crate.** Verified: rank primary in `plugins-good` 1.24.2, a real fetch followed the 302 to Hugging Face's CDN and reported `size = 487614201`, and throughput matched `curl`. TLS is guaranteed — `plugins-good` hard-depends on `libsoup-3.0-0`, which depends on `glib-networking`. **Zero new Rust dependencies.** Set `iradio-mode=false`.
- **The sha256 costs no new code.** `glib::Checksum::new(ChecksumType::Sha256)` is already linked through `gst::glib`, and can hash in a pad probe as the bytes arrive. An earlier draft proposed ~80 lines of hand-rolled cryptography; that would have been the worst option available. Both hashes are already in `WhisperModel`, measured.
- **The download is the first step of the transcription job**, inside `TranscribeKind::Whisper` on the worker thread — not a separate `Downloading` mechanism. It then inherits cancel, the progress relay, one-at-a-time and `Failed` for free, and `TranscribeKind::Test` is untouched. (An earlier draft said "alongside `Queued`/`Running`"; there is no such enum — the state is `TranscriptionState { queued, running, finished }`.) **Accepted tradeoff:** a recording started mid-download preempts the job and restarts the download. Rare, since the coach just accepted a prompt to start it.
- **The prompt stays on the UI side:** on Transcribe with no model present, confirm with the size, then send the command.
- **`.part` then rename**, as export already does.
- **On a sha mismatch, delete and fail** with a message naming the path. No automatic retry: pressing Transcribe again prompts again, which is the same outcome without a counter.
- **Disk full** is `filesink`'s error, which is the job's ordinary `Failed`. **A second instance** is not supported by the app anyway — two would already fight over `state.json` — so it gets no design here.
- **State the network facts:** `souphttpsrc` defaults to a 15 s timeout and 3 retries, and the CDN's signed URL expires about an hour after issue — which only matters below roughly 135 kB/s. Whether a retry re-requests the expired URL was not verified.
- **Tests never touch Hugging Face.** They serve a file from a local `std::net::TcpListener`.

### S4. Desktop integration

**One application ID, used everywhere.** Today the binary is `video-coach-app` while the config directory is `coach-cuts` — pick one ID and use it for the package name, the binary, the `.desktop` basename, `StartupWMClass=`, and the running window.

**The running window's association is a one-line call.** An earlier draft called this "a real gap" needing a winit patch or a Slint upgrade, "grepped and confirmed". It was wrong: `slint::set_xdg_app_id(…)` is public in Slint 1.18 (`i-slint-core-1.18.0/api.rs:1443`, re-exported by `slint`), and the winit backend applies it through `with_name` for both Wayland `app_id` and X11 `WM_CLASS`. Call it right after `BackendSelector::select()` in `main.rs`, before `AppWindow::new()`. Slint passes an empty instance name (`WM_CLASS = ("", id)`), which is why `StartupWMClass=` is worth setting.

- **The icon already exists:** `apple/App/Assets.xcassets/AppIcon.appiconset/` has it at 512 px and below. No design work.
- **No AppStream `metainfo.xml`.** `apt install ./file.deb` never reads it, and it brings validation demands (releases, content rating, screenshots) for a Flatpak that isn't happening.
- **`Exec=` carries no `%f` or `%U`.** `main.rs` treats argv[1] as a project folder, so a `file://` URI would open or create a bogus project under `$HOME`.
- **`vblank_mode=0` stays out of `Exec=`.** It disables vsync for the whole Mesa process during normal use, to fix a slowdown that only happens with the monitor off (BACKLOG #36). It remains a documented workaround.
- **No maintainer scripts.** dpkg's file triggers already cover `/usr/share/applications` and `/usr/share/icons/hicolor` (checked in `/var/lib/dpkg/triggers/File`). Say so, so the plan doesn't add a `postinst`.
- **Launching from the menu hides diagnostics.** stderr goes to `~/.xsession-errors`, so both `bus: loaded …` and the panic when the renderer can't be selected land there. Document where to look; build nothing.

### S5. The release pipeline

There are no git tags, no CHANGELOG, and `version = "0.1.0"`. The user wants a real pipeline; keep it as small as a real one can be.

- **One source of truth for the version.** CI fails a tag that doesn't equal the workspace `Cargo.toml` version. A draft that let "a tag drives the build" coexist with "the version comes from `Cargo.toml`" allowed them to drift.
- **Build on `runs-on: ubuntu-24.04`** — the exact target distro. The binary already uses `GLIBC_2.39` symbols, so building on anything newer raises the floor above the target. An earlier draft proposed a build container; that was AppImage-era caution the spec itself called less relevant for a `.deb`.
- **`GGML_NATIVE` comes from `.cargo/config.toml`** (S1), and CI **asserts the `-mavx2` line** in the build output.
- **Strip the binary.** There is no `[profile.release]` today, and the release binary is 47 MB and unstripped.
- **Smoke-test in a clean `ubuntu:24.04` container:** `apt install ./x.deb` must resolve — this, not the laptop, is what proves S2's list — and every *software-path* element the code names must exist (`gst-inspect-1.0`). Starting the app there needs Xvfb, Mesa EGL, `fonts-dejavu-core` (the UI asks fontconfig for "monospace"), a fixture project as argv[1], and a timeout, since the app runs until its window closes; it will report `avdec_*`, never `vah265dec`. **The hardware decoder check is Done-when #3, by hand on the laptop.** An earlier draft's smoke test could not have passed in CI at all.
- **The Windows CI job's comment** says Windows is "not a release target until Phase 11". It still isn't — reword it.

### S6. Licence notices ship with the binary

**An earlier draft said depending "owes nothing". That is true of GStreamer only.** The binary statically contains whisper.cpp (MIT), a prebuilt Skia (BSD, downloaded at build time), the DejaVu fonts via `include_bytes!`, and hundreds of MIT/Apache crates, and those licences require their notices to travel with the binary. Ship `/usr/share/doc/<package>/copyright` plus a generated third-party notices file.

### S7. The README is rewritten, not edited

BACKLOG #23 names two false claims on line 5. There are many more — the README describes the macOS app throughout: its platform, its stack, HEVC output, `.mov` recordings, format v6, and a "Pre-built downloads" link to a Releases page that does not exist yet. Rewrite it for the port: what the app is, what it needs, how to install it, that transcription downloads a model once, and that export falls back to software encoding without a VA driver (parent spec risk 4). Link `apple/` as the reference implementation.

---

## Deliberately not in this phase

- **AppImage and Flatpak** — backlogged, pointing at S0.
- **Windows** — its own phase; nothing has been built toward it.
- **An apt repository and auto-update.** Updating means downloading the new `.deb`.
- **Code signing.**
- **The GStreamer 1.28 `whispertranscriber` migration** — needs a newer GStreamer than the target ships.
- **BACKLOG #38, #39, #40 and #46**, which each name packaging or Phase 11 as their revisit point. #39 and #46 are triggered by growing hardware variety, which one `.deb` for one laptop does not cause; #38 and #40 are unaffected by how the app is installed. Re-deferred, each with a line.

## Stale facts corrected alongside this phase (not design)

Found during research, fixed in a separate docs commit rather than carried as spec decisions: the parent spec's "PipeWire capture" (cameras are enumerated through PipeWire but captured with `v4l2src`) and "VA-API/NVENC encode" (there is no NVENC); `scripts/linux-gate-check.sh`, which installs the deprecated `gstreamer1.0-vaapi`, checks `vah264enc` rather than the `vah264lpenc` the code uses, and describes `v4l2src` as a fallback when it is the primary path; the "Ubuntu 24.04" description of the reference laptop, which lives in the parent spec and the seek-latency spike (not `CLAUDE.md`, as an earlier draft claimed); and the "~140 MB" model figure, which is `base.en` — the default `small.en` is 487.6 MB.
