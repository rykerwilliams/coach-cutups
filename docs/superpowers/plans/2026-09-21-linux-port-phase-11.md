# Linux Port — Phase 11 Plan (Packaging)

**Date:** 2026-09-21
**Spec:** `docs/superpowers/specs/2026-09-21-linux-port-phase-11-design.md` (decisions S0–S7)
**Status:** Reviewed (simplify and correctness passes applied).

**Execution.** A fresh subagent per task, given this plan, the spec and `CLAUDE.md`. The orchestrator runs `verify` and commits each task, **staging paths explicitly** — never `git add -A`, which swept a foreign worktree into `7bf06a0`. Every task builds the workspace and passes its tests on its own. Each task writes its own `CLAUDE.md` addition, rather than a closeout sweep.

**The phase is already de-risked.** The correctness review built a real `.deb` with cargo-deb 3.8.0, installed it into a clean `ubuntu:24.04` container, and launched it under Xvfb: it reported `GLPlatform(EGL)`, stayed up, and created `project.json`. What follows closes the five gaps that experiment found.

**Known facts. Don't re-derive these — each was reproduced during review.**
- **whisper's SIMD:** `GGML_NATIVE=OFF` alone gives x86-64-v3; with `SOURCE_DATE_EPOCH` set it gives **no SIMD at all**; the explicit `GGML_SSE42/AVX/AVX2/FMA/F16C/BMI2=ON` flags survive it. The names are right for whisper.cpp 1.8.3 (`ggml/CMakeLists.txt:151-163`).
- **`.cargo/config.toml`'s `[env]` reaches build scripts**, and cargo-deb honours it — **but changing `[env]` does not rerun the build script.** `cargo clean -p whisper-rs-sys` is genuinely required — **and cleans only the dev profile** (cargo 1.98.1). The release copy survives until `cargo clean -p whisper-rs-sys --release`. *(An earlier draft said it removed every profile's copy; Task 1 found otherwise. It matters most for the `.deb`, which is built from `target/release`.)*
- **The build output line** is in `target/release/build/whisper-rs-sys-<hash>/output`: `-- Adding CPU backend variant ggml-cpu: -msse4.2;-mf16c;-mfma;-mbmi2;-mavx;-mavx2 …` — `;`-joined. **The repo's `target/` holds six such files today, all `-march=native`.**
- **`Swatinem/rust-cache` would preserve a stale native build**: it hashes `.cargo/config.toml` into its key, but on a miss it restores the prefix key and keeps dependency build dirs younger than a week — and cargo then doesn't rerun the script.
- **`[[bin]] name = "coach-cuts"` needs `path = "src/main.rs"`**, or the manifest fails to parse and the whole workspace breaks. With it, `cargo run -p video-coach-app` runs `target/debug/coach-cuts` and nothing else names the binary.
- **`slint::set_xdg_app_id` before `select()` returns `Err(NoPlatform)`** — a `let _ =` would silently no-op. Call it after, and `.expect()` it. `xprop` then shows `WM_CLASS = "", "coach-cuts"` (empty instance), which matches `StartupWMClass`.
- **cargo-deb defaults** the package name to the crate name (`video-coach-app`), emits **no `Maintainer:`** (dpkg then warns on every later apt command, permanently), and a placeholder description. It **strips by default** (47 MB → 37 MB; the `.deb` is 11 MB). `$auto` runs `dpkg-shlibdeps`, which needs `dpkg-dev` — without it cargo-deb only *warns* and ships no libc floor.
- **`$auto` actually yields:** `libc6 (>= 2.39)`, `libfontconfig1`, `libfreetype6`, `libglib2.0-0t64`, `libgstreamer-gl1.0-0 (>= 1.23.1)`, `libgstreamer-plugins-base1.0-0`, `libgstreamer1.0-0`, `libstdc++6`.
- **`gst-inspect-1.0` is in `gstreamer1.0-tools`**, which nothing in the dependency chain pulls. With it, every software-path element exists in the clean container; `vah*` is absent without `/dev/dri`, as expected.
- **Docker works without `sudo`** — the user is in the `docker` group.
- **Two existing, non-ignored tests point the whisper transcriber at a *missing* file carrying our own file name** and expect a fast `Failed`: `a_missing_model_names_the_path_and_the_url` (`video-coach-media/src/transcribe.rs`) and `a_new_job_runs_the_model_just_picked` (harness). Any "download when absent" trigger derived from the path would pull ~600 MB from Hugging Face on CI.
- **`Transcriber` is never joined** — a cancelled job's thread keeps running. The next queued job starts in the same bus turn.
- **Hugging Face's `resolve/main/` URL is mutable.** Today's commit is `5359861c739e955e79d9a303bcbc70fb988958b1`; the base.en redirect's `x-linked-etag` equals the code's sha256.
- **The icon set** spans 16–1024 px; hicolor has no 1024 directory.

---

## Task 1 — Pin whisper's instruction set

1. **`.cargo/config.toml`**, committed, `[env]`: `GGML_NATIVE = "OFF"` and `GGML_SSE42`, `GGML_AVX`, `GGML_AVX2`, `GGML_FMA`, `GGML_F16C`, `GGML_BMI2` all `"ON"`. Comment both halves: the explicit flags are what survive `SOURCE_DATE_EPOCH`.
2. **`cargo clean -p whisper-rs-sys` and again with `--release`**, rebuild, and confirm `-mavx2` and no `-march=native` in the output — then again with `SOURCE_DATE_EPOCH=0`.
3. **Re-run the whisper `#[ignore]`d throughput test** and confirm no regression against the spike's 0.73×. A regression means the SIMD didn't take.
4. **`CLAUDE.md`:** the x86-64-v3 floor, and that a changed `GGML_*` needs `cargo clean -p whisper-rs-sys` because cargo won't notice.

**No `strip = true`.** cargo-deb strips what it packages; stripping the release profile would also strip local `--release` builds, losing panic backtraces and the symbols `perf` needs for the `measure-media` skill.

Commit: `build: pin whisper.cpp's instruction set`.

## Task 2 — One application ID, and desktop integration

1. **The ID is `coach-cuts`**, matching the config directory. `[[bin]] name = "coach-cuts", path = "src/main.rs"` in the app crate; the package stays `video-coach-app`, so every documented `cargo … -p video-coach-app` command still works. Update `main.rs:4`'s doc comment.
2. **`slint::set_xdg_app_id("coach-cuts").expect(…)`** immediately after `BackendSelector::select()`, before `AppWindow::new()`.
3. **`packaging/coach-cuts.desktop`**: `Exec=coach-cuts` with **no `%f`/`%U`**, `Icon=coach-cuts`, `StartupWMClass=coach-cuts`, `Categories=AudioVideo;Video;`. `desktop-file-validate` it if available.
4. **Icons**: the six distinct sizes 16/32/64/128/256/512 from the `.appiconset`, into `packaging/icons/<size>x<size>/coach-cuts.png` (hicolor's own naming, so Task 4 maps each directory straight across). Skip 1024; scaling covers 48.
5. **Verify on the laptop (X11):** `xprop WM_CLASS` on the running window reads `"", "coach-cuts"`.
6. **`CLAUDE.md`:** the app ID and where it is set.

Commit: `feat(app): an application ID and desktop entry`.

## Task 3 — The model downloader

**The job must be told whether it may download, and from where.** Deriving it from the path would download into `$COACH_CUTS_WHISPER_MODEL`'s directory, and would make CI pull ~600 MB through the two existing missing-model tests.

1. **`TranscribeKind::Whisper { model: PathBuf, fetch: Option<Fetch> }`**, `Fetch { url, sha256, bytes }`. **The bus sets it `Some` only when the path came from the cache directory**, never under the override; `set_transcribe_model` updates it under the same "only when it's ours" check it already applies to the path. **Existing tests pass `None`** and keep today's behaviour unchanged.
2. **A standalone `download(url, dest, sha256, bytes, progress, cancel)` in `video-coach-media`**, called as the job's first step when the model is absent and `fetch` is `Some`. Testable on its own, which keeps any URL-override seam out of `TranscribeKind`.
   - `create_dir_all` the models directory first — `filesink` doesn't create it.
   - `souphttpsrc location=<url> iradio-mode=false ! filesink location=<dest>.part`.
   - **Progress by polling** `query_position::<Bytes>` against `WhisperModel::bytes()` from the job thread, the way whisper's percent is polled today. No pad probe.
   - **On EOS, hash the *file*** in chunks through `glib::Checksum` (well under a second), so what is verified is exactly what gets renamed. Match → rename; mismatch → **delete the `.part`** and fail naming the path.
   - **On cancel, leave the `.part`.** The thread is never joined, so deleting it could land after the *next* job's `filesink` opened the same path, sending 487 MB into an unlinked inode. The next attempt truncates it anyway, and a quit mid-download leaves one regardless. *(Superseded in the closeout: the `ONE_AT_A_TIME` lock added from the code review means the cancelled job still holds the lock while it cleans up, so the race this guarded against can't happen, and a cancel now deletes its `.part` like every other failure.)*
3. **Pin the URL to the Hugging Face commit** (`resolve/5359861c…/`), not `main`: an upstream re-upload would otherwise fail every download's hash with no recovery short of a release. Update `MODEL_URL_PREFIX`, and the stale "found, never fetched" comments in both crates and `main.rs`.
4. **`WhisperModel::bytes()`** as a `const fn` — the sizes are only in doc comments today. It feeds the label, the progress denominator, and a length check.
5. **A distinct downloading state**, because without one the screen reads "Transcribing… 0:40 · 63%" while downloading and the whisper clock then includes the download time:
   - `TranscribeMessage::Downloading(u8)`;
   - `TranscriptionState.downloading: Option<u8>`, set from `Downloading`, cleared on the first `Progress` or `Finished`;
   - the inspector shows "Downloading small.en… 63%", and resets `since` on `Some` → `None`.
6. **The button is the prompt.** When the chosen model is absent and there is no override, Transcribe reads **"Download small.en (488 MB) and transcribe"** — pressing it is the consent. No modal: the only dialog in `app.slint` today is the error dialog, and a confirm would mean a new two-button modal inside the Esc cascade. The UI needs to know the chosen model and whether its file exists.
7. **A failed download drops the queue behind it.** Otherwise every queued clip re-attempts in turn — offline at the field, or 487 MB per clip after a hash mismatch.
8. **Tests serve from a local `std::net::TcpListener`**, calling `download()` directly: a good file lands and verifies; a wrong hash deletes the `.part` and fails naming the path; a cancel leaves **no final file**; a server error fails. **Verify each fails against a deliberately broken implementation first.** Plus a bus-level test that `fetch: None` never downloads.
9. **One manual end-to-end** against the pinned real URL for `base.en` into a scratch `XDG_CACHE_HOME`, confirming the hash — then delete it.

Commit: `feat(transcribe): download the model on first use`.

## Task 4 — The `.deb`

1. **`cargo install cargo-deb`** (no `sudo`); record the version. Install **`dpkg-dev`** is required for `$auto` — it is already on the laptop; CI must install it.
2. **`[package.metadata.deb]`**: `name = "coach-cuts"`, a `maintainer`, an `extended-description`, and
   - `depends = "$auto, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-plugins-ugly, gstreamer1.0-libav, gstreamer1.0-gl, gstreamer1.0-pipewire, libxcursor1, libxi6, libxkbcommon-x11-0, libgstreamer1.0-0 (>= 1.24)"`. Comment the three X11 libraries (winit dlopens them) and the floor (tested behaviour, not API need — it sits beside `$auto`'s `>= 1.20` as a legal duplicate constraint, and apt resolves it correctly).
   - `recommends = "intel-media-va-driver | va-driver-all, zenity"`.
   - assets: `target/release/coach-cuts` → `/usr/bin/`, the `.desktop` → `/usr/share/applications/`, `packaging/icons/<size>x<size>/` → `/usr/share/icons/hicolor/<size>x<size>/apps/`.
3. **Licence notices (S6):**
   - `license-file` → a hand-written `packaging/copyright`: the AGPL notice plus the statically-linked C/C++ that crate-licence tools cannot see — whisper.cpp (MIT, inside `whisper-rs-sys`), Skia (BSD, inside `skia-bindings`), the DejaVu fonts.
   - **`cargo-about` generates the crate notices at package time**, with its `accepted` licence list doubling as the **GPL-2.0-only tripwire** spec S0 names — a crate that would make the combination incompatible then fails the build rather than being found later. Not committed, so it can't drift from `Cargo.lock`.
4. **`packaging/smoke-test.sh`**, the one definition used locally and in CI, run inside `ubuntu:24.04`:
   - `apt install ./coach-cuts_*.deb` must resolve — **this, not the laptop, proves the dependency list**;
   - install `gstreamer1.0-tools` (smoke step only, never `Depends:`) and `gst-inspect-1.0` every software-path element;
   - assert `dpkg-deb -f … Depends` contains `libc6 (>= 2.39)`, so a missing `dpkg-shlibdeps` can't ship silently;
   - **launch it — mandatory, not optional**, since it is the only proof of the three dlopened X11 libraries: install `xvfb xauth libgl1-mesa-dri libegl-mesa0`, then `XDG_CONFIG_HOME=<tmp> timeout 20 xvfb-run -a coach-cuts <empty dir>`. Pass on exit 124 (still running), no panic on stderr, and `project.json` created. An empty directory is enough; `main.rs` opens or creates a project there.
5. **Inspect it:** `dpkg-deb -I` and `-c`; confirm the name, the maintainer, stripping, and the file list.

Commit: `build: package as a .deb`.

## Task 5 — The release pipeline

1. **A separate `.github/workflows/release.yml`**, on `push: tags: ['v*']` **and** `workflow_dispatch`. Putting it in `rust.yml` would need a tags trigger that re-runs every other job on each tag, plus per-job guards.
2. **`permissions: contents: write`**, scoped to the job that uploads.
3. **Gate on tests:** run the workspace's `fmt`/`clippy`/`test` first, or depend on them.
4. **Fail if the tag ≠ `v` + the workspace version** (skipped under `workflow_dispatch`).
5. **No `rust-cache`.** Releases are rare, a cold build is ~14 min, and a clean build removes the cached-native-library hazard at its root.
6. Repeat the workspace job's apt list, plus `dpkg-dev`.
7. **`cargo deb`, then assert the SIMD** — after packaging, so it checks the build that was packaged: `find target -path '*whisper-rs-sys-*' -name output` must match at least once, **every** match must contain `-mavx2`, and **none** may contain `-march=native`.
8. **Run `packaging/smoke-test.sh`** in `ubuntu:24.04`.
9. **Upload to a GitHub Release — only on a tag.** Under `workflow_dispatch` the whole pipeline runs for real and uploads nothing, which is how it gets validated without cutting a release.
10. **Reword the `windows` job's comment** in `rust.yml`.
11. **Validate:** `actionlint` if available, then **push the workflow and trigger it once via `workflow_dispatch`** if Actions are enabled on the fork. **Do not push a tag.**
12. **`CLAUDE.md`:** the release process.

Commit: `ci: build and publish the .deb on a tag`.

## Task 6 — The README

Rewritten for the port (spec S7): what the app is; installing the `.deb`; that hardware decode wants a VA driver and export falls back to software encoding without one; that transcription downloads a model once, and its size; that menu launches log to `~/.xsession-errors`; building from source (cmake, libclang, the GStreamer dev packages, the x86-64-v3 floor); `apple/` as the reference implementation. **No Releases link until one exists.** Mark **BACKLOG #23** resolved.

Commit: `docs: rewrite the README for the Linux port`.

## Task 7 — Closeout

1. Adversarial review of the Phase 11 diff; apply and backlog.
2. **Re-defer BACKLOG #38, #39, #40 and #46**, a line each. AppImage and Flatpak are already recorded under #24 — point there, don't duplicate.
3. **Hands-on checklist**, naming the two Done-when items only a human can check: **#2** (it's in the menu with its icon, and the running window groups under it) and **#3** (`bus: loaded …` from the installed copy reads `vah265dec` / `memory:DMABuf` / `egl`).
4. **Whether to cut `v0.1.0`** — the user's call.

## Deliberately not in this phase

AppImage, Flatpak, Windows, an apt repository, auto-update, code signing, the GStreamer 1.28 `whispertranscriber` migration, caching the whisper context (#62).
