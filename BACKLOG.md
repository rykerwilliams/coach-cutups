# Backlog

Deferred items from the scoreboard work (spec → plan → execution → review cycle).
Each entry: what, why deferred, when to revisit.

## Spec / plan corrections (low priority — code is correct, docs lag)

### 1. Spec clock table uses `now ≤ tH1End`; code uses strict `now < tH1End` — RESOLVED
- Spec table updated to strict `<` with a note explaining why (asymmetric to rows 4-5 because of the missing-tag fallback collision). Plan's quoted code block also updated.

### 2. Plan references `CoachCutups.xcodeproj` / scheme `CoachCutups` — RESOLVED
- 12 plan references updated to `apple/VideoCoach.xcodeproj` / scheme `VideoCoach`.

### 3. Plan didn't note `xcodegen generate` is required after creating any new App-target file — RESOLVED
- Plan header now includes a callout block: run `xcodegen generate --spec apple/project.yml` after creating any file under `apple/App/**`. `apple/VideoCoachCore/**` files are SwiftPM-discovered automatically.

## Code follow-ups (not blocking — flag if related work happens)

### 4. `ScoreboardReplayOverlay.Coordinator.clip` is now refreshed on every `updateNSView`
- Fixed in commit `435fed7` (final-review polish).
- Still worth flagging: the coordinator's `clip` capture being stale was a
  latent bug only because `recordingDuration` happens not to change via the
  current undo paths. If clip-level undo ever extends to recording duration,
  audit this overlay (and `StrokeReplayLayer` for the same pattern).

### 5. `MatchEventKind.isHalfTag` has one real call site — RESOLVED
- Inlined the switch at `Workspace.tagMatchEvent`; dropped `isHalfTag` and
  reworked `setHalfTag`'s precondition to switch on `kind` directly. Net win:
  the call site is now exhaustively checked at compile time, so a future
  `MatchEventKind` case can't silently land in the "goal" branch.

### 6. `MatchInspectorPanel` could reuse a "tag with keyboard hint" view helper
- Buttons render `"\(displayName)  G"` etc. — a small `LabeledTagButton` view
  would centralize the formatting. Two call sites today; not worth abstracting.
  Revisit if a third tag-button surface appears.

### 7. `drawText` in `ScoreboardDraw.swift` does an extra context-flip + translate
- Could use `CTM = scale(1, -1)` via `textMatrix` to flip glyphs in place,
  avoiding the saveGState / translate / scale / restore dance. Working code,
  tests pass, the rewrite has a non-zero baseline-math-mistake risk; skipped
  during the final review. Worth visiting next time someone touches the
  function (e.g., when adding a second overlay that needs the same helper —
  extract it then).

### 8. `CompilationInstruction` carries three correlated scoreboard fields — RESOLVED
- Collapsed `scoreboardConfig` / `matchEventsAbs` / `clipStartAbsSeconds` into one nested `ScoreboardContext?`. Compositor's read is now a single optional unwrap; the "if config is nil the other two are ignored" invariant is enforced at the type level. Also removes one wasted per-frame `absNow` add when no scoreboard.
- `make(...)` builder collapsed three params to one (existing test call sites unchanged — they used defaults). Public `export(...)` signature unchanged; `ExportSheet` and the E2E test untouched.

## UX gaps (no spec coverage; surface if users hit them)

### 9. `matchLengthSeconds` UI bound through `* 60 / 60` Stepper — RESOLVED
- Wave 2 replaced `matchLengthSeconds: Int` with `MatchFormat`, and Wave 2
  review added `regulationPeriodMinutes` / `overtimePeriodMinutes` derived
  properties so the Stepper binds directly without `Binding(get:set:)`.

### 10. Stoppage time has no upper cap
- Per spec, deliberately uncapped. If extreme injury delays produce
  `+15:23`-style strings, the new `plusRect` width (`clockW * 1.0`, set in
  commit `435fed7`) is wide enough through `+99:59`. Beyond that, text
  centering will clip. Reasonable for the YAGNI bar.

### 11. No "rapid undo coalescing" for match-event tagging
- Each keypress = one undo entry, matching the existing `editClip` granularity.
  If users complain about Cmd-Z needing 20 presses to unwind a goal storm,
  coalesce consecutive `editMatchEvents` actions within e.g. 500ms.

## Wave 2 deferred (match-inspector revamp)

### 12. Always-visible event picker vs `eventModeActive` toggle
- Wave 2 ships an `E`-triggered overlay (`eventModeActive`) with three buttons
  (1/2/3 → Home Goal / Away Goal / Start-Stop). Adversarial review raised the
  question: if the only ways to fire those events are the keyboard `1/2/3`
  or clicking the buttons, why have the toggle at all? A permanently-visible
  compact row would remove `eventModeActive` from the inspector entirely
  (the keyboard still needs the mode flag to disambiguate from zoom).
- Deferred because: this is a UX call, not a code call. The toggle gives the
  user explicit visual cue that "1/2/3 is now in event-tag mode, not zoom" —
  helpful when the cursor is over the source video and zoom is the muscle-
  memory default. If we make the picker permanent, we need a different way
  to signal that.
- Revisit if a user reports the toggle feels noisy/redundant.

## Clip transcript + summary (Apple AI)

### 13. Audit `swiftLanguageModes: [.v5]` in `VideoCoachCore/Package.swift`
- **Why deferred:** Bumping to Swift 6 mode surfaced a real
  `AVAssetExportSession`-is-not-`Sendable` issue in `CompilationExporter.swift`
  (`Task.detached` captures non-Sendable `AVAssetExportSession`). Fix requires
  `nonisolated(unsafe)` wrappers or a Sendable shim — non-trivial complexity
  for an audit that wasn't part of this feature's scope.
- **When to revisit:** When other work touches `CompilationExporter` or when
  Swift toolchain updates make the Sendable annotation cheaper to satisfy.
  Inline comment on the pin explains the constraint.

### 14. `DeviceWiringModifier.body` chained `.onChange` modifier split
- **Why deferred:** The Swift 6.2 / macOS 26 toolchain can't type-check the
  four-modifier chain in one go. Stepped `let stepOne / stepTwo / stepThree`
  workaround documented inline. Collapsing to a single chain still triggers
  the type-checker timeout under this SDK.
- **When to revisit:** Whenever the SDK or compiler resolves the inference
  budget regression. Test by trying the collapsed form and rebuilding.

### 15. Verify `SpeechAnalyzer` authorization flow on macOS 26
- **Why deferred:** `AppleClipIntelligence.requestSpeechAuthorizationIfNeeded`
  uses the legacy `SFSpeechRecognizer.requestAuthorization` API. Public docs
  at implementation time did not confirm whether `SpeechAnalyzer` shares this
  auth gate or has its own. Conservative: keep the SF guard; worst case it's
  an extra check that no-ops.
- **When to revisit:** First manual smoke test. If granting Speech permission
  doesn't propagate to `SpeechAnalyzer`, the guard might need replacement.

### 16. Test coverage: `.transcribing` → `.summarizing` phase transition — OBSOLETED by the Linux port spec (summarization is dropped; see #22)
- **Why deferred:** `TranscriptionCoordinator` correctly sets `currentPhase`
  after the transcript write, but no test asserts the in-flight state
  transitions visible to the inspector. The code path is short and correct;
  a refactor that moved the phase assignment would produce an obvious UI bug.
- **When to revisit:** When touching coordinator state machine or adding new
  pipeline phases. Add a test using `FakeClipIntelligence.transcribeDelaySeconds`
  + `summarizeDelaySeconds` to observe both intermediate states.

### 17. Cmd-z while focused on transcript/summary field reverts AI write too
- **Why deferred:** Explicit spec decision (see "Edge case (accepted)" in
  the design spec). If the user is typing in transcript or summary while an
  AI write lands, the focus-loss flush bundles the AI write into the user's
  undo step. Window is small (active typing during the few seconds between
  job-start and summary-land); recovery is one Transcribe-button click. The
  fix (per-field diff in the focus-loss flush) was judged worse than the
  original.
- **When to revisit:** If users actually report this in practice. Inline
  comment on `Workspace.applyAIWrite` documents the rationale.

### 18. No "queued" state in the inspector
- **Why deferred:** When a second clip is enqueued behind an in-flight job,
  `coordinator.state(for: queuedClip.id)` returns `.idle` — same as
  never-transcribed. The inspector shows the Transcribe button as enabled.
  Minor UX gap; user could re-click and would see the request silently
  deduplicate.
- **When to revisit:** If two-recordings-in-quick-succession becomes a common
  workflow. Trivial fix: add a `.queued` case and surface it in the button label.

### 19. First-run speech model download UX
- **Why deferred:** Apple's `AssetInventory.assetInstallationRequest` blocks
  transparently inside `transcribe()`. First-run UX is "Transcribing…
  (longer than usual)" with no explicit progress. Spec accepted this; if it's
  painful in practice, add a "Downloading speech model…" caption swap.
- **When to revisit:** First manual smoke test on a fresh machine.

## Linux port (spec `docs/superpowers/specs/2026-09-19-linux-port-design.md`)

### 20. Chrome coordinate space for non-16:9 sources
- **Why deferred:** With the port letterboxing the base image, the text bar, PiP
  and scoreboard can lay out in output space (overlapping the letterbox bars,
  reading like broadcast furniture) or content space (staying inside the
  picture). Purely cosmetic, and it only differs for non-16:9 sources. There is
  no evidence either way in the macOS tree because its non-uniform stretch
  collapses the two spaces. Spec recommends output space.
- **When to revisit:** Phase 7 (clip preview), the first time a non-16:9 source
  is composited. Only the three chrome layers change; the stroke denormalization
  rule is unaffected either way.

### 21. `Project` ownership in the command bus — RESOLVED (Phase 2 spec D5: the bus thread owns it and publishes `Arc<Project>` snapshots)
- **Why deferred:** Whether the bus owns `Project` (every mutation is a
  `Command`) or the UI owns it and the bus owns media only. Spec recommends the
  bus owning it and emitting `Event::ProjectChanged(Arc<Project>)`, with the UI
  deriving Slint properties from the snapshot — it makes undo unambiguously
  bus-side and avoids partial-update bugs. Not locked because it depends on
  Slint's property model, which nobody has prototyped against.
- **When to revisit:** Phase 2 plan, after a Slint property-model spike.

### 22. whisper model distribution
- **Why deferred:** Bundle (~140 MB package), download on first run, or require
  a user-supplied path. Spec recommends download-on-first-run with explicit
  prompt and progress. Note this does not newly break an offline guarantee —
  see #19, the macOS app already downloads a speech model inside `transcribe()`.
- **When to revisit:** Phase 10. Decide before the packaging phase, since it
  changes the artifact size.
- **Resolved 2026-09-21:** download on first use, implemented in Phase 11
  (spec `2026-09-21-linux-port-phase-11-design.md` S3). Note the "~140 MB"
  above is `base.en`; the default `small.en` is 487.6 MB, 3.3x that.

### 23. README accuracy: "no network calls" and "No FFmpeg" — RESOLVED
- **Why deferred:** Both claims on README line 5 are or will be false. "No
  network calls" is already inaccurate on macOS (#19). "No FFmpeg" becomes false
  the moment `gst-libav` ships, which it must for software decode of arbitrary
  match film. Not fixed now because the README describes the macOS app, which
  still ships.
- **When to revisit:** When the Linux build becomes the primary artifact
  (Phase 11), or sooner if the macOS README is touched for any other reason.
- **Resolved 2026-09-21:** the README is rewritten for the Linux port (Phase
  11 spec S7). It no longer makes either claim: it says the speech model
  downloads once, on first use, and that nothing leaves the machine after
  that; FFmpeg isn't mentioned (the package does depend on `gst-libav`).
- **Note 2026-09-22:** `ffmpeg` is now a **test-only** build dependency
  (`packaging/build-deps.txt`): `ffprobe` is the chapter test's independent
  reader of the `chpl` box (match vision spec C3). Nothing links or ships it,
  and the `.deb` still doesn't depend on it.

### 24. Linux packaging: AppImage vs Flatpak
- **Why deferred:** Flatpak sandboxing complicates camera, microphone and
  arbitrary-path file access — all three of which this app needs. Spec
  recommends AppImage first.
- **User expectation (2026-09-19):** the user pictured something like the macOS
  `.app` (one self-contained thing to download and run), which maps to
  AppImage, not Flatpak. The tension: GStreamer and the VA drivers live in the
  OS on Linux, so an AppImage must bundle GStreamer carefully and borrow the
  host's VA drivers. A `.deb` depending on Ubuntu's GStreamer is the simplest
  option for the user's own laptop. Weigh both against that expectation.
- **When to revisit:** Phase 11.
- **Resolved 2026-09-21:** a `.deb`, after the evidence overturned AppImage.
  The reasoning, and why Flatpak is a capture rewrite rather than a
  packaging choice, is in the Phase 11 spec's S0. AppImage and Flatpak stay
  open for a later phase if the app is ever handed to someone else.

### 25. Wayland vs X11 for the drawing overlay
- **Why deferred:** Freehand telestration wants low input latency and the two
  display stacks differ. No measurement exists.
- **When to revisit:** Phase 6 spike, before stroke capture is built on either.

### 26. Fate of the `apple/` tree
- **Why deferred:** Keep as reference implementation until the Linux port
  reaches parity, then delete — or keep indefinitely as a macOS build nobody
  runs. Deleting is the honest choice if nobody runs it, but the decision costs
  nothing to postpone.
- **When to revisit:** Milestone D.

### 27. macOS export bugs the port fixes but the Swift tree keeps
- **Why deferred:** The review found five live bugs in the macOS app: the export
  scoreboard clock ignores pauses and skips (`CompilationCompositor.swift:253`);
  non-16:9 sources are anamorphically distorted at fixed export resolutions;
  preview ignores `showPiP` while export honors it; the export Quality picker
  has no effect at all (`ExportSettings.bitrate` has zero production call
  sites); and unknown commentary events persist as empty-kind records that lose
  their payload. The port fixes all five by construction. Not fixed in Swift
  because `apple/` is the reference implementation and is not maintained in
  parallel.
- **When to revisit:** Only if the macOS app ships again. The scoreboard fix is
  small — pass `clip` instead of `clipStartAbsSeconds` into
  `CompilationInstruction` and call the preview formula.

### 28. Non-finite floats silently corrupt a project on save
- **Why deferred:** `serde_json` does not error on NaN or infinity — it writes
  `null`. A non-finite value in an *event* payload then re-decodes as
  `EventKind::Unknown`, so the event vanishes from replay, is re-emitted
  verbatim on the next save, and never produces a diagnostic. In a non-event
  field it becomes `null`, which no `f64` accepts, so the whole project reports
  `Malformed` and refuses to open. Either way the user loses work silently.
  Not fixed in `video-coach-core` because a serializer-side validator that walks
  the document for nulls is more machinery than the problem warrants, and
  because the crate has no producer of NaN today — `Zoom::clamped` and
  `SkipCoordinator::request_skip` were the two panic-or-corrupt paths and both
  are fixed. `CommentaryEvent::new` now carries a `debug_assert` on finiteness
  so a producer bug is loud in development.
- **When to revisit:** Phase 2 and Phase 6, which introduce the first real
  producers (player positions, gesture coordinates normalized against a
  possibly-zero-height rect). The bus contract already localizes capture to one
  place, so sanitize there. Consider also surfacing a count of `Unknown` events
  from `store::read` so corruption is observable rather than quiet.

### 29. Long-GOP 4K scrubbing on low-power iGPUs
- **Why deferred:** On the reference laptop (15 W Comet Lake iGPU), accurate
  seek on synthetic 4K HEVC with a 2 s GOP measured 191 ms median / 336 ms
  worst — the only measured case over the 250 ms budget. The user's own footage
  (HEVC 1440p30, 0.5 s GOP) seeks in 10 / 22 ms, and a 2 s-GOP 1080p60 file in
  92 / 149 ms, so nothing the user actually shoots is affected. KEY_UNIT during
  drag (37 ms median on the 4K file) already covers live scrubbing; only the
  accurate settle on release is slow. Options if it matters: detect GOP length
  at import (the gate script already measures it) and offer short-GOP proxies,
  as NLEs do. Not worth building for a source type nobody has imported yet.
- **When to revisit:** When a user imports 4K long-GOP footage (common from
  some action cameras and broadcast downloads), or if Phase 2 targets a slower
  GPU than the reference laptop.

### 30. Re-measure SkipCoordinator's burst window against real seeks
- **Why deferred:** `DEFAULT_BURST_WINDOW` (150 ms) was tuned against mpv and
  VideoToolbox. On the reference laptop an accurate seek on the user's footage
  takes 10 ms median / 22 ms worst, so a leading exact seek lands long before
  150 ms and the coordinator will rarely enter burst mode at all. That is
  probably fine — the window then mostly just delays the settle after a burst
  — but it should be tuned by feel with the real transport, not by arithmetic.
- **When to revisit:** Phase 2, once skip keys drive a real player.

## Phase 2 deferrals (spec `docs/superpowers/specs/2026-09-19-linux-port-phase-2-design.md`)

### 31. GL re-setup after a window hide
- **Why deferred:** On Wayland, hiding a Slint window destroys it, and
  `RenderingSetup` later arrives with a new GL context. GStreamer's GL elements
  hold the old display/context from NULL→READY, so recovery means cycling the
  pipeline to NULL, re-wrapping, reloading and re-seeking. Phase 2's main
  window is never hidden, so teardown is handled (synchronous NULL with an
  acknowledgement) and re-setup is not.
- **When to revisit:** The first phase that hides a window (e.g. a preview or
  export window), or if a Wayland user reports a black player after
  minimize/restore.

### 32. Recents list and a menu bar
- **Why deferred:** macOS had neither; restore-last-project covers the common
  case, and a third entry point for Open/Add adds UI surface without adding
  capability.
- **When to revisit:** When the user works across several projects regularly,
  or if Linux users expect a File menu.

### 33. Pinch-to-zoom
- **Why deferred:** winit 0.30 delivers pinch gestures only on macOS/iOS, so
  Slint's `ScaleRotateGestureHandler` receives nothing on Linux. Scroll-zoom
  and drag-pan cover the interaction.
- **When to revisit:** When winit gains Linux touchpad gestures.

### 34. Rotated source videos
- **Why deferred:** Phase 2 rejects sources whose `image-orientation` tag isn't
  `rotate-0`, because the GL path doesn't rotate and none of the user's ~74
  files is rotated. Supporting it means swapping the stored aspect and adding
  `glvideoflip video-direction=auto` to the sink bin, and the Phase 6 stroke
  coordinates would need to follow.
- **When to revisit:** The first time a user needs portrait phone footage.

### 35. Physical-key bindings for A/D and digits
- **Why deferred:** Slint key events carry text, not scancodes, so A/D and the
  zoom digits follow the keyboard layout (on AZERTY the digits need Shift).
  Arrows are unaffected.
- **When to revisit:** Only if a non-QWERTY user reports it.

### 36. Decoding slows to ~0.1× when the display is off (vsync-blocked swap)
- **Why deferred:** In Phase 2 Task 5, with the laptop's monitor DPMS-off,
  playback ran at about 0.1× in both the app and the Task 0 spike (26 GL
  uploads in 8 s instead of ~240). With `vblank_mode=0` it ran at full speed.
  The likely cause is that Slint's vsync-blocked buffer swap on the shared EGL
  context also throttles GStreamer's GL upload work; the mechanism isn't
  confirmed. It doesn't affect normal use with the screen on, but it means a
  stalled UI thread can slow decoding, which will matter for export (Phase 8)
  if export ever shares the UI's GL context.
- **When to revisit:** Phase 8, when export runs its own GL pipeline — make
  sure it uses its own context, not the UI's. Or sooner if playback stutters
  when the window is occluded or on another workspace.

## Phase 4 deferrals (spec `docs/superpowers/specs/2026-09-19-linux-port-phase-4-design.md`)

### 37. Measure and correct the commentary A/V offset
- **Why deferred:** audio from `pipewiresrc` is stamped on arrival, while
  `v4l2src` video carries kernel capture times, so audio may sit ~20–40 ms
  late. Nothing measured it against a real sync source.
- **When to revisit:** Phase 8 (PiP export), with a clap test on the real
  camera; correct with a fixed audio `ts-offset` if it's over a frame.

### 38. Orphaned recordings after a crash
- **Why deferred:** a crash loses the event log, so the `.mkv` can't become a
  clip (R7). The file stays playable but unreferenced in `recordings/`.
- **When to revisit:** after Phase 3 (which deliberately doesn't auto-delete
  unreferenced media); with a user-visible "clean up unused recordings"
  action, or at packaging.
- **Re-deferred at Phase 11 (2026-09-21):** how the app is installed doesn't change what a crash leaves in `recordings/`. Revisit with the clean-up action, which is its own UX question.

### 39. Fall back to x264 when a VA encoder is present but broken
- **Why deferred:** R4 picks the encoder by element presence. A VA element
  that exists but fails fails the recording with an error.
- **When to revisit:** if a user's recording fails at start on a machine with
  VA elements, or at packaging (Phase 11) when hardware variety grows.
- **Re-deferred at Phase 11 (2026-09-21):** one `.deb` for one laptop grows hardware variety by nothing. Revisit on the first report from another machine.

### 40. Camera format and audio-source fallbacks
- **Why deferred:** R3 refuses cameras without a 16:9 ≤1280 30 fps mode
  (parent spec and macOS rule); R1 drops the `pulsesrc` fallback (its clock
  was measured ~473,000 s off monotonic, and the target runs PipeWire).
- **When to revisit:** Phase 11 packaging, or when a real camera or system
  hits the refusal.
- **Re-deferred at Phase 11 (2026-09-21):** packaging turned out not to touch capture — the `.deb` runs the same capture code as `cargo run`. Revisit when a real camera or system hits the refusal.

### 41. Live device list and global device preferences
- **Why deferred:** devices are enumerated when the popover opens, not
  watched; preferences are per project (macOS parity).
- **When to revisit:** if hot-plugging a camera while the popover is open
  proves annoying, or if re-picking devices per project does.

### 42. PiP checkbox, start flash, level-meter polish
- **Why deferred:** `show_pip` has no consumer until export (Phase 8), so it
  takes `pip_for_new_recordings` (default true) with no UI. The red start
  flash and the meter's 1 s peak hold and colour gradient are macOS polish.
- **When to revisit:** the checkbox shipped in the Phase 3 inspector
  (per clip); the polish at the end of the port.

### 43. `a_player_error_is_reported_and_play_recovers` is timing-sensitive under load
- **Why deferred:** during the Phase 4 review it failed twice in full
  workspace runs with the machine at load ~19 (a second typefind error
  arrived after the reload), then passed repeatedly on the same tree and on
  a clean checkout. Not touched by Phase 4.
- **When to revisit:** if it fails in CI; make the test tolerate repeated
  errors from one failure, or wait for the reload's `Loaded` before asserting.

## Phase 3 deferrals (spec `docs/superpowers/specs/2026-09-19-linux-port-phase-3-design.md`)

### 44. Clip-edit undo coalescing, tag-overview Duration sort, suggestion ↑/↓
- **Why deferred:** macOS parity items that add UI state or Slint key
  plumbing for small gains: one undo step per field session is already
  predictable; the overview sorts A–Z; Tab takes the top suggestion and
  typing narrows the list.
- **When to revisit:** if any is missed in real use.

### 45. Multi-select and bulk tag edits
- **Why deferred:** macOS had single selection only.
- **When to revisit:** if tagging many clips at once becomes a chore.

## Phase 5 deferrals (spec `docs/superpowers/specs/2026-09-19-linux-port-phase-5-design.md`)

### 46. Export on machines without surfaceless EGL, and more encoders
- **Why deferred:** the exporter uses `GLDisplayEGL::new_surfaceless()`
  (Mesa); proprietary NVIDIA drivers may lack the extension, and export then
  fails loudly. `vah264enc` and `nvh264enc` aren't on any test machine, so
  their settings would be untested.
- **When to revisit:** when someone runs the port on NVIDIA or an AMD/VA
  machine with `vah264enc`; add an EGL-device or GBM display path and the
  encoder entries then, measured.
- **Re-deferred at Phase 11 (2026-09-21):** the release pipeline proves export on Mesa (llvmpipe in CI, Intel on the laptop) and nothing else. Unchanged trigger.

### 47. Rare hang when a second bus shuts down while its player is prerolling — RESOLVED
- **Why deferred:** seen only in tests: open → shutdown → open → shutdown in
  one process hung in the second `shutdown()` 3 times in 200 runs under 4×
  parallel load; a single open/shutdown never hung (240 runs). The bus thread
  looked stuck while the player was prerolling a load. It may mean closing the
  window mid-load can freeze the app. User chose to keep moving on the phases
  (2026-09-19). An unfinished investigation's diff is in the session
  scratchpad (`shutdown-hang/wip.diff`), not committed.
- **When to revisit:** if closing the app ever hangs, or during the end-of-port
  hardening pass.
- **Seen on CI, 2026-09-21:** GitHub run 35668867690, attempt 2, hung in the
  harness test `a_cancel_clears_the_queue_and_leaves_no_failure` for over two
  hours before being cancelled — the test opens a project and shuts down
  within about a second, so the bus is still loading the source when the
  shutdown lands, which is this entry's shape. Both waits in
  `BusHandle::shutdown` (`acked.recv()`, `thread.join()`) are unbounded. The
  same test passed in the run before it and in attempt 1. CI jobs now carry
  `timeout-minutes`, so a recurrence fails in minutes instead of hanging. Its
  own revisit trigger — the end-of-port hardening pass — has now arrived. (spec `docs/superpowers/specs/2026-09-19-linux-port-phase-6-design.md`)
- **Resolved 2026-09-21:** a GStreamer 1.24.2 deadlock, not the bus. Taking
  `playbin3` down (READY or NULL) while a load's typefind thread is reporting
  the file's type deadlocks its `urisourcebin`: the state change holds the
  bin's state lock and waits for the typefind thread to stop, and that thread
  is waiting for the same lock to plug `parsebin` (gdb, every thread, in both
  the player and the bus; newer GStreamer no longer takes the lock). Any
  window close, source unload or reload inside a load's first milliseconds
  could hit it. `SourcePlayer::take_down` now lets a load still prerolling
  finish (bounded at 5 s) before any downward state change, and
  `taking_the_pipeline_down_mid_load_never_deadlocks` fails without it.

### 48. Live drawing overlay has no automated coverage; two accepted gaps
- **Why deferred:** `wire_drawing`, `show_strokes`, `clear_drawings` and the
  tick's expiry/rebuild live in `main.rs`, which no test binary links, so the
  live rule, the rebuild-on-change rule and "cleared on every recording
  transition" are covered only by the user's hands-on checks. Two behaviours
  are accepted rather than fixed: a mid-stroke window resize normalizes
  earlier points against the release rect, and while drawing is enabled the
  2/3 zoom keys pivot on the picture's centre (hover no longer reaches
  `zoom-area`).
- **When to revisit:** if the drawing overlay grows (a palette, shapes), move
  its state into a testable module; fix the hover pivot if it annoys in use.

### 49. `records_h264_and_opus_with_the_file_duration` is timing-flaky
- **Why deferred:** failed once at 2.033 s vs an expected 2.000 s during the
  Phase 6 review, and passed on re-run. Live test sources plus a loaded
  machine.
- **When to revisit:** if CI flakes; widen the tolerance to a few frames.

### 50. `tests/recorder.rs` fails when its tests run in parallel
- **Why deferred:** verified pre-existing (on a stashed tree): 1–2 of 3 fail
  under parallel execution, and pass serially. Live test sources contending
  for the VA encoder is the likely cause. Found during Phase 7.
- **When to revisit:** if CI flakes; mark the recorder tests serial, or give
  them their own encoder instance.

### 51. A video-less recording would hang the preview's pump on a frozen picture
- **Why deferred:** `composite::wait_for_room` waits without a deadline, which
  is exactly what makes PAUSED work: the pump stops because the appsrcs stop
  draining. If a clip's recording had no video stream while `show_pip` is
  true, the requested PiP pad would never produce and `glvideomixer` would
  wait on it forever, leaving the pump blocked on a picture that never moves.
  Not fixed: a deadline in `wait_for_room` would break pause, and the recorder
  never writes a recording without video — only a corrupt or hand-made file
  reaches this.
- **When to revisit:** if a preview is ever seen frozen with the transport
  still saying it is playing; the fix is to check the recording's streams when
  the job is built (a probe) and drop `show_pip` when there is no video.

## Phase 8 deferrals (spec `docs/superpowers/specs/2026-09-19-linux-port-phase-8-design.md`)

### 52. No UI for the source and commentary volumes
- **Why deferred:** `preview_source_volume` / `preview_commentary_volume` are read
  by preview and export and default to 1.0, which is what macOS shipped, but
  nothing sets them. Adding two sliders is easy; the question is where they
  belong (the export sheet, the preview transport, or preferences).
- **When to revisit:** the first time a commentary is drowned out by crowd noise.

### 53. 2160p export
- **Why deferred:** measured 0.56× realtime and it only upscales the user's
  1440p footage. `Resolution::R2160` stays in the project format.
- **When to revisit:** a 4K camera, or a machine that encodes 4K faster than
  realtime.

### 54. Preview has no game audio
- **Why deferred:** Phase 8 gives export the full mix, but preview still plays
  only the commentary. Phase 7 made the recording's native branch the pipeline
  clock and the only volume-controlled element, so a second pumped audio track
  needs an `audiomixer` pad that stalls the graph if unfed, breaks the pacing
  loop (the pump is paced by the clock it would feed), needs seek and EOS
  handling for a third appsrc, and needs the scrub mute to cover both tracks.
  Export is what gets shared, so it took the audio work first.
- **When to revisit:** when judging levels by ear matters, i.e. alongside the
  volume UI (#52).

### 55. An export run leaks about 19 dmabuf fds and never plateaus
- **Why deferred:** measured over eight consecutive export runs in one
  process: open file descriptors climbed 120 → 253 (~19 a run, `/proc/self/fd`
  dominated by `anon_inode:dmabuf`), with no plateau. Thread count stayed flat
  and RSS plateaued, so it is descriptors alone. The cause is
  `composite::Gl::shared`: the surfaceless `GLDisplayEGL` is process-wide and
  deliberately never finalized, so the display's buffer pools and the dmabufs
  imported through them outlive every run. Dropping the shared display is what
  the comment on `Gl::shared` says breaks concurrent exports — every
  surfaceless `GLDisplayEGL` wraps the same `EGLDisplay`, and finalizing one
  calls `eglTerminate` for all of them, which made side-by-side exports fail to
  import frames. So the obvious fix is the one thing that is known not to work,
  and 19 fds a run is ~50 runs inside a 1024 soft limit.
- **When to revisit:** if a long session ever hits `EMFILE`, or when GStreamer
  offers a way to release a display's imported buffers without terminating the
  `EGLDisplay`. Raising `RLIMIT_NOFILE` is the cheap stopgap.

## Phase 9 deferrals (spec `docs/superpowers/specs/2026-09-20-linux-port-phase-9-design.md`)

56. **The score label overflows its cell at double-digit scores.** Measured at
  1080p: the score cell is 138.2 px wide; `"3 - 1"` is 109.4 px, but
  `"12 - 9"` is 139.8 px and `"10 - 10"` is 170.3 px. The label is centred, so
  it spills symmetrically into the home and away cells rather than clipping.
- **Why deferred:** macOS behaved identically (it never fit the score), so
  this is not a regression, and soccer — the format the spec is written
  around — does not reach double digits. Fixing it means either a fourth memo
  slot keyed on size or a narrower column, and neither earns its place until a
  format that scores in double digits exists.
- **When to revisit:** when a basketball or hockey format lands, or the first
  time a real scoreboard reads `10 - 10`.

57. ~~**A realistic club name is ellipsized to ~7 characters.**~~ **Fixed in
  `babdea6`.** The spec's premise was measurably wrong: fitting
  `"Manchester United"` to the cell needs 16.7 px at 1080p, well above macOS's
  6 px floor, so "illegible anyway" did not hold and essentially no real club
  name rendered. Labels now shrink to a floor of a quarter of full size and
  ellipsize only below it. Left here as the record of why the spec said
  otherwise.
58. **`scan_abs` can pair a new source's index with the old source's offset.**
  `scan_abs` (`crates/video-coach-app/src/main.rs`) falls back to
  `abs_seconds(ui.source_index, ui.last_secs)` when `ui.target_abs` is `None`.
  `last_secs` is written only by a *successful* `query_position()`, while
  `ui.source_index` is updated by `Event::Position` independently — so in the
  window after a cross-source seek has settled but before a query succeeds, the
  readout (and a tag taken in that instant) would pair the new index with the
  old source's seconds. That is exactly the pairing the function's doc comment
  says it prevents.
- **Why deferred:** not reproducible. A settled `Position` is only published
  once the flight is `Idle`, and the query works by then, so the window is
  empty in practice. Closing it properly needs either source seconds carried on
  `Event::Position` or `last_secs` keyed by source index — more machinery on
  the hot readout path than a window nobody has hit is worth.
- **When to revisit:** if a tag or the readout is ever seen a whole source's
  duration out, or when `Event::Position` gains a payload for another reason —
  add the source seconds to it then and the fallback becomes exact for free.

## Phase 10 deferrals (spec `docs/superpowers/specs/2026-09-20-linux-port-phase-10-design.md`)

59. **Segment timestamps and click-a-line-to-seek.** whisper returns per-segment
  `start_timestamp()`/`end_timestamp()` (centiseconds) for free, and storing them
  would let the coach click a transcript line to jump there — something the
  macOS app could never do.
- **Why deferred:** the transcript is editable. The moment the coach fixes a
  mangled player name, stored timings describe text that no longer exists, and
  every consumer needs a reconciliation story. Editable-plus-timestamped is a
  real design; editable-plus-timestamped with no reconciliation is a bug
  waiting to be found. It is also a format change (v7 → v8).
- **When to revisit:** if transcript search lands (which wants anchors anyway),
  or if the coach asks to navigate by what they said.

60. **whisper-rs's `set_abort_callback_safe` is unsound in 0.16.0.** It boxes the
  closure into a `Box<Box<dyn FnMut() -> bool>>` then installs
  `trampoline::<F>` with `F` the concrete closure type, so the trampoline
  reinterprets the fat pointer's data half. `set_progress_callback_safe`
  twelve lines above does it correctly. We work around it by passing an
  already-boxed trait object, so `F` is the boxed type and the cast is right.
- **Why deferred:** the workaround is free and local. Upstreaming it means a
  patch to a slow-moving crate (last commit 2026-03-14, now on Codeberg).
- **When to revisit:** when bumping whisper-rs — check whether the workaround
  is still needed, and whether it is still *safe*, since a fixed upstream would
  make the double box wrong in the other direction.

61. **All three whisper-rs `*_safe` callback setters leak their box.**
  `Box::into_raw` with no matching free; three small boxes per transcription
  job.
- **Why deferred:** a few dozen bytes per job on a job that allocates hundreds
  of megabytes. Recorded only so nobody spends an afternoon hunting it.
- **When to revisit:** never, unless a future caller sets callbacks in a loop.

62. **Cache the `WhisperContext` across queued jobs.** Phase 10 runs one
  worker thread per job, copying the `Exporter` precedent, so a six-clip
  queue loads `ggml-small.en.bin` six times.
- **Why deferred:** an earlier draft specified a long-lived worker holding the
  context, justified as saving "minutes" on a six-clip session. That was
  overstated — a warm `whisper_init_from_file` is sub-second — and it
  contradicted the spec's own "the bus thread owns the queue", needing a
  second channel and a drain protocol the design never named. It would also
  hold 466 MB resident through an entire recording session, beside the capture
  pipeline, on a 15 W laptop.
- **When to revisit:** if the Phase 10 closeout's throughput measurement shows
  model load is a material fraction of a job. The clean shape is a long-lived
  worker that drops the context whenever the queue empties *or* a blocker
  starts, which is what makes it more than a one-line change.


63. **A truncated recording that EOSes cleanly still transcribes as complete.**
  `Reader::rest` tells a cancel and a posted `ERROR` apart from EOF, but a
  Matroska/WebM file cut mid-stream — which `finish_recording` already knows it
  produces, since it emits `UserError::StopNotClean` — commonly EOSes with no
  bus error at all. `rest` then returns `Ok(partial)` and whisper transcribes
  ten seconds of a ninety-second take as the whole thing. S4's `""`-means-never
  -transcribed makes a short transcript indistinguishable from a right one, so
  nothing tells the coach.
- **Why deferred:** the check itself is cheap — `Clip::recording_duration` is
  already on the clip, and a run that read materially less than that is
  partial — but it means threading an expected duration down into media's
  `read_all`, which is a real interface change on a path shared with export,
  for a case that needs a crash or a kill mid-recording to reach.
- **When to revisit:** when the extraction interface is next opened anyway
  (Phase 11's model download touches neither, but a streaming or partial-
  transcript feature would), or if `StopNotClean` turns out to be common on
  real hardware rather than the backstop it is meant to be. The honest fix is
  for `read_all` to report how much sound it got and for the bus to mark a
  short read as `Finish::Failed("the recording is incomplete")`, which reuses
  the slot Phase 10 already has.

64. ~~**A panic inside a transcription job would wedge the queue for the
  session.**~~ **Fixed in P3 Task 3.2**, `video-coach-media/src/job.rs`. The
  deferral said to revisit this "if the job thread grows a path that can panic
  on data", and vision is that path: an analysis indexes tensors and slices on
  what it decoded, and it shares the one running slot with transcription and
  tracking (spec B2), so a job that never finished would now stop all three.
  Every job thread starts through `job::spawn`, which sends the body's own
  terminal message or `JobMessage::panicked` carrying the panic's text, and
  then drops the sender. Left here as the record of why it waited.

65. **A cancelled whisper run keeps eight threads busy for ~12 s after the
  coach has moved on.** The abort callback is consulted once per encode and
  once per decode pass, so `Transcriber::drop` cancels and lets the thread go
  (spec S5). When a recording is what preempted it, that abandoned run
  overlaps the capture encoder at the start of the take; when the queue starts
  the *next* job immediately afterwards, two whisper contexts can be resident
  at once.
- **Why deferred:** the obvious guard — hold the `JoinHandle` and refuse to
  start a job while a cancelled one is still dying — stalls the queue
  silently, because `run_next_if_idle` only runs at the bottom of a bus turn
  and nothing wakes the bus when that thread finally exits. Making it correct
  needs a deadline, which is more machinery than a CPU spike deserves.
- **When to revisit:** if the closeout's throughput measurement shows the
  overlap costing real time on a take, or when a cheaper stop exists — a
  whisper.cpp whose abort is honoured per graph node would make the whole
  question go away, so check it when bumping whisper-rs (BACKLOG #60).

## Phase 11 deferrals (spec `docs/superpowers/specs/2026-09-21-linux-port-phase-11-design.md`)

66. ~~**`a_file_with_no_audio_track_is_a_failure` flaked once under a parallel
  run.**~~ **RESOLVED — it was not a flake.** In `video-coach-media`'s
  transcribe tests, `matroskademux`'s "Internal data stream error" sometimes
  reached the test before the "no sound" answer it asserts. GitHub's runner
  then failed it every time, which made it reproducible: `Reader::start`
  checked its error slot before its stream-collection slot, so the demuxer's
  follow-on `not-linked` error could beat the "no audio track" answer it
  follows. Reading the collection first makes a video-only file always
  report "no sound" (commit `bb17011`).

67. ~~**A frame-accurate scrub on a full Trace file lands 0.2–0.3 s off
  target.**~~ **RESOLVED — it was never off: the targets were misprinted.**
  The readings came from a throwaway bus test that scrubbed to fractions of
  the file's duration (½, ⅕, ⅘, 0.35) and printed each *target* rounded to
  whole seconds (`{:.0}`), next to the position at full precision. On a
  1623.3955 s half, ½ is 811.69775 and 0.35 is 568.188425, and the player
  reported exactly those; the 70 s remux's "25 → 24.50" was 0.35 × 70.011
  = 24.50385, likewise exact. Every one of the eight readings equals its true
  target to the printed digit. Nothing on the UI path loses time either: the
  scrubber's value is a continuous `f32` (0.25 ms resolution at an hour), the
  release sends the last moved value, and the readout shows the seek's
  target until it settles.
  - **Standing proof:** the ignored
  `real_footage_scrubs_land_on_the_frame_export_picks` (20 targets per half,
  System and production sinks) — on two Trace halves, all 80 landings
  reported their target to <0.1 ms and showed the frame export picks — and
  the CI guard `a_paused_scrub_shows_the_frame_export_picks` on an
  edit-listed fixture.

68. **The volume slider still uses Slint's stock slider.** A volume drag that
  leaves the window on X11 ends with a cancel the stock `SliderBase` ignores
  (it returns early for any button but the left), so the volume applies but its
  `released` never fires and the value isn't persisted.
- **Why deferred:** the same Slint behaviour froze the scrubber, which got a
  custom `scrubber.slint`; the volume slider's failure is cosmetic by
  comparison — the level changes, it just isn't remembered.
- **When to revisit:** if the volume ever doesn't stick, or when the scrubber's
  wrapper is generalised.

69. **The preview can starve under heavy load from other programs.** While
  measuring A/V sync, many preview runs delivered 4–80 frames in 50 s instead
  of 1500, on both audio sinks and both source files, while another session's
  GPU/CPU-heavy process was running (and occasionally without it). The preview's
  Rust pump ran at the test's `nice 19` priority there.
- **Why deferred:** seen only in tests at lowest priority under deliberate
  contention; the clean runs were perfect (1500/1500 frames).
- **When to revisit:** if the coach's preview ever stutters or freezes while
  something else is running — check whether the pump needs a higher priority or
  the preview a way to drop frames gracefully.

70. **A heap-corruption abort once, tearing down whisper in the harness.** One
  run of `crates/video-coach-harness/tests/transcribe.rs` died with glibc's
  `corrupted size vs. prev_size` (SIGABRT); three reruns were green. The
  Android branch (`claude/android-tablet-port`) records the same crash as its
  BACKLOG #69, a whisper teardown crash.
- **Why deferred:** seen once, not reproduced, and unrelated to the change it
  surfaced under (the `frame_at` boundary fix).
- **When to revisit:** soon if it recurs — heap corruption in a native library
  can crash the real app mid-transcription, not just a test. Start from the
  Android branch's findings, and try the harness under a memory checker
  (`MALLOC_CHECK_=3`, or valgrind on the single test) to catch it at the write
  rather than at the free.
- **Update (2026-09-22, match-vision Task 1.2):** not whisper-only. The GL
  export suites abort the same way (`corrupted size vs. prev_size`,
  `malloc(): mismatching next->prev_size`): 2 of 20 runs of the media
  `tests/export.rs` binary at 07312a2, 3 of 20 with Task 1.2, and once in the
  harness's `tests/export.rs`. None under gdb (25 runs). No unsafe code was
  added, so a race in GStreamer, Mesa or llvmpipe teardown is the lead. It is
  now recurring, so it is due: `MALLOC_CHECK_=3` or valgrind on the media
  export binary.

71. **A possible thump at the start of every commentary recording.** The
  Android session's audio analyzer found that real Linux recordings open with
  a full-scale negative DC step decaying over ~350 ms (its BACKLOG #70 on
  `claude/android-tablet-port`). Suspects: `pipewiresrc` start-up, or
  `opusenc`. Export's audio ramp is only `RAMP_SAMPLES` = 240 samples (5 ms),
  far too short to hide it.
- **Why deferred:** not yet confirmed by ear, and it can't be reproduced with
  the test capture sources (`audiotestsrc` has no hardware start-up); the
  agent working here doesn't open the real microphone without a task that
  needs it. Added to the hands-on checklist instead.
- **When to revisit:** if the user hears a thump or click at the start of a
  take. Likely fixes, cheapest first: fade the first few hundred ms in, or put
  a DC-blocking high-pass (`audiocheblimit mode=high-pass cutoff=20`) before
  the encoder; find the true source first by recording a few seconds with and
  without each element.

72. **A source's first load once never prerolled, on CI.** GitHub run
  35697707647, attempt 1: `a_seek_in_the_final_second_stays_in_its_source`
  (`crates/video-coach-harness/tests/transport.rs`) opened its project, the bus
  issued the load of source 0 at 0 s (`Position { target_abs: Some(0.0) }`), and
  no settled position followed within the harness's 15 s. Attempt 2 of the
  same commit passed, as did the runs before and after.
- **Why deferred:** once in roughly seven runs, and not reproduced. It is
  probably not the BACKLOG #47 fix: `SourcePlayer::take_down` only waits when a
  load is already in flight (READY, going up), and a first load starts from a
  stopped player — unless two loads were issued back to back at open, when the
  second would wait up to `LOAD_SETTLE` (5 s) on the first.
- **Recurred locally, 2026-09-24**, on the same symptom and a different test in
  the same file: `a_skip_burst_then_a_scrub_release_never_sticks` timed out
  "waiting for settled at 0 in source 0" with `ProjectOpened` still unconsumed,
  during a full `cargo test --workspace` on a machine also running a release
  build. The same suite reran green. So it is the **open**, not the skip burst,
  and it is not CI-only.
- **When to revisit:** if it recurs on CI, or if a video ever fails to appear
  when a project opens. Capture `GST_DEBUG=*:3,playbin3:5,urisourcebin:5` on
  the failing run; check whether two loads were in flight at open.


73. **`,` looks stuck across a timestamp gap longer than half a frame.** A back
  step seeks to half a nominal frame before the shown frame's start, nominal or
  its own, whichever is earlier (`step_target`, `player/mod.rs`). Where the file
  has a real gap there, the ACCURATE seek lands on the frame after the gap —
  the frame already shown — so the step does nothing and repeating it doesn't
  help. (A frame held long is left, since its own start is earlier; but after a
  scrub lands inside one, its start is the scrub's target, so `,` creeps back
  half a frame a press until it leaves it.)
- **Why deferred:** MP4 game footage has no such gaps (Trace's irregular
  29.997 fps drifts well under half a frame); only a VP8 WebM fixture does.
  A fix needs a retry further back, or knowing the frame boundaries.
- **When to revisit:** if a coach reports `,` stuck on some file, or when a
  source with real gaps (a screen capture, a dropped-frame phone clip) is used.

74. **A reel trim can be a silent no-op.** A goal merged into the previous reel
  entry (at or before its end on the same source, spec R2) has its lead-in
  ignored, and a lead-in that reaches back past the previous entry's end is
  clamped to it, so setting either changes nothing in the export. The Match
  panel still offers "Reel starts here" on that goal, and its row still shows
  the stored span ("−12 s / +6 s") rather than what the reel will play.
- **Why deferred:** disabling the button, or showing the clamped span, needs
  the row to know the reel's merges and clamps, which only the plan computes;
  the goals a coach tags are rarely close enough to merge. The stored trim is
  kept, so it takes effect if the earlier goal is deleted or trimmed.
- **When to revisit:** if a coach reports a trim that "does nothing", or when
  the Match panel grows a view of the reel's actual entries.

75. **A skip burst's replay margin failed once, by 54 ms.** One run of
  `a_skip_burst_while_playing_lands_where_replay_puts_it`
  (`crates/video-coach-harness/tests/recording.rs`) failed with "replay reaches
  7.5019 before the pause anchored at 7.4479". The same suite reran 12/12
  green, and two full workspace runs either side were green. Seen while the
  match-event editor's commands landed, which touch no transport code.
- **Why deferred:** once, under a loaded machine, on a timing margin rather
  than a logic error. It is not BACKLOG #70 (that is the whisper teardown).
- **When to revisit:** if it recurs, or if a coach reports a take whose replay
  drifts from where they paused. Start by printing the margin on failure.

76. **`a_cancelled_copy_leaves_nothing`'s fixtures sit on the 30 s EOS bound.**
  That test generates two 1280×720 × 900-frame H.264 sources, and
  `fixtures.rs`'s pipeline bound is 30 s; unloaded the suite takes ~29 s, so on
  any busy machine (CI included) the test fails in fixture generation with
  "fixture pipeline did not reach EOS within 30 s" — nothing to do with the
  copy it tests. Verified pre-existing on unmodified HEAD under load, 3 of 3.
- **Why deferred:** it fails loudly in the fixture, not silently in the code,
  and the copy path itself is proven by the deterministic deadlock test.
- **When to revisit:** the first time CI fails on it, or with the next test
  touching that file — a smaller fixture (fewer frames, or 640×360) is the fix,
  not a wider bound.

77. **An export queue across projects.** The coach (2026-09-24): "i open project
  1, do stuff, enqueue. then project 2, do stuff, enqueue, then start the queue
  and walk away for a bit. other apps have this sort of thing, like mkvtoolnix's
  muxer." Today an export run belongs to the open project and starts at once
  (`bus/export.rs`'s `Active`/`ExportRun`), so exporting three matches means
  sitting through three of them.
- **The shape, agreed with the coach:** an `ExportJob` is already a
  self-contained value — the plan, the sources' paths, the scoreboard, the
  cues, the tags — so **enqueue is "build the jobs now, run them later"**, and
  the queue is a list of them. The work is: don't tie the run loop to the open
  project; a panel listing what is waiting, by project name, with Start and
  remove; and one job's failure (footage moved, disk full) failing only itself.
  A first version keeps the queue in memory — closing the app loses it — and
  doesn't let a queued job be edited.
- **Why deferred:** only by the coach's own order (2026-09-24): the detection
  measurement comes first. It is not blocked on anything.
- **When to revisit:** as soon as P3's measurement is done, or sooner if the
  coach asks.

78. **App settings for the things an export writes.** The coach (2026-09-24):
  "add to the backlog an appsettings? we don't need a screen for it yet. maybe we
  already have it. e.g. the srt file gen, the other chapter track, etc. these are
  general app config settings to be turned off or on."
- **What exists already, and where.** Two homes, deliberately: per-project in
  `Preferences` inside `project.json` (`scan_volume`, the preview volumes, the
  last export resolution, quality and scoreboard mode, `pip_for_new_recordings`)
  and machine-wide in `$XDG_CONFIG_HOME/coach-cuts/state.json` (the last project,
  the speech model, the window size). **Machine-wide is the cheap one:** a field
  added to `Preferences` is a `formatVersion` bump every time, which is why the
  whisper model picker went to `state.json` in the first place.
- **What would become a setting:** whether an export writes the `.srt` sidecar,
  the `.chapters.txt` list and the embedded `tx3g` track; whether it writes the
  file tags; possibly the reel's default lead-in and tail (20 s / 6 s today,
  constants), and the avatar's pulse constants. All are "on" today with no way
  to say otherwise.
- **The decisions to take first:** which of those are a property of the *machine*
  (this coach never wants a sidecar) versus of the *project* (this match is for
  YouTube, that one is for the parents' TV); and whether a setting that changes
  what a file contains belongs in the export sheet next to its own row rather
  than in a settings screen at all.
- **Why deferred:** no screen is wanted yet, and every one of these is currently
  the right default. It becomes real the first time a default is wrong for a
  coach, and then it should arrive with its home already decided rather than as
  six checkboxes.
- **When to revisit:** the first "can I turn that off", or when a second coach
  uses the app.

79. **What the app does over a forwarded X11 display, and saying so.** The coach
  asked (2026-09-24) whether it runs over remote X11. Expected answer: **no, by
  construction** — `main.rs` selects Slint's Skia OpenGL renderer and fails
  loudly otherwise, and that renderer needs **EGL** (CLAUDE.md's decode rules:
  EGL is what lets GStreamer import decoded frames without a CPU copy; X11
  forwarding offers indirect GLX). Even if it started, forwarding raw 1080p30
  frames is gigabytes a minute with no hardware decode at the far end.
- **What to do:** run it once over `ssh -X` and record **exactly** what happens —
  a clear message naming the renderer, or a confusing crash. Then say it in the
  README beside the other runtime requirements: this is a local application, and
  remote use means streaming the screen (Sunshine/Moonlight, NoMachine, RustDesk
  or VNC on the real session), not forwarding the display. If the failure is
  confusing, make the message name EGL and point at that line.
- **Why deferred:** nothing is broken; the gap is that a coach who tries it gets
  no explanation.
- **When to revisit:** with the next README pass, or the first time anyone asks
  again. The docs session on `claude/docs` owns the README and is the natural
  place for the wording once the behaviour is known.

## P3 deferrals (spec `docs/superpowers/specs/2026-09-22-match-vision-design.md`, verdict `docs/superpowers/spikes/2026-09-24-match-vision-measurements.md`)

80. **The sixteen restarts have never been written down, and V-3 depends on
  them.** `kickoffs.txt` is the blank template in all three tagged folders, so
  D4's `W` is still the spec's guess of 150 s. Measured, `W` is the single most
  influential number in the goal rule: it sets how much of the match the
  suggestions claim, and at the chosen constants that is **57% of a held-out
  match**, which is most of the gap between the rule and chance.
- **What to do:** the coach writes the restart of each of the sixteen goals into
  `kickoffs.txt` (`<1-based source> <mm:ss>`, the moment the ball is played from
  the centre spot). The ground-truth run reads it already and scores V-3 without
  another line of code.
- **Why deferred:** it is the user's own step and nothing in the repo can do it.
- **When to revisit:** before any further work on suggested goals. Nothing else
  in P3's follow-ups is worth doing first.

81. **Task 3.6 — the detector runtime and formation-feasibility spike (L3, V-2,
  V-7) — is deferred with P5 rather than run.** Its entry condition is met (the
  goal precision bars failed), but it measures `rten` against `ort` for a
  detector nothing has decided to build, and V-2 — does the virtual camera frame
  both halves of the pitch at a kick-off? — only matters once P5 is wanted.
- **Why deferred:** the verdict says the formation check is the **only** cue
  left with a chance of clearing the bars, and whether to build it is the user's
  call, not a measurement's. Running the runtime benchmark first would pin a
  dependency choice to a phase that may never start.
- **When to revisit:** when the user asks for P5, or for P6 (click-to-track),
  which needs the same runtime and the same V-7 latency figure.

82. **P4 (showing suggestions) is not justified and is not started.** No
  `MatchSuggestion` in the project format, no `Analyze` command, no Match-panel
  row, no scrubber mark. Held out, the rule finds 7 goals of 9 with 29 false
  ones, and the periods are worse.
- **What would change it:** #80's restarts, a learned audio tagger in place of
  the level cue (the spec defers YAMNet), or P5's formation check. Not another
  threshold sweep: 540 points were tried and the best of them is worth +0.15
  over chance on footage it had not seen.
- **Why deferred:** the analysis backend is built, measured and shelved exactly
  as P3 said it would be if the numbers came out this way.
- **When to revisit:** when one of the three inputs above lands.

83. **`a_pause_while_a_flushing_seek_recovers_playing_sticks` flaked once under
  the full workspace run.** `video-coach-media`'s lib tests, 2026-09-24: one
  failure inside a `cargo test --workspace` (every other suite green), and the
  same suite reran 122/122 green on its own a minute later. The test races a
  pause against a flushing seek's recovery, so a loaded machine — the workspace
  run has several GStreamer suites alive at once — is the obvious suspect.
- **Why deferred:** one occurrence, green on rerun, and the assertion is about
  a state the app reaches constantly and no user has ever seen wrong.
- **When to revisit:** if it fails twice, or once on CI. Then print the states
  the recovery went through rather than only the final one.

84. **Music under a goals reel.** The coach (2026-09-24): "pull in soundtrack
  files that are licensed for use, e.g. rock, edm, etc. for the goals clips." A
  reel is the one export with no commentary — it is game sound alone — and a
  highlights reel is the one thing people expect music under.
- **The mixing is the easy half.** `core::audio` already builds the regions and
  the envelope the export mixes in Rust, and the reel's entries are the same
  plan every other target uses: a track becomes one more region under the whole
  compilation, ducked or replacing the game sound. `avenc_aac` already encodes
  the result.
- **The licence is the hard half, and it is the reason this is not a weekend.**
  The app is AGPL and runs on the coach's machine; the *output* is a video they
  may upload. Bundling audio means shipping files whose licence permits
  redistribution AND synchronisation AND the coach's own upload, and YouTube's
  Content ID will flag plenty of "royalty-free" music regardless of what its
  licence says. The workable shapes, roughly in order of how little they promise
  on someone else's behalf: (a) the coach points at their own files, and the app
  only mixes; (b) a curated list of links the coach downloads themselves; (c)
  the `.deb` ships tracks, which means clearing each one and carrying the
  licences in `packaging/copyright`.
- **It should feel semi-automatic** (the coach, same day): *"pick genre"* —
  choose **rock** or **EDM** on the export and the app does the rest, rather
  than picking a file and setting levels. That works with any of the three
  shapes above: a genre is a folder the coach filled once, a curated list
  grouped by genre, or the shipped set tagged. The app then picks a track that
  fits the reel's length, starts it at the top, ducks the game sound under it
  and fades it out at the end — no controls beyond the genre unless the coach
  opens something.
- **The decisions before any code:** which of the three shapes; whether the
  music ducks under the game sound or replaces it; whether the genre is
  remembered per project or per export; what happens when a reel is longer than
  the track (loop, fade, or refuse); and whether the same track is used every
  time or it rotates, since a coach exporting ten matches does not want ten
  identical soundtracks.
- **Why deferred:** it is a licensing question with a small piece of code
  attached, not the other way round.
- **When to revisit:** when the coach says which of the three shapes they want.
  (a) is buildable immediately and is the one that promises nothing on anyone
  else's behalf.

85. **Recent projects, and a drawer to switch between them.** The coach
  (2026-09-24): "'recent projects' menu or similar? also could have a project
  drawer to switch between recents? good for working with several project and
  going back and forth." They have three tagged matches in two clubs and move
  between them; today every switch is **Open Project…** and a folder picker.
- **Most of it exists.** `bus/state.rs` already keeps `last_project` in
  `state.json` (machine-wide, no format bump) and `restore_last_project` opens
  it at launch. A recents list is that field grown into a short `Vec<PathBuf>`,
  written where it is written now — in `open_project`, which is the one place a
  project is opened.
- **The list needs a name per entry, and the folder is the wrong one.** Two of
  the coach's projects are called `20260917-canfield` and
  `2016B vs Hudson 2026-09-19`; a drawer wants the project's own `name` and
  ideally its teams. Either store the name beside the path when it is opened
  (cheap, can go stale) or `store::read` each entry when the drawer opens (a
  handful of small reads, always right — and it is how a missing project gets
  greyed out rather than failing on click).
- **The drawer is the bigger half:** where it lives (a panel beside the sources,
  or a sheet), what a row shows, and what happens to unsaved state on a switch —
  today a project change goes through `open_project`, which is already the
  all-or-nothing path, so the switch itself is not the risk.
- **Why deferred:** nothing is blocked; it is friction, not a gap. It is also
  the natural companion to the **New match…** flow
  (`docs/superpowers/specs/2026-09-24-new-match-flow-design.md`), which creates
  the projects this would switch between — build them in that order.
- **When to revisit:** with the New match… flow, or the first time the coach
  says the picker is slowing them down again.

86. **The basket: a cut that spans matches.** The coach (2026-09-24), on the
  back of #85, and then more precisely: *"so i would be in project a, do a
  corner kick clip and then enqueue it, then go to project 2"* — and pressing
  Start gives **one video of all the pieces**, in the order they were added,
  each carrying its own match's scoreboard and clock. That is what they picked
  over "one file per project", which is #77's queue. **The two are different
  features that share machinery: #77 runs several jobs, this builds one job from
  several projects.** Original framing: *"especially good to get clips of the 'same thing' across different game projects?"* — every corner of the season,
  one player's goals across three matches, every time a press worked. Today a
  clip belongs to a project and an export is built from one project's plan, so
  the answer is export three reels and join them elsewhere.
- **What already fits.** Tags are the existing "same thing" (`Clip.tags`, the
  per-tag export rows), and `ExportJob` is already a self-contained value —
  entries carrying their own source paths, recordings, cues and scoreboard. So
  an export whose entries come from several projects is not a new renderer; it
  is a new way to *build* the entry list. This is the same realisation as the
  export queue (#77), one step further: the queue runs several jobs, this builds
  one job from several projects.
- **What doesn't.** The match clock and the scoreboard are per project, so an
  entry from another match must carry its own — the plan currently derives them
  once per job. The text bar's numbering ("2 / 5") means nothing across matches.
  Clip ids are unique per project, not globally. And nothing in the app can
  currently *see* another project's clips, which is the real work: a library
  view over a set of project folders, which is #85's drawer grown a level.
- **The decisions before any code:** what the set of projects is (a folder the
  coach nominates, the recents list, something explicit like a "season"); whether
  the result is an export or a saved compilation that can be re-exported; and
  whether the scoreboard is drawn per entry from its own match or dropped.
- **What the coach's own workflow needs, which is less than the library.** They
  described adding pieces **as they work**: make the clip in the project they
  are in, put it in the basket, move on. That needs no view over other projects
  at all — the basket is a list the app holds while the coach moves between
  projects, and each entry is a self-contained piece captured when it was added.
  The library (#85 grown a level) is what you need to go *looking* for pieces
  you made months ago; it is the second half, not the first.
- **Why deferred:** only by order. The first half — a basket filled as you go,
  exported as one video — is buildable now and is the smaller feature.
- **When to revisit:** after #77's queue, whose machinery it shares, or sooner
  if the coach starts gathering corners before the queue exists.

87. **Resizable panels.** The coach (2026-09-25): "resizing all the panels". Every
  column is a fixed width today — the left column is **280 px** of a window whose
  minimum is 1100×700 (`app.slint:638`, `:672`), and the lists inside it are
  capped in px too (the Match panel at `min(168px, lines × 28px)`, the
  highlights panel likewise). So a coach with a 4K screen gets the same 280 px
  of clip names as one on a laptop, and a long team name or clip name is
  ellipsized when there is room to spare.
- **What it means concretely:** a draggable splitter between the picture and each
  side column, a minimum per panel, and the widths remembered. `state.json` is
  the right home (machine-wide, no format bump, and it already keeps the window
  size), which also decides the question: a width is a property of the coach's
  screen, not of a project.
- **What to watch:** the picture's content rect is computed from the space left
  over, and three things are placed in it by the app — the live rings, the
  scoreboard image and the rubber band. They all follow the content rect
  already, so a resize is not a new class of bug, but the scoreboard is
  rasterized per device-pixel size (`main.rs`'s `show_board`) and would re-raster
  on every drag frame — it needs to raster on release, or on a coalesced size.
- **Why deferred:** nothing is broken; it is a comfort gap on large screens. It
  is also best done once rather than per panel, and it touches the same file four
  other features are queued in.
- **When to revisit:** with the next round of UI work, or the first time the
  coach says they can't read a clip name.

88. **The inset's size and corner, per clip.** The coach (2026-09-25): "avatar
  sizing and position should be settable per clip i think?" Today both are fixed:
  `PIP_WIDTH_RATIO` and a hard, flush bottom-right corner
  (`core/src/layout.rs`), with `AVATAR_BOX_RATIO` shrinking the avatar's circle
  inside that box. A clip keeps only *whether* it shows an inset
  (`Clip::show_pip`) and *which kind* (`Clip::inset`), both v10 fields.
- **The shape:** two more fields on `Clip` — a size (a ratio, or a few named
  steps) and a corner (one of four) — defaulted to today's values, so every
  existing clip renders unchanged. That is a `formatVersion` bump to 12, which
  is cheap and additive; and inspector controls beside the existing "Show avatar
  in export" checkbox.
- **What has to follow it:** the caption bar's available width — which
  `layout::bar_text_rect` now reserves at the right-hand end, so a left-hand
  corner has to move the reservation with it, not just widen it — the
  scan view's live self-view (`layout::self_view_rect` /
  `avatar_self_view_rect` — the corner must match what the export will do), the
  avatar's circle, and the GL 1×1 filler. The scoreboard is top-left, so a
  top-left inset needs a rule: refuse that corner, or let them overlap.
- **Worth deciding first:** whether a size is free (a slider) or a few steps
  ("small / medium / large"), since a free ratio makes every clip's frame a
  different shape and the reason the app has looked consistent so far is that
  it has never offered one.
- **Why deferred:** only by order — the basket is mid-build in the same files.
- **When to revisit:** straight after the basket closes out.

89. **The app's live self-view sits under the drawings; the export's inset sits
  over them.** The inset became the mixer's **top** layer on 2026-09-25, so that
  the caption bar's 60% black could not wash over the coach's face once the inset
  was moved onto the bar (`composite::install_overlay_pad`). A stroke drawn into
  that corner is therefore behind the inset in an export, while `app.slint` draws
  the live self-view *under* the live stroke layer and says in a comment that
  this is "the export's layer order".
- **The fix is one move:** put the self-view's `Image` above the drawing layer in
  `app.slint` and correct that comment. Two lines.
- **Why deferred:** `app.slint` and `main.rs` were owned by another agent for the
  change that caused this, and a stroke into the inset's own corner is rare
  enough that the app and the export disagreeing there costs a corner of a line.
- **When to revisit:** the next time anything touches the player's layer stack.
