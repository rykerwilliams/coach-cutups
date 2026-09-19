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

### 23. README accuracy: "no network calls" and "No FFmpeg"
- **Why deferred:** Both claims on README line 5 are or will be false. "No
  network calls" is already inaccurate on macOS (#19). "No FFmpeg" becomes false
  the moment `gst-libav` ships, which it must for software decode of arbitrary
  match film. Not fixed now because the README describes the macOS app, which
  still ships.
- **When to revisit:** When the Linux build becomes the primary artifact
  (Phase 11), or sooner if the macOS README is touched for any other reason.

### 24. Linux packaging: AppImage vs Flatpak
- **Why deferred:** Flatpak sandboxing complicates camera, microphone and
  arbitrary-path file access — all three of which this app needs. Spec
  recommends AppImage first.
- **When to revisit:** Phase 11.

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

### 39. Fall back to x264 when a VA encoder is present but broken
- **Why deferred:** R4 picks the encoder by element presence. A VA element
  that exists but fails fails the recording with an error.
- **When to revisit:** if a user's recording fails at start on a machine with
  VA elements, or at packaging (Phase 11) when hardware variety grows.

### 40. Camera format and audio-source fallbacks
- **Why deferred:** R3 refuses cameras without a 16:9 ≤1280 30 fps mode
  (parent spec and macOS rule); R1 drops the `pulsesrc` fallback (its clock
  was measured ~473,000 s off monotonic, and the target runs PipeWire).
- **When to revisit:** Phase 11 packaging, or when a real camera or system
  hits the refusal.

### 41. Live device list and global device preferences
- **Why deferred:** devices are enumerated when the popover opens, not
  watched; preferences are per project (macOS parity).
- **When to revisit:** if hot-plugging a camera while the popover is open
  proves annoying, or if re-picking devices per project does.

### 42. PiP checkbox, start flash, level-meter polish
- **Why deferred:** `show_pip` has no consumer until export (Phase 8), so it
  takes `pip_for_new_recordings` (default true) with no UI. The red start
  flash and the meter's 1 s peak hold and colour gradient are macOS polish.
- **When to revisit:** the checkbox with Phase 8 or the Phase 3 inspector;
  the polish at the end of the port.

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
