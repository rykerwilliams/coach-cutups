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
- **User expectation (2026-09-19):** the user pictured something like the macOS
  `.app` (one self-contained thing to download and run), which maps to
  AppImage, not Flatpak. The tension: GStreamer and the VA drivers live in the
  OS on Linux, so an AppImage must bundle GStreamer carefully and borrow the
  host's VA drivers. A `.deb` depending on Ubuntu's GStreamer is the simplest
  option for the user's own laptop. Weigh both against that expectation.
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

### 47. Rare hang when a second bus shuts down while its player is prerolling
- **Why deferred:** seen only in tests: open → shutdown → open → shutdown in
  one process hung in the second `shutdown()` 3 times in 200 runs under 4×
  parallel load; a single open/shutdown never hung (240 runs). The bus thread
  looked stuck while the player was prerolling a load. It may mean closing the
  window mid-load can freeze the app. User chose to keep moving on the phases
  (2026-09-19). An unfinished investigation's diff is in the session
  scratchpad (`shutdown-hang/wip.diff`), not committed.
- **When to revisit:** if closing the app ever hangs, or during the end-of-port
  hardening pass.

## Phase 6 deferrals (spec `docs/superpowers/specs/2026-09-19-linux-port-phase-6-design.md`)

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

64. **A panic inside a transcription job would wedge the queue for the
  session.** `Transcriber::start`'s thread calls `on_message(Finished(..))`
  after `transcribe` returns, so a panic on the way there sends no `Finished`
  at all — and `whisper_run`'s scoped `run.join().expect(…)` re-panics rather
  than returning an `Err`. `Bus::transcribing` then stays `Some` forever,
  `run_next_if_idle` returns at its first line, and every later clip sits in
  the queue silently. `Command::CancelTranscription` is the one way out.
- **Why deferred:** there is no likely panic. Every fallible path in
  `recognise` and `read_all` returns `Err`, and ggml aborts the process rather
  than unwinding. The honest fix — `catch_unwind` around the job, or sending
  `Finished` from a guard that runs on unwind — is machinery for a case that
  has never happened, and the cancel already recovers it. (Since the job
  thread is no longer joined, a panic at least reaches stderr now instead of
  being swallowed by `Drop`'s `let _ = join()`.)
- **When to revisit:** if a panic is ever *seen* here, or if the job thread
  grows a path that can panic on data — a slice index, an `unwrap` on
  something whisper returned — rather than only on programmer error.

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
