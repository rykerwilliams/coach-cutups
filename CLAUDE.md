# Coach Cutups — Project Conventions

## Workflow for non-trivial features

Each feature goes through a four-stage loop, with adversarial review at every artifact handoff:

1. **Brainstorm → spec** (`docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`)
2. **Adversarial review on the spec** — see "Review pattern" below. Apply fixes, then commit.
3. **Write plan** (`docs/superpowers/plans/YYYY-MM-DD-<topic>.md`)
4. **Adversarial review on the plan** — same pattern. Apply fixes, then commit.
5. **Compact the conversation before plan execution.** Plans get long; execution dispatches many subagents and consumes context fast. Start the execution phase with fresh context — re-read the plan + spec + this file rather than relying on accumulated chat history.
6. **Execute** via `superpowers:subagent-driven-development` (fresh subagent per task).
7. **Adversarial review on the shipped code changes** — apply fixes, then commit.
8. **Backlog deferred items** to `BACKLOG.md` at the worktree root.

## Review pattern (use for specs, plans, and shipped code)

For each review pass, spawn **two adversarial agents in parallel**:

- **Simplify agent** — find every place the design / plan / code is more complex than it needs to be. Recommended subagent: `general-purpose`. Frame as "adversarial simplification review."
- **Code-review / correctness agent** — find correctness bugs, fragile patterns, things that pass tests today but break tomorrow. Recommended subagent: `feature-dev:code-reviewer` for code; `general-purpose` for specs/plans.

Both agents get:
- The artifact under review (spec, plan, or diff range)
- The relevant codebase reference paths (so they can verify claims, not just trust the artifact)
- The full "user values" block (below)

After both reviews return:

1. **Group similar findings** across the two reviews.
2. **Spawn one deliberation agent per group** (in parallel). Each agent's job:
   - Research all issues in its group against the codebase
   - For each issue, decide the best long-term fix
   - Adversarial self-review of its own conclusions
   - **Defer to human** if the right fix isn't obvious
3. **Apply / skip per group**:
   - **APPLY** when the fix is strictly better than the original
   - **SKIP** when the fix is worse than the original issue (every change must earn its place)
   - **DEFER** when judgment is required from the human
4. Surface anything deferred at the end.

## User values (paste into every adversarial review prompt)

- Best long-term design over short-term tradeoffs
- It's OK to change adjacent code if it helps get to the best long-term design
- Simplicity — avoid over-engineered systems and fixes
- Don't care about effort or severity
- Care about long-term codebase quality and maintainability
- Don't need to fix every single race condition or edge case if they're super rare unless the fix has zero tradeoffs
- Pay close attention to fixes that add complexity — the fix needs to be worth it
- Every change must earn its place; if the fix is worse than the original issue, skip it
- Leave the code in a better place than we found it

## Project skills (`.claude/skills/`)

| Skill | Use it to |
|---|---|
| `port-swift-module` | Translate a module from `apple/` into `video-coach-core` without repeating past mistakes |
| `verify` | Run fmt, clippy, tests and the core dependency audit before committing |
| `measure-media` | Benchmark GStreamer decode/seek on real hardware without fooling yourself |
| `adversarial-review` | Run the review pattern below on a spec, plan, or diff |

`.claude/` is committed; personal overrides go in `.claude/settings.local.json` (gitignored).

## Build + test conventions

### Rust port (primary)

The Linux port is the active codebase. Spec: `docs/superpowers/specs/2026-09-19-linux-port-design.md`.

```bash
cargo test -p video-coach-core     # pure logic -- needs NO GStreamer
cargo test --workspace             # everything -- needs GStreamer dev libraries
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

**Running the app** (needs a display and GStreamer's runtime plugins incl.
`gstreamer1.0-gl`):

```bash
cargo run --release -p video-coach-app               # restores the last project
cargo run --release -p video-coach-app -- <folder>   # opens (or creates) a project there
```

The app must run on Slint's **Skia OpenGL** renderer (it selects it and fails
loudly otherwise): that renderer is EGL on X11 and Wayland, and EGL is what
lets GStreamer import decoded frames without a CPU copy. It logs the decoder,
the caps entering `glupload` and the GL platform on every source load (`bus:
loaded …` on stderr); that line is the zero-copy diagnostic, and on the
reference laptop it reads `vah265dec` / `memory:DMABuf` / `egl`.
`scripts/linux-gate-check.sh` measures decode throughput. Last-project
state lives in `$XDG_CONFIG_HOME/coach-cuts/state.json`; point
`XDG_CONFIG_HOME` elsewhere when testing so the real one isn't touched. With
the monitor off (DPMS), playback slows unless run with `vblank_mode=0`
(BACKLOG #36).

**Crate layout:**

| Crate | Holds |
|---|---|
| `video-coach-core` | Pure logic: project format, playback timeline, zoom, stroke replay. |
| `video-coach-media` | GStreamer: source player, capture, export frame driver, overlay rasterizer. |
| `video-coach-app` | Slint UI, command bus, event layer. |
| `video-coach-harness` | Headless integration tests driven over the bus. |

**`video-coach-core` declares no media dependency** — not GStreamer, not an image
or font crate, not a feature that pulls one in. CI runs its tests on a runner
with no GStreamer installed, so adding one fails the build rather than passing
silently. If you need a media type in core, you need a different design.

**Bus contract — caller-captured timestamps.** Any command that lands in the
commentary event log carries its timestamp (and source-position anchor) as a
field, captured at the input event on the UI thread, never assigned by the bus
handler. Queue delay would reintroduce the drift that puts drawings behind the
ball on replay. Querying position on a running pipeline is the only direct
pipeline access permitted outside the bus task.

**Pixel work split.** GStreamer owns every full-frame pixel operation, on the
GPU. Rust owns the edit (which decoded frame lands at each output PTS) and the
vector overlay layer only. This is measured, not preferred — see
`docs/superpowers/spikes/2026-09-19-compositing-throughput.md`. Do not move
full-frame resampling into Rust.

**Decode path stays zero-copy, which needs `decodebin3` AND an EGL context.**
Use `decodebin3` (or `playbin3`) with the video stream selected by caps
(`video/x-raw(ANY)`), or an explicit `demux ! parse ! <hw decoder>` chain —
never `decodebin`. And the GL context must be **EGL**: on X11 GStreamer defaults
to GLX, where 1.24's DMABuf importer is unavailable and every frame is copied
through the CPU (~11× slower; seeks ~5× slower). In the app the EGL context is
Slint's Skia renderer's, shared with GStreamer.
Verify on real hardware with `scripts/linux-gate-check.sh <file>`; see
`docs/superpowers/spikes/2026-09-19-seek-latency.md`.

**Capture records on the system clock, from time 0 = `base_time`.**
- **Sources:** the camera is `v4l2src`, for kernel timestamps and the `exposure_dynamic_framerate=0` control that stops low-light drops to 7.5 fps. The mic is `pipewiresrc`.
- **Clock:** the recorder always forces `SystemClock` (CLOCK_MONOTONIC). `pulsesrc`'s clock was measured days off.
- **Time 0:** `matroskamux` writes running time as-is, so recording time 0 is the pipeline's `base_time`, read when `set_state(PLAYING)` returns. **Never wait for PLAYING:** the mux holds preroll until the camera's first frame.
- **Event times:** `host_ns` comes from `video_coach_media::now_ns()`.
- **Tests:** they use injected test sources (`CaptureKind::Test`) and never the real camera or mic. See `docs/superpowers/specs/2026-09-19-linux-port-phase-4-design.md`.

### Reference implementation (`apple/`, not maintained)

The macOS app is kept as the reference for behavior and invariants. It is **not
maintained in parallel** and is not built by CI. Read it to answer "what did the
original do?", not to change it. Several known bugs are deliberately left in it
(see `BACKLOG.md` #27); the port fixes them by construction.

- **Core package tests:** `swift test --package-path apple/VideoCoachCore`
- **App build:** the `.xcodeproj` is gitignored, regenerated from `apple/project.yml`. After creating any new file under `apple/App/**`:
  ```
  cd apple && xcodegen generate && cd ..
  xcodebuild -project apple/VideoCoach.xcodeproj -scheme VideoCoach -destination 'platform=macOS' build
  ```
- Core package files under `apple/VideoCoachCore/**` are auto-discovered by SwiftPM — no xcodegen needed.

## Architecture notes (reference implementation)

These describe `apple/`. The Rust port's architecture is in the spec above.

- **`VideoCoachCore`** (Swift Package) holds all pure logic: data model, clock semantics, custom AVFoundation compositor, export pipeline. Tested headlessly via `swift test`.
- **App target** (`apple/App/`) is SwiftUI + AppKit interop. Workspace is `@Observable @MainActor`; ContentView owns ephemeral UI state (`@State` + `@Binding` to children).
- **`Workspace` is project-data only** — never put pure UI mode flags on it. Inspector mode, modal-flow flags, etc. live on `ContentView` as `@State`.
- **Custom compositor lives on the export path only.** Preview playback uses AVFoundation's built-in compositor because macOS 26 strips custom-compositor instruction subclasses (`ClipPreviewBuilder.swift` documents this). Overlays in preview live as AppKit overlay views above `AVPlayerView`.
- **Project file is `project.json` under the project folder**, plus a `recordings/` subdir of `.mov` clips. `formatVersion` discipline: bump on every additive schema change; migration happens at decode time, never at save. (The Rust port starts at v7 and refuses anything lower.)

## Backlog

Carry deferred items in `BACKLOG.md` (worktree root). Format: numbered list under headings (Spec/plan corrections, Code follow-ups, UX gaps). Each entry includes "Why deferred" and "When to revisit."
