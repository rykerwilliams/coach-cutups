# Linux Port — Phase 11 Plan (Packaging)

**Date:** 2026-09-21
**Spec:** `docs/superpowers/specs/2026-09-21-linux-port-phase-11-design.md` (decisions S0–S7)
**Status:** Draft, pre-review.

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task, **staging paths explicitly** — never `git add -A`, which swept a foreign worktree into `7bf06a0`. Every task builds the workspace and passes its tests on its own.

**Known facts. Don't re-derive these — each was verified or reproduced during review.**
- **whisper's SIMD:** `whisper-rs-sys` defaults to `-march=native`. `GGML_NATIVE=OFF` alone yields `-msse4.2 -mf16c -mfma -mbmi2 -mavx -mavx2` (x86-64-v3). **With `SOURCE_DATE_EPOCH` set it yields no SIMD at all** unless each instruction set is turned on explicitly. The build script emits **no** `rerun-if-env-changed`, so a changed `GGML_*` on a warm `target/` is silently ignored.
- **`slint::set_xdg_app_id`** exists in Slint 1.18 (`i-slint-core-1.18.0/api.rs:1443`) and the winit backend applies it for Wayland `app_id` and X11 `WM_CLASS`. It needs the platform selected first.
- **winit `dlopen`s `libxcursor1`, `libxi6`, `libxkbcommon-x11-0` on X11**, invisible to `dpkg-shlibdeps`. The laptop has them; a clean system does not.
- **`souphttpsrc`** is rank primary in `plugins-good` 1.24.2, follows Hugging Face's 302, reports the size, defaults to a 15 s timeout and 3 retries. TLS comes via `libsoup-3.0-0` → `glib-networking`, both hard dependencies of `plugins-good`.
- **`glib::Checksum::new(ChecksumType::Sha256)`** is already linked through `gst::glib`. No hashing crate is needed.
- **The transcription state** is `TranscriptionState { queued, running, finished }` — there is no `Queued`/`Running` enum.
- **`main.rs` treats argv[1] as a project folder.**
- **dpkg file triggers** already refresh `/usr/share/applications` and `/usr/share/icons/hicolor`. No maintainer scripts are needed.
- **The icon exists:** `apple/App/Assets.xcassets/AppIcon.appiconset/`, 512 px and below.
- **The reference laptop is Linux Mint 22.1, X11 Cinnamon**, and already has every dev package — so it **cannot** prove the dependency list. Only a clean container can.

---

## Task 1 — Pin whisper's SIMD, and strip the release binary

The smallest task, and first because every later build inherits it.

1. **`.cargo/config.toml`**, committed, with `[env]`: `GGML_NATIVE = "OFF"` and `GGML_SSE42`, `GGML_AVX`, `GGML_AVX2`, `GGML_FMA`, `GGML_F16C`, `GGML_BMI2` all `"ON"`. Comment why each half exists — the explicit flags are what survive `SOURCE_DATE_EPOCH`.
2. **`cargo clean -p whisper-rs-sys`**, then rebuild and **confirm the build output's `ggml-cpu:` line carries `-mavx2` and not `-march=native`.** Then confirm it again with `SOURCE_DATE_EPOCH=0` set — that is the case the explicit flags exist for.
3. **`[profile.release]` with `strip = true`.** Record the before/after size.
4. **`CLAUDE.md`:** the x86-64-v3 floor, and that a changed `GGML_*` needs `cargo clean -p whisper-rs-sys` because cargo won't notice on its own.
5. **Re-run the whisper `#[ignore]`d throughput test** and confirm the ratio hasn't regressed against the spike's 0.73×. A regression means the SIMD didn't take.

Commit: `build: pin whisper.cpp's instruction set, strip release builds`.

## Task 2 — One application ID, and desktop integration

1. **The ID is `coach-cuts`**, matching the config directory that already exists. Used for the package, the installed binary, the `.desktop` basename, `StartupWMClass=`, and the window.
   - Rename the **binary** via `[[bin]] name = "coach-cuts"` in the app crate. The **package** stays `video-coach-app`, so `cargo run -p video-coach-app` and every documented command keep working — verify that.
2. **`slint::set_xdg_app_id("coach-cuts")`** immediately after `BackendSelector::select()` in `main.rs`, before `AppWindow::new()`.
3. **`packaging/coach-cuts.desktop`**: `Exec=coach-cuts` with **no `%f`/`%U`** (argv[1] is a project folder), `Icon=coach-cuts`, `StartupWMClass=coach-cuts`, `Categories=AudioVideo;Video;`. Validate with `desktop-file-validate` if available.
4. **Icons** from the existing `.appiconset`, copied into `packaging/icons/<size>/coach-cuts.png` for the hicolor sizes it has.
5. **Verify the association** on the laptop: run the renamed binary and read `WM_CLASS` with `xprop` (the session is X11). It must read `coach-cuts`. Screenshot proof isn't needed; the `xprop` line is.

Commit: `feat(app): an application ID and desktop entry`.

## Task 3 — The model downloader

1. **The download is the first step of the transcription job**, inside `TranscribeKind::Whisper` on the worker thread, when the model file is absent. It inherits cancel, the progress relay, one-at-a-time and `Failed`. `TranscribeKind::Test` is untouched.
2. **`souphttpsrc location=<url> iradio-mode=false ! filesink location=<path>.part`**, with the sha256 computed by `glib::Checksum` in a pad probe as bytes arrive. On EOS: compare, then rename `.part` → final. **On mismatch, delete and fail** with a message naming the path. No automatic retry.
3. **Progress** comes off the sink pad's byte count against the reported size, through the existing progress relay. Decide how the UI tells "downloading" from "transcribing" in the one status line — the `running: Option<(Uuid, u8)>` shape may need a phase marker; keep it minimal.
4. **The prompt is UI-side**: on Transcribe with the chosen model absent, a confirmation naming the model and its size; only on accept does `Command::Transcribe` go out. Under `$COACH_CUTS_WHISPER_MODEL` there is never a prompt — the coach supplied the file.
5. **The URL** comes from `WhisperModel` + the existing `MODEL_URL_PREFIX`; the sha256 from `WhisperModel::sha256`, already measured.
6. **Tests serve a file from a local `std::net::TcpListener`** — never Hugging Face. Cover: a good download lands and verifies; a wrong hash deletes the `.part` and fails naming the path; a cancel mid-download leaves no final file; a server error is `Failed`. **Verify each fails against a deliberately broken implementation** before trusting it, as the Phase 10 cancel test taught.
7. **One manual end-to-end** against the real URL for `base.en` (148 MB) into a scratch `XDG_CACHE_HOME`, confirming the hash — then delete it. **Do not** re-download into the real cache.

Commit: `feat(transcribe): download the model on first use`.

## Task 4 — The `.deb`

1. **`cargo-deb`** as the tool; install it with `cargo install cargo-deb` (no `sudo`). Note its version.
2. **`[package.metadata.deb]`** in the app crate:
   - `depends = "$auto, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-plugins-ugly, gstreamer1.0-libav, gstreamer1.0-gl, gstreamer1.0-pipewire, libxcursor1, libxi6, libxkbcommon-x11-0, libgstreamer1.0-0 (>= 1.24)"` — comment the three X11 libraries (dlopened, invisible to shlibdeps) and the floor (tested behaviour, not API need).
   - `recommends = "intel-media-va-driver | va-driver-all, zenity"`.
   - assets: the binary to `/usr/bin/coach-cuts`, the `.desktop` to `/usr/share/applications/`, the icons to `/usr/share/icons/hicolor/<size>/apps/`, and the licence files (item 3).
3. **Licence notices (S6):** `/usr/share/doc/coach-cuts/copyright` (the AGPL notice plus the statically-linked components: whisper.cpp MIT, Skia BSD, DejaVu fonts) and a **generated** third-party notices file for the crate graph. Pick a generator (e.g. `cargo-about`) that runs without network after a fetch; don't hand-maintain hundreds of entries.
4. **Build it and inspect it:** `dpkg-deb -I` for the control fields, `dpkg-deb -c` for the file list. Confirm `$auto` resolved to real library packages including `libc6 (>= 2.39)`.
5. **Prove the dependency list in a clean container** — `docker run ubuntu:24.04`, `apt install ./coach-cuts_*.deb`, it must resolve. Then `gst-inspect-1.0` every software-path element the code names. **This is Done-when #1, and the laptop cannot substitute for it.** Record whether docker needed `sudo`; if it does, **stop and ask the user** rather than working around it.

Commit: `build: package as a .deb`.

## Task 5 — The release pipeline

1. **A `release` job** in `.github/workflows/rust.yml` (or its own workflow file — decide and say why), on tags matching `v*`, `runs-on: ubuntu-24.04`.
2. **Fail if the tag ≠ `v` + the workspace version.** One source of truth.
3. **Build, then assert the `-mavx2` line** in the whisper build output — a cached `rust-cache` could otherwise ship a native build. Make the assertion robust to where cargo writes build-script output.
4. **`cargo deb`**, then the **clean-container smoke test** from Task 4 as a CI step: install resolves, software elements exist. Optionally start the app under Xvfb with Mesa EGL, `fonts-dejavu-core` and a fixture project, with a timeout — it will report `avdec_*`, never `vah265dec`. Only include the launch step if it can be made reliable; a flaky release gate is worse than none.
5. **Upload the `.deb` to a GitHub Release** for the tag.
6. **Reword the `windows` job's comment** — it says Windows is not a release target "until Phase 11"; it still isn't.
7. **Validate the workflow without cutting a release:** `actionlint` if available, and a dry run of every step locally. **Do not push a tag.** Creating the first release is the user's call, at the closeout.

Commit: `ci: build and publish the .deb on a tag`.

## Task 6 — The README

Rewritten for the port (S7): what the app is; the `.deb` install; what it needs (a VA driver for hardware decode, and that export falls back to software encoding without one — parent spec risk 4); that transcription downloads a model once and which; where stderr goes when launched from the menu (`~/.xsession-errors`); how to build from source (cmake, libclang, the GStreamer dev packages, the x86-64-v3 floor); and `apple/` as the reference implementation. **Remove the link to a Releases page until one exists.**

Commit: `docs: rewrite the README for the Linux port`.

## Task 7 — Closeout

1. Adversarial review of the Phase 11 diff; apply and backlog.
2. **Re-defer BACKLOG #38, #39, #40, #46** with a line each, and add entries for **AppImage** and **Flatpak** pointing at spec S0.
3. `CLAUDE.md`: the release process, the app ID, the SIMD rule.
4. **Hands-on checklist**, and the question of cutting the first tag.

## Deliberately not in this phase

AppImage, Flatpak, Windows, an apt repository, auto-update, code signing, the GStreamer 1.28 `whispertranscriber` migration.
