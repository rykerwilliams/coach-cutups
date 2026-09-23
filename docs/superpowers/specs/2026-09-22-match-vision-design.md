# Match Vision: goals found for the coach, highlighted players, and a goals reel

**Date:** 2026-09-22
**Status:** Draft, adversarial review applied. The user answered Q4, Q7, Q8 and Q9 on 2026-09-22 (Decided, at the end). Q1, Q2, Q3, Q5 and Q6 are still open; the plan proceeds on each one's recommended default unless the user says otherwise.
**Builds on:** Phase 9 (match events, the scoreboard, `ScoreboardContext`), Phase 8 (the export graph, the overlay, the audio mix), Phase 10 (the transcription queue, the 16 kHz `Reader`), Phase 11 S3 (model download on first use)
**Evidence:**
- `docs/superpowers/spikes/2026-09-22-vision-feasibility.md`, cited below as **the spike**.
- A second research pass on the real footage and on existing products. It names the team, so it stays uncommitted; its technical findings are carried here and labelled **[footage]**.
- `docs/superpowers/spikes/2026-09-19-{seek-latency,export-graph,compositing-throughput}.md` and `2026-09-21-whisper-throughput.md`.

Labels, as in the spike: **[measured]** means measured on this machine or on the real files. **[cited]** comes from a named source. **[estimate]** is arithmetic, and every estimate is a number that Phase 3 (P3, below) replaces with a measurement.

---

## Goal

The coach breaks down a match faster, and the result is more useful to share:

1. **Goals and kick-offs are found for the coach.** The app suggests every goal and every period start and end. The coach jumps to each one, watches it, and confirms it with the same keystroke as today (Z, X or V), and a confirmed one feeds the scoreboard exactly as a hand-tagged one does. The same events become chapters in the app, to jump between them while scanning. In exported `.mp4` files the reel has a chapter per goal and a clip export a chapter per clip (C2).
2. **Players can be highlighted.** The coach marks a player with a coloured ring and a label such as "#7", at one frame or across a range, and the ring follows the player. It shows while scanning, while recording commentary, in the preview and in every export. The app tries to read the shirt number, and the coach typing it is always the fallback.
3. **A goals reel.** One export holds every goal, each with its build-up and its assist, cut straight from the game video with the scoreboard burned in and no commentary.

## Scope and product rules

These are the user's decisions (2026-09-22). This spec follows them and does not reopen them.

- **Detections are suggestions.** Nothing is ever written to the match without the coach's confirmation.
- **Kick-off detection is the primary goal detector.** Every kick-off except a period start comes straight after a goal, and the team kicking off is the team that conceded. Cheering is supporting evidence only. The confirmation rule has three cases:

  | Evidence | Verdict |
  |---|---|
  | Cheer, then a kick-off | Goal, high confidence |
  | Cheer, no kick-off | Near miss, discarded |
  | Kick-off, no cheer | Quiet goal, lower confidence |

- **Jersey reading is attempted but gated on research.** Typing "#7" is always available, and the jersey work never blocks anything else.
- **The reel is a separate export, not a kind of clip.** "No commentary-less clips" still holds: nothing here adds to `Project::clips`.
- **Drawing stays recording-only; highlighting doesn't.** A player highlight is not a drawing (H1): it may be placed while scanning, and it is saved with the footage. Pen drawings stay recording-only.
- **Everything runs offline on the CPU, off the UI thread, and can be cancelled.** Recording always wins.
- **Only permissively licensed models and weights.**
- **We build this ourselves.** Trace gives the user nothing but the video files [footage]: no events, highlights or player tags, and no export of any kind. No product fits: the services that detect events are cloud-only and tied to their own cameras, and nothing detects events offline or on Linux [footage].

## The footage this is designed for [footage, measured]

- **The source is Trace's "follow-the-play" cut.** Trace films from one fixed, high, dual-lens camera at the halfway line and crops a panning, zooming virtual view out of it. So the camera never moves physically, but the framing changes every frame.
- **Format:**
  - 1920×1080 H.264 High at about 5.0 Mbps, with AAC-LC 48 kHz stereo audio.
  - **One file per half**, 1610–1690 s long (about 27 min).
  - A remux of Trace's HLS web stream (`encoder=dailymotion/hls.js`). The average rate is 29.997 fps and differs per file, and the timestamps are slightly irregular.
  - `creation_time` is zeroed.
- **Nothing is burned in:** no logo, score or clock. Reading a scoreboard off the picture is not possible.
- **Players are 80–180 px tall in the wide framing and 150–400 px zoomed.** Back numbers are often readable by eye in both.
- **Artefacts:**
  - A translucent "ghost" duplicate of a player appeared at the lens-stitch seam in one wide frame.
  - The colours are strongly processed.
- **At a kick-off the virtual camera frames the halfway line and both halves,** which is what makes the formation test (D5) possible. How *often* it does so is unmeasured, and that is question V-2.
- **The machine** is an i7-10610U (4 cores, 8 threads, AVX2 and FMA, no AVX-512 or VNNI) with a Gen9.5 iGPU. Decode runs at 739 fps and 651 fps through EGL (on 1440p HEVC); GL readback of 1080p at about 232 fps [measured, spikes].

Other footage (handheld sideline phone video, broadcast with a score bug) is **not** the design target. Nothing here may break on it, but nothing is tuned for it either (see Deferred).

---

## Delivery phases

The phases are ordered by risk and value. **The ones that need no ML come first**, and each detector ships only after passing its acceptance bar (G4) on the coach's own tagged matches.

| Phase | Delivers | ML | Gate before it ships | Format |
|---|---|---|---|---|
| **P0** Round trip | Fix BACKLOG #67 (a scrub on a Trace file reports landing 0.2–0.3 s off target). Prove the scan-to-export round trip on a Trace file (H6). `,` and `.` step one frame back and forward while paused, for tagging and placing highlight keys on the exact frame (the arrows skip 3 s). The readout shows tenths while paused, so a step is visible. Fast scanning at 2×–32× (S) | none | A scrub lands within one frame, and the scan player's displayed frame is the one export picks for the same position (H6) | – |
| **P1** Reel and chapters | The goals reel with per-goal trims, MP4 chapters on every export, chapter markers on the scrubber and `[` / `]` to jump between them | none | tests | v8 |
| **P2** Hand-placed highlights | The highlight data model, the H tool, keyframed boxes, rings in scan, preview, export and the reel, colours and typed labels | none | tests | v9 |
| **P3** Measure | The analysis backend with no UI: audio and motion passes, `Analyzer`, core signals, kick-off pattern, confirmation rule, scoring tool; the runtime and detector spike (G5) | spike only | produces the bars' inputs (V-1 to V-8) | – |
| **P4** Suggestions from sound and motion | Storing suggestions, the analysis job on the scheduler, the Match panel's suggestion rows | none (DSP) | G4 bars for periods and goals | v10 |
| **P5** Formation check | Kit clustering and the formation test at kick-off candidates, dropping false kick-offs (D5). Built only if P3 shows the goal precision bars fail on sound and motion alone; P5 keeps its number even if skipped. | detector (from P3) | G4 goal precision bars | – |
| **P6** Click-to-track | A click snaps to a player, and the range fills from the tracker between the coach's keys | detector (reused) | G4 tracking bar | – |
| **P7** Jersey numbers | A research spike, then OCR voted over a track, auto-filling the label | OCR | G4 jersey bar; otherwise it stays typed | – |

**The user's tagging (G1) starts when P0 ships** and runs alongside P1 and P2. It needs only today's app and P0's fix, so it runs on an interim 0.1.1 build of P0 (the plan's choice of release point), which changes no format: the projects it tags are v7. P3 starts once one match is tagged: the signal passes, the tuning and V-2 to V-8 need only that. Its verdicts (V-1's held-out numbers and every G4 bar) wait for a second tagged match (G2). P3 is where the phase order stops being a guess: if sound and motion alone clear the goal bars, P5 is skipped and its detector arrives with P6, for tracking.

**Why P4 comes before P5.** Sound and motion need no model download, and they already give "a kick-off happened here" (D3). If P3 shows the quiet tier is too noisy without the formation check, P4 ships the high tier and period suggestions only, and P5 brings the rest (G4).

---

## Decisions

### F. Project format: the first real version bump

**F1. `store::read` must accept older versions before any bump ships.** Today it returns `LegacyProject` ("created by the macOS version") for *anything* below `CURRENT_FORMAT_VERSION`. Bumping to 8 as the code stands would make every existing v7 project unopenable, with a message that blames the wrong app. So P1 first:

- adds `MIN_READABLE_FORMAT_VERSION = 7`;
- makes `read` accept `MIN_READABLE..=CURRENT`;
- keeps `LegacyProject` for versions below 7 and `TooNew` for versions above current;
- **keeps a one-time backup on upgrade:** when `store::write` raises a file's `formatVersion`, it first copies `project.json` to `project.json.v<old>`, only if that file doesn't exist yet, and never overwrites it. That makes going back to the older build possible (restore the backup, losing the changes since), where otherwise the first save would lock it out with `TooNew`.

**F2. Every change here is additive, and read as it stands.**

- **A field added to a struct that exists at v7** (`MatchEventRecord.reel_lead_in` and `reel_tail`, and the new lists on `Project`) is an `Option` or a `Vec` with a field-level `#[serde(default)]`. `None` and empty are exactly what an older file means, so the hazard in `project.rs`'s header comment (an `f64` or `bool` defaulting to `0.0` or `false`) cannot arise.
- **The fields of a struct that is new here** (`PlayerHighlight`, `HighlightKey`, `MatchSuggestion`) get **no default at all**. Every file holding one was written by a build that writes all of them, so a missing one is a malformed file, as a missing field of `Clip` is.
- `write` always stamps the current version. A non-additive change, if one ever comes, adds its migration step then.

**F3. Why bump at all when serde ignores unknown keys.** Because an *older* build ignores them too. It would open a newer file, drop the highlights and suggestions it can't see, and save. The bump makes that build refuse with `TooNew` instead. So **every phase that stores a new field bumps once**. The versions below assume the table's order, and if the phases ship in a different order the numbers follow the order they actually ship in.

| Version | Phase | Change |
|---|---|---|
| v8 | P1 | `MatchEventRecord.reel_lead_in: Option<f64>`, `reel_tail: Option<f64>` |
| v9 | P2 | `Project.player_highlights: Vec<PlayerHighlight>`, with `HighlightKey.tracked` from the start (H2) |
| v10 | P4 | `Project.match_suggestions: Vec<MatchSuggestion>` |

P3, P5, P6 and P7 store nothing new: P5 only drops candidates, and P6's `tracked` ships with the struct in P2.

**F4. Source edits remap the new fields, exactly as they remap match events.** `move_source` and `remove_source` remap `player_highlights` and `match_suggestions`. `source_is_referenced` counts highlights, so a source with highlights on it can't be removed until they are deleted. That is Phase 9's rule for match events, for the same reason: silently retargeting them would be subtly wrong. Suggestions are machine output, so removing a source **deletes** its suggestions instead of refusing. A relink keeps a source's suggestions, as it keeps its match events: relink is for a moved file, so the footage is the same.

### C. Chapters

**C1. In the app, a chapter is a match event or a pending suggestion.** Nothing new is stored for them.

- The Match panel's list already has a row per event, with seek. P4 adds suggestion rows to it.
- The scrubber gains a marker per chapter:
  - a goal in its team's primary colour;
  - a period boundary in white;
  - a pending suggestion as a hollow marker.
- `[` and `]` seek to the previous and next chapter. Both keys are free today (`ui/app.slint`'s `handle-key`), and they yield to text fields like every other shortcut.

**C2. In a file, there is one chapter per plan entry, titled with the entry's text bar line.** A clip export gets a chapter per clip. The reel gets a chapter per goal, because each goal is an entry (R2). Using the entry is what makes chapter times exact:

- a chapter starts at `start_frame / OUTPUT_FPS`;
- that comes from `plan.total_frames()`'s quantized frames, never from a sum of durations (CLAUDE.md, Phase 8).

A plan with fewer than two entries gets no chapters.

- **Goals inside a clip export don't become chapters.** `chpl` chapters are flat, so a goal chapter would split its clip's chapter and title the rest of the clip with the goal. Mapping the time is easy: the first frame whose `source_time` reaches the goal. The reel is where goals are the unit, and Q6's whole-match export is where goals and periods would be chapters. See Q6.

**C3. Chapters are written as a hand-written Nero `chpl` box, spliced into the `.part` before the rename.**

- **Why hand-write it:** `mp4mux` 1.24.2 has no `GstTocSetter` [measured, spike §5].
- **Where the box goes:** export already reserves `moov` at the front of the file (`reserved-max-duration`). The splice appends `chpl` to `moov/udta`, grows `udta` and `moov` by its size, and shrinks the following `free` box by the same amount.
  - The file's size and `mdat` are untouched, and no `stco` offset moves.
  - The spike prototyped this, and `ffprobe -show_chapters` read both chapters back, with every frame still decoding [measured].
  - If `moov` has no top-level `udta`, the splice creates one around `chpl`. Measured today, `mp4mux` writes `udta(meta)` with both `x264enc` and `vah264lpenc`, but nothing guarantees it.
  - The `free` box after `moov` must be consumed exactly (it is removed) or keep at least its 8-byte header. A remainder of 1–7 bytes counts as no room.
  - The splice finds `moov` and the `free` after it by walking top-level boxes, and assumes nothing about their order beyond `moov` preceding `mdat`. If `moov` follows `mdat` (the reserve was not honoured), it writes nothing.
- **Layout:** `chpl` version 1, with start times in 100 ns units and titles as UTF-8 of at most 255 bytes (truncated on a character boundary). There are at most 255 chapters (`MAX_CHAPTERS`). A plan with more keeps the first 255 and notes it in the export's diagnostics. Only a clip export of more than 255 clips can reach it.
- **If `free` is too small, the file keeps no chapters, and the export's diagnostics say so.** That can only happen to a very short export with many long titles: the reserve scales with duration. Relocating `moov` and rewriting `stco` is not worth that case.
- **Where the code lives:** the box walker and the splice are about 60 lines in `video-coach-media` (`chapters.rs`). The chapter list itself (times and titles) is pure and lives in core. Its test reads the output back with `ffprobe`, so `ffmpeg` joins `packaging/build-deps.txt` as a **test-only** dependency, commented as such. GStreamer's `qtdemux` doesn't read `chpl`, and no released Rust MP4 crate parses it (`mp4-atom` 0.15.0), so `ffprobe` is the only independent reader. It is also the one mpv uses. The test fails, never skips, without it.

**C4. Who reads `chpl`:** VLC, and ffmpeg and ffprobe (so mpv too) [cited, spike §5]. Apple's players read only the QuickTime `chap` text track, and YouTube reads only timestamps in the video's description. Which of these the coach's audience uses is **open question Q5**. Nothing here depends on the answer, since `chpl` is the cheap baseline.

### R. The goals reel

**R1. A reel is an `ExportTarget`, not a clip.** `ExportTarget` gains a unit variant `Reel`. The export sheet gets one row, "All goals", left out when the project has no goals, like an empty per-tag row.

- One run, one progress model and one cancel serve every target, as today.
- **The file name** follows spec E6: `<label> - <project>.mp4`.
- **The row is not ticked by default** (the user, 2026-09-22): the reel renders about 36 s a goal, which an ordinary export should not pay for unasked. The coach ticks it.
- **The Export… button is enabled whenever the sheet would have a row** (`export_targets` is not empty), not on "the project has clips". A project with goals and no clips, which every ground-truth project is, can export its reel.

**R2. Each goal is one plan entry, with one `Play` segment.** The segment spans `[goal − lead_in, goal + tail]` on the goal's own source.

- **Defaults:** `REEL_LEAD_IN = 30 s` and `REEL_TAIL = 6 s`, overridden per goal, each side independently (R3).
- **30 s is the generous default the user asked for.** It covers the build-up and the assist of any ordinary youth move, and the coach trims it down, which is cheaper than finding footage that was cut off. It is never replaced by a guess that could be shorter (see Deferred): a cut-off assist is the one failure the reel must not have.
- **Clamps:**
  - The segment is clamped to `[0, duration]` of its source. A segment cannot cross a source boundary, as for clips. Since every Trace file is one half, a period boundary is the only boundary it could cross anyway.
  - **A segment never starts before the previous goal's segment ends on the same source.** Two goals a minute apart would otherwise replay the same footage.
  - **A goal at or before the previous entry's end on the same source makes no entry of its own.** Its moment is already in that entry, so it extends that entry's end to `max(previous end, goal + tail)`. The numbering (`<n> / <total>`) counts entries, and the merged entry keeps its first goal's text; the burned-in score still turns over on each goal's frame.
- **Which goals: confirmed ones only** (the user's decision, 2026-09-22). That is every goal match event, whether tagged by hand or confirmed from a suggestion. A pending suggestion is not in the reel: it can be a false alarm, the burned-in score wouldn't count it, and a quiet-tier one has no time to cut around (D4).
  - **So none is left out by accident,** the export sheet's reel row says "N suggested goals not confirmed" while any goal suggestion is pending (neither resolved nor dismissed, D6).
- **Order:** entries are in match order (`abs_seconds`).
- **Entry text:** `"<n> / <total> | <team> goal | <home>-<away>"`. The score is the one after the goal, from `ScoreboardContext::state_at` at the goal's own time. **Where `state_at` is `None`** (no scoreboard configured, or no period started by then) the score part is dropped: `"3 / 5 | Home goal"`, with "Home"/"Away" standing in for team names when no scoreboard is configured. Every tagged goal is in the reel, including one the scoreboard doesn't count, which shows an unchanged score. That is a tagging slip for the coach to see, not something the reel hides.

**R3. The trim is stored on the goal.** It is two fields on the goal's record, `reel_lead_in` and `reel_tail`, each an `Option<f64>` of seconds relative to the goal and `None` for the default, so setting one side leaves the other following the default. Being relative to the goal, they survive a source move or relink along with the goal. They are set from the scan position, which is where the coach is looking:

- The Match panel's goal row gets **"Reel starts here"** and **"Reel ends here"**, each with a reset, and shows the current span ("−30 s / +6 s").
- The position is captured by the caller at the click, per the bus contract.
- The command refuses a start that isn't before the goal, an end that isn't after it, and a position on a different source from the goal.
- **Undo:** trims are part of `MatchEventRecord`, so they ride on Phase 9's existing `EditMatchEvents` whole-list snapshot and its purge on source moves. No new undo action is needed.

**R4. What a reel entry puts on screen and in the mix.**

- **Picture:**
  - the game video at identity zoom, since a reel has no zoom events;
  - the scoreboard, per frame from the displayed frame's source time, so the score ticks over on the frame the goal is tagged at;
  - the text bar;
  - any player highlight whose range the segment crosses (H5).
- **No PiP.** The pad gets the existing GL 1×1 filler, the path a clip with `show_pip` off already takes.
- **No drawings.**
- **Audio:** the game's audio only, at `preview_source_volume`, with no commentary region.
- **What changes in the code:**
  - `PlanEntry.clip_id` becomes `Option<Uuid>`: `None` for a reel entry.
  - `compilation_schedule` walks a reel entry with no events, so `zoom_at(&[], t)` gives identity zoom. It stops assuming every target selects clips: `Reel` builds its entries from the goals (R2), not from `selected_clips`.
  - `audio_regions` adds a commentary region only for an entry with a `clip_id`.
  - **`ExportJob.entries` becomes `Vec<Option<EntryMedia>>`,** with `EntryMedia` unchanged (`{ recording, clip }`), and `None` only for a reel entry. A clip with `show_pip` off still has its media: the mixer needs its recording for the commentary, and `Pip::open` keeps deciding on `show_pip`. `None` gets the GL filler, and the mixer treats a commentary region with no media as silence, as it already treats a game region with no file.
  - **`OverlayFrame.clip` becomes `Option<&Clip>`,** and `None` draws no strokes. That is the smaller reshape: `stroke_replay::visible_strokes` keeps its `&Clip` argument and its call sites.
  - The pump, the mixer geometry, the audio pipeline, the `.part` rename and the progress model are unchanged.

### W. The whole-match export (the user, 2026-09-22; Q6 (a) answered yes)

The coach wants the match itself, end to end, with the clock and score burned in and **no commentary** — the film a parent or a player watches, not a cutdown.

- **W1. `ExportTarget::WholeMatch`,** one row, "Whole match", present whenever the project has a source video. Like the reel it is **not ticked by default**: it is the longest render the app can be asked for.
- **W2. One entry per source video, in order, whole.** `clip_id: None`, so there is game sound, no commentary, no webcam inset, and the scoreboard and any highlights are drawn from each displayed frame, exactly as the reel does. The entries carry no caption: the scoreboard already names the period and the clock, and a text bar across the whole match would be noise.
- **W3. Chapters are the match's own moments,** not one per entry: every period start and stop and every goal, labelled as the Match panel labels them ("Second half", "Home goal 1-0"). The reel and clip exports keep a chapter per entry (C2). A match with no events still gets one chapter per half.
- **W4. It is long, and the coach is told.** The row's detail is the running time ("2 videos · 54:12"), which is the honest warning.

### H. Player highlights

**H1. A highlight belongs to the footage, not to a clip.** It is stored on the project, positioned by `source_index` and source seconds, like a match event. Because of that it shows wherever that footage is on screen: scanning, recording, the preview, every clip export that crosses it, and the reel. **Highlights may be placed outside a recording** (the user's decision, 2026-09-22), while pen drawings stay recording-only: a highlight that follows a player only works when placed on the footage before recording, the reel can only show highlights that belong to the footage, and the recording-only rule protects the commentary's meaning, which a label on the footage doesn't touch.

- **Why not the clip's event log, like a stroke:**
  - Tracking runs on source time, offline, before the coach records.
  - A highlight describes the footage ("this player, these seconds"), not the commentary.
  - The reel has no clip to hang it on.
- **The cost:** a highlight made for one clip also shows in another clip that covers the same moment. That is usually what the coach wants (the same moment, the same player), and it is **open question Q2**.
- **Why keying by source time is right:** it is the key the scoreboard uses (`FrameSpec::source_time`). So a highlight freezes correctly when a commentary pause freezes the source, which is the spike's point in §0.

**H2. The stored shape (v9).**

```rust
pub struct PlayerHighlight {
    pub id: Uuid,
    pub source_index: usize,
    pub color: Rgba,          // stored as a colour, not a pen, like a stroke
    pub label: String,        // "#7"; empty draws no label
    pub keys: Vec<HighlightKey>, // sorted by source_seconds; never empty
}
pub struct HighlightKey {
    pub source_seconds: f64,
    pub rect: NormRect,       // x, y, w, h as fractions of the source frame
    pub tracked: bool,        // false for the coach's own keys (always, until P6)
}
```

- `NormRect` is new in core. It is source-normalized, never output space. The spike's §1b sketch had a separate `corrections` list, which is dropped: a coach key *is* a correction (T3).
- **`tracked` ships with the struct in P2,** where it is always `false`. It is what lets P6 re-track a stretch without touching the coach's keys (T3), and declaring it now means P6 needs no format change and no default: like every field of a new struct, it is required (F2).
- **A key's `source_seconds` is the stream time of the frame it was placed on** (H3), so two keys on one frame are the same number. A key replaces another only at exactly the same time, and no tolerance is stored or needed. The mutators keep `keys` sorted; `read` doesn't re-sort them.
- **With two or more keys, the range is `[first key, last key]`.** Between two keys the rect is linearly interpolated, and outside the range nothing is drawn.
- **A highlight with a single key is "at a timestamp".** It holds its box for `SINGLE_KEY_SPAN = 1.0 s`, centred on the key (`[k − 0.5 s, k + 0.5 s]`).
  - So it shows for the whole of a commentary pause on that frame, and for a readable second when the footage plays through it.
  - No frame-matching tolerance is needed.
  - On a panning virtual camera the static box drifts during that second of playthrough, the same limit as hand keys a second apart (H3).
- **Colours:** a new highlight takes the current pen (the swatch row, remembered in `state.json`). With a highlight selected in the H tool, clicking a swatch recolours it. The stored value is the colour, not the pen, as for strokes.

**H3. Placing keys by hand (P2).** `H` enters the highlight tool.

- **A drag draws a box around the player and makes it a key at the displayed frame:**
  - on the highlight whose ring it starts on, if one shows at this frame;
  - otherwise on the selected highlight, if it is on this source and the frame is within 10 s of its range, so a stale selection never stretches a highlight across the match;
  - otherwise on a new highlight, which becomes selected.

  A key at a frame that already has one replaces it (the same stream time, exactly: H2). Selecting a highlight in the inspector, or starting a drag on its ring, selects it.
- **Esc:** the first Esc deselects a selected highlight; the next leaves the tool, ahead of the rest of the Esc cascade. In `handle-key` the H tool's Esc goes before the recording's, so Esc in the tool during a paused recording never stops the take.
- **The box is mapped to source space** through the live zoom with `Zoom::source_point`, so a box drawn while zoomed is stored correctly.
- **Keys are placed on a paused picture.** A drag or click in the H tool while the picture plays places nothing and shows a hint, "Pause to place a highlight (Space)", like the drawing hint. The key's source position is **the displayed frame's stream time** (`Frame.stream_time`, H6), captured by the caller at pen-down, per the bus contract. So the box and its time describe the same frame, and `Decoder::frame_at` picks exactly that frame for it in export.
- **`,` and `.` step one frame back and forward while paused** (P0), so the coach can put a key on the frame they mean. The arrows skip 3 s.
- **The tool works while scanning or a recording is paused.** "While drawing", a coach can pause, ring a player and talk. `SetHighlightKey` joins `TagMatchEvent` on the recording allow-list. A drag in the H tool never shows the pen's "Drawing works while recording — press R" hint.
- **On a moving virtual camera, keys placed by hand need to be about a second apart.** Trace's framing pans, so the player's source-space position moves even when the player doesn't. That is the honest limit of P2, and it is what P6's tracker removes.
- **The inspector:**
  - a highlight has a label field (typed "7" is shown as "#7"), a colour and a delete;
  - **"Delete key here"** removes the key at the displayed frame, enabled when the displayed frame's stream time equals one of its keys'. Deleting a highlight's last key deletes the highlight.
- **Undo:** `UndoAction::EditHighlights { before, after }`, a whole-list snapshot like `EditMatchEvents`. It is purged from both stacks on a source move or remove, for Phase 9's reason: a snapshot isn't remapped.

**H4. How a highlight is drawn:**

- a ring at the feet: an ellipse centred on the box's bottom edge, `1.4 × box width` wide and `0.35 ×` that tall, stroked in the highlight colour with the strokes' dark edge;
- the label in a pill of the same colour above the box, placed inside the picture rect (below the box, or shifted sideways, at an edge), because the label drawing isn't masked;
- **a stroke width that scales with the picture's height**, like a pen;
- **anything outside the picture rect is clipped away.**

The mapping from source-normalized coordinates to picture pixels is the existing `Zoom::transform(src_w, src_h, picture_w, picture_h)`, applied to the displayed frame's zoom and offset to the picture rect. It is the same affine the zoom and the strokes' content rect use, so a ring can't drift from the picture. No new mapping function is added.

**H5. Where highlights are drawn:**

- **Preview and export:** `overlay.rs` draws them first, under the strokes (the coach's live pen is on top), the text bar and the scoreboard. The driver passes `highlight_shapes(&project.player_highlights, frame.source_index, frame.source_time, frame.zoom, …)`, one core function that the live layer uses too. It takes each visible highlight's interpolated source-normalized rect (`highlights_at`), maps it through `Zoom::transform` (H4), and returns the ring and the box in picture pixels, so the overlay itself stays zoom-agnostic. Nothing is derived, so there is no context object: the job reads the highlights from the project snapshot it started with.
- **No inference runs in the preview or export path.** They read stored keys only, so export stays deterministic and its throughput unchanged: the overlay is 3.6 ms a frame [measured].
- **Scanning and recording:** Slint path elements on the live layer that draws strokes. They are fed from the tick's position query, which is the one pipeline access CLAUDE.md permits outside the bus, and from the UI's zoom.
  - The ring can trail the picture by up to a frame during playback.
  - It is exact when paused, which is when keys are placed (H3).

**H6. P0's round trip is a hard prerequisite for P2 and for tagging.** A key (like a Z/X tag) is stored at the position the scan player reports, and export draws it on the frame `Decoder::frame_at` picks for that position. The two must be the same frame. They can differ when the player and the decoder disagree on stream time: CLAUDE.md's MP4 edit-list class, which BACKLOG #67's symptoms point at (on a Trace half, a scrub to 812 reports 811.70).

- **The seams:** `mailbox::Frame` gains `stream_time` (the sample's `segment.to_stream_time(pts)`), and media gains `frame_times(source, targets)`, which answers with the stream time of the frame `Decoder::frame_at` picks for each target, without an export. Neither is reachable from the harness today. `Frame.stream_time` is also what P2's highlight keys are placed at (H3).
- **The test:** extend `video-coach-harness/tests/real_footage.rs` (`#[ignore]`d behind `COACH_FOOTAGE`, as it is today). On an HLS-remuxed Trace file, scrub to about 20 targets across the half, paused. For each, take the displayed frame from the mailbox (`SinkKind::System`) and assert that its stream time equals that of the frame `Decoder::frame_at(reported position)` returns, and that the reported position is within one frame of the target.
- **#67 is also checked on the production path.** Only the System sink's frames can be read back, but the app runs `Harness::production()`'s sinks (`autoaudiosink` among them, which can supply the clock), so the same scrubs repeat there and assert the reported position is within one frame of the target.
- **Real footage is never committed:** the repository is public and the footage shows children.
- **If #67's root cause can be reproduced in a generated fixture** (for example an edit list or a non-zero first PTS), the fix also lands with a CI test on that fixture. If it can't, the ignored test is the only proof, and the spec says so in the fix's commit.

### S. Fast scanning (P0; the user's request, 2026-09-22)

A coach tagging a 27-minute half, or looking for the next goal, wants to run the match fast rather than skip 10 s at a time.

- **S1. Speeds are 1×, 2×, 4×, 8×, 16× and 32×, forward only, while playing.** `L` doubles the speed, up to 32×. `J` halves it, down to 1×. Both act only while the scanner is playing; a held key doesn't repeat. A speed button beside Play shows the speed and cycles up on a click, back to 1× after 32×. The readout shows the speed above 1× (`12:34 / 27:10 · 8×`).
- **S2. Scanning only.** Fast playback is never part of a recording. `J`, `L` and the button do nothing while recording or previewing, and a recording starts paused, so it starts at 1×. The clip model, the event log, replay and export stay 1×, so no format changes.
- **S3. Any pause returns to 1×.** That one rule covers Pause, a recording's start, a jump to a clip, the end of the last source and an unload, and so Play always starts at 1×. A scrub, a skip or running into the next source while fast keeps the speed.
- **S4. Sound is muted above 1×.** No pitch-corrected audio.
- **S5. The picture keeps up, or drops frames; it never lags.** The scan sink drops late frames (QoS), so the displayed frame stays with the position. Measured on a Trace half: every speed decodes every frame, and QoS drops what's late (32× shows ~88 fps, lag ≤ 0.6 s). Key frames only was worse at 32× (16 fps, lag ~1 s), so there is no key-frame mode.
- **S6. Find fast, tag paused.** At 32× a 300 ms reaction is 10 s of match, so the coach pauses, steps with `,`/`.` to the frame, then tags. A tag pressed while fast still works (caller-captured, as always); it is only as precise as the reaction.

### D. Suggested kick-offs, goals and periods

**D1. The analysis unit is one source,** and on the design footage that is one half.

- A job reads the file twice, with no seeking:
  - **the audio pass** uses the existing `Reader`, streaming at 16 kHz mono. That covers the whistle band (2–4.5 kHz) [spike §0].
  - **the motion pass** is `decodebin3`, then GL scaling to 160×90, then `gldownload`, then an appsink, with `videorate` dropping to 5 fps before `glupload` so dropped frames are never uploaded.
- **Two passes rather than one pipeline with two appsinks:** a pipeline with two sinks that aren't synced stalls whenever one isn't drained. The audio pass costs seconds anyway.
- **Motion is the mean absolute luma difference** between consecutive thumbnails.
  - On Trace footage that includes the virtual camera's pan and zoom. That is intended: the camera holding on the centre spot is part of the stillness D3 looks for. Removing global motion (a shift and zoom estimate on the thumbnail, still in core on the number series) is added only if V-8 shows it's needed.
  - It is computed in media on a 14,400-pixel buffer. That is analysis of a thumbnail GL already made, not full-frame work, so it keeps to the pixel rule [spike §2].
  - Core receives only the time series.
- **Cost:** about 48,500 frames per half, at about 650 fps of decode, is about 75 s [estimate from the measured decode rate]. P3 measures it (V-6).

**D2. The signals are pure functions in `video-coach-core`, on sample and number slices.**

- **Whistles** (`whistles(samples_16k) -> Vec<Whistle { start, duration, freq }>`):
  - a hand-written Goertzel bank over 2–5 kHz in 75 Hz bins, on 32 ms windows with a 16 ms hop;
  - a whistle is a peak at least 15 dB over the band's median, holding pitch within ±150 Hz, for at least 150 ms;
  - "long" is at least 0.8 s.
  - **Why no FFT crate:** core's dependency audit expects exactly four crates (`verify`). The bank costs about 2 s of CPU per half [estimate].
- **Cheers** (`cheers(samples_16k) -> Vec<Cheer { onset, peak_db }>`): broadband level in 0.3–3 kHz at least 8 dB over a 60 s rolling median, for at least 1.5 s.
- **Still intervals** (`still_intervals(motion) -> Vec<Range<f64>>`): motion below θₘ for at least 10 s.
- **Where the thresholds live:** each is a named `const` in core, and every one of them is an **initial value that P3 replaces** with one chosen on the tuning match (G2). The spike's cited figures for whistle detection (88% precision, 93% recall on broadcast) are the only external numbers, and they aren't for this microphone.

**D3. A kick-off is the pattern that follows a walk-back:** a still interval, then motion above θₘ for at least 3 s. A whistle within `[end − 5 s, end + 2 s]` is evidence, not a requirement. Whether it gates candidates is P3's call (V-1, V-5): it gates only if kick-offs without one are mostly false on the tuning match.

- **Its time `K` is the whistle when there is one, otherwise the end of the still interval.**
- **Why this pattern:** after a goal the players walk back for 30–90 s, the ball sits on the centre spot and the virtual camera holds, and then the restart is sudden [user; spike §4e].
- **What else matches it:** throw-ins and free kicks are short stoppages, and most never reach the 10 s floor. Injuries and water breaks do. That is what the cheer rule (D4) and the formation check (D5) are for.

**D4. The user's confirmation rule, as code:**

- **Goal:** a kick-off `K` that is not a period start. It is **high** tier if a cheer onset lies in `[window start, K − 15 s]`, otherwise **quiet** tier.
- **Its window is `[max(K − W, K_prev), K]`**, where the goal must be. `K_prev` is the previous kick-off kept in the same source (a goal or a period start), or 0. A goal can't come before the kick-off that restarted play, so this clamp is exact, and it makes the windows of one run disjoint. `W` starts at 150 s and V-3 sets it.
- **Its `at`** (the estimated goal instant) is the onset of the **last** cheer in `[window start, K − 15 s]`, minus 1 s, for the high tier. Earlier cheers in the window were near misses. The quiet tier has no `at`: without a cheer there is no evidence of when inside the window the goal happened.
- **A cheer with no kick-off in the following `W` is a near miss** and is not suggested. The scoring tool lists these so the rule can be checked (G3).
- **A period start** is the first kick-off in a source.
- **A period end** is the last long whistle in a source. On the design footage each file is a half, so this finds the half's own end. Other endings (three short whistles, a mid-file half-time on whole-match footage) are added only if the tuning match shows misses.
  - **Whether Trace's files include the opening kick-off is V-4.** If they don't, `auto_back_anchor_p1` already covers it.

**D5. The formation check (P5) runs only at candidate kick-offs, and only drops false ones.** It is built only if P3 shows the goal precision bars (G4) fail on sound and motion alone. It stores nothing: a kick-off that fails it is dropped, which is the precision gain, and one that passes is suggested as it would be without the check.

- **Which frames:** one frame a second in the 8 s before each `K`, read with `Decoder::frame_at`, scaled by GL and downloaded. Only these frames, never the whole half.
- **Players:** a person detector (L1) finds them.
- **Kit colour:** media samples each player's torso (the central half of the box, from 20% to 50% of its height) and passes a mean colour to core.
- **The test, in core:**
  - Cluster the torso colours into two kits (k-means, k = 2, rejecting outliers). The referee and the goalkeepers are the outliers.
  - The kits pass if a line within ±30° of vertical in the image separates them with at least 85% of players on their own kit's side, and at least `MIN_PER_KIT` players of each kit are seen. `MIN_PER_KIT` starts at 4 and is an initial value that V-2 sets. The design footage is U10, which plays 7v7, so 4 means most of a team.
  - **Why this works on the design footage:** the camera sits at halfway, so the halfway line is near-vertical in the image.
- **Why the painted line isn't detected:** the kits' separation *is* the halfway line at a kick-off, and it survives the frames where the line itself is hidden or out of frame. Line detection is deferred until P3 shows the separation test isn't enough on its own.
- **Which side scored is not detected** (the user's decision, 2026-09-22). The coach confirms with Z or X while watching the goal (D7), so a "Home goal?" label from the kit that kicks off would save no keystroke. See Deferred.
- **Detector cost:** at most 60 candidates × 8 frames × about 50–120 ms per 640-input inference at B3's 4 threads is about 25–60 s per half [estimate: the spike's 40–90 ms is for 8 threads, and on 4 physical cores hyper-threading adds little to this work]. P3 chooses the input size by far-side recall (V-7).

**D6. Suggestions are stored (v10), and resolving one is derived.**

```rust
pub struct MatchSuggestion {
    pub id: Uuid,
    pub source_index: usize,
    pub seconds: f64,                // K for a goal or period start; the whistle for a period end
    pub kind: SuggestionKind,
    pub dismissed: bool,
}
pub enum SuggestionKind {
    Goal { tier: GoalTier, window: (f64, f64), at: Option<f64> }, // at: high tier only
    PeriodStart,
    PeriodEnd,
}
```

- **Why they live in `project.json`:** re-running the analysis costs minutes, a dismissal has to persist, and source moves have to remap suggestions exactly as they remap events (F4). A sidecar keyed by path would break on relink.
- **Only suggestions are stored,** never the raw signals. The window and `at` are stored, not derived, so a later build's retuned constants never move an old suggestion.
- **The seek point:** `max(at − 10 s, window start)` for a high-tier goal, the window start for a quiet one, and `seconds − 10 s` for a period row. With the Seek bar (G4) failed, high-tier goals use the window start too.
- **A suggestion is *resolved* when a compatible match event lands in it.** A goal of either side inside a goal suggestion's window resolves it. A start/stop resolves a period suggestion if it is within ±10 s of its `seconds`.
  - This is not a stored status. So undoing a tag un-resolves the suggestion for free, and a Z, X or V pressed while reviewing resolves the suggestion it lands in.
  - That keypress is the only confirm path. The coach seeks to the suggestion, plays, and presses Z, X or V on the frame it happens.
  - Each event resolves the earliest pending suggestion it lands in.
- **Dismissal is not an undo step.** Suggestions are the machine's output, not the coach's data. A dismissed row stays in place, dimmed, and its Dismiss button reads **Restore**, so one command (`SetSuggestionDismissed`) covers both. Keeping them out of the undo stack means a re-run (D7) can never collide with an undo snapshot. Restore is what makes a mis-clicked Dismiss recoverable, since a re-run never brings a dismissed suggestion back.

**D7. The Match panel shows:**

- **"Find goals and kick-offs"**, which queues one analysis job per source (skipping any already queued or running), with its progress on the panel. Nothing records that a source was analysed: a re-run replaces only pending suggestions and keeps what the coach resolved or dismissed (below), so analysing a source again is always safe and costs only its minutes.
- **suggestion rows** interleaved with the events, in match order. A row shows its tier ("Goal?" or "Quiet goal?"), with:
  - **Seek**, which jumps to the suggestion's seek point (D6) and plays;
  - **Dismiss** (a dismissed row's button reads **Restore**, D6).
  - **Confirming is the existing keypress.** The coach presses Z, X or V on the frame it happens, which goes through `TagMatchEvent` exactly as today, so the scoreboard, undo, the cap and the bus contract are untouched. The event then resolves the row (D6). No row writes an event itself: a suggestion's times are estimates, and a tag at one would put the score change seconds away from the ball crossing the line, in every export.

  **Re-running a source** replaces its pending suggestions. A new suggestion is dropped if an existing resolved or dismissed one of the same kind has its `seconds` within 10 s, so a re-run never brings back what the coach already dealt with.

**D8. Why not the alternatives.**

| Alternative | Why not |
|---|---|
| SoccerNet-trained action spotters | The weights are non-commercial, and the best of them scores about 19% on amateur AI-camera footage [footage, cited] |
| Reading a scoreboard | Nothing is burned in [footage] |
| Ball-in-goal detection | The ball is a few pixels in the wide framing |
| Whole-match player detection | Not needed: the detector runs only at candidates |

### T. Click-to-track (P6)

**T1. A click snaps to a player.** In the H tool, a click with no drag reads the displayed frame with `Decoder::frame_at`, runs the detector once on a 640×640 native-resolution crop around the click, and takes the person box that contains the click as the key, on the same target as a drag (H3).

- **It is not a queued job (B2).** It runs on its own short-lived thread, so it never waits behind a transcription and never preempts an analysis. It may overlap a running job for a moment.
- **Cold, it costs** a pipeline start, a key-unit seek and a walk forward, a model load and one inference: about 0.3–0.6 s [estimate]. A spinner shows at the click until the box appears. V-7 measures it against G4's 1 s bar, and if it misses, the H tool keeps a decoder and a session warm while it is open.
- **A click that hits no box, or made before the detector model is downloaded,** falls back to the drag.
- **While recording, the click doesn't snap and Track is disabled** (B2: recording blocks every heavy job). A click falls back to the drag, and the coach tracks after the take.

**T2. The tracker fills between keys.**

- **Crops:** each step crops a native-resolution 640×640 region around the predicted box, by GL geometry (`gltransformation`), never by slicing a downloaded frame in Rust [spike §2].
  - **One frame is in flight:** pull the frame with `Decoder::frame_at`, set its crop from the prediction, push it through `appsrc → gltransformation → gldownload → appsink`, and pull the crop before taking the next frame.
  - **Why:** a free-running graph queues frames ahead of the inference, so a crop set from the latest prediction would land on a later frame.
  - **Rate:** the detector runs on every second frame.
- **The tracking logic** is one track reduced from SORT, in core and about 200 lines [spike §1b]:
  - a constant-velocity Kalman filter in source-normalized coordinates;
  - a gate on IoU or distance to the prediction, widened after a miss, because Trace's virtual camera pans and zooms (a jump in the whole frame reads as a jump of the player).
- **Why no learned single-object tracker:**
  - The detector is already there.
  - Same-kit crossings, the common case in football, are where single-object trackers switch identity [spike §1b].
  - Their weights bring the GOT-10k data-licence question [footage].
- **Output:** tracked keys between two coach keys, or forward from the last coach key to the end the coach set ("Track to here"). The tracker infers on every second frame, but **stores only the keys interpolation can't reproduce**: a tracked key is dropped when the box interpolated between the keys kept either side of it is within `TRACK_KEY_TOLERANCE` of it (its centre within 10% of the box's height, and its size within 10%) [initial value]. The first and last keys of a run are always kept, and a coach key is never dropped.
  - **Why:** every command rewrites `project.json` on the bus thread, and `EditHighlights` snapshots the whole list (H3). A key is about 300 bytes of `project.json`. A heavily highlighted match (40 highlights of 15 s) is about 2.7 MB at 15 keys a second, and about 0.4 MB thinned [estimate]. A synchronous save handles that in milliseconds, as it already handles strokes kept at up to 60 points a second.
  - **Why not a fixed lower rate:** it would shrink the file too, but it bounds no error, and the error is largest on a turn or a pan of the virtual camera.

**T3. Correcting the track.**

- **The track stops at a loss.** When no detection passes the gate for 0.5 s, the tracker stops and the highlight shows "lost at 12:03".
- **The coach's keys are the corrections.** The coach scrubs there, drags a key, and presses Track again.
  - Tracking between two coach keys replaces only the `tracked` keys between them. It never replaces a coach key.
  - A correction therefore re-runs only its own stretch.
- **A result that went stale is dropped.** A result arriving after the coach changed that highlight's own keys would clobber the change, so the job carries a snapshot of the coach keys and drops its result on a mismatch ("Track again").
- **Undo:** an applied result is one `EditHighlights` step ("Track"). This deliberately differs from `ClipEdit::Transcript`, the machine write kept off the stack (Phase 10) because it lands unasked and pushing clears redo.
  - **A track is asked for:** the coach presses Track and waits on it.
  - **Off the stack, it would be erased:** undoing any earlier highlight edit restores a whole-list snapshot without the tracked keys, and nothing could redo them.

**T4. Tracking is interactive, so it goes first in line and preempts an analysis (B2).** A 10 s range is about 150 inferences, which is 8–18 s at 50–120 ms each on 4 threads [estimate].

### J. Jersey numbers (P7, research-gated)

**J1. The typed label is the feature, and OCR only fills it in.** OCR runs after a track completes, on the tracked crops.

- **Every crop gets a reading.**
- **A number is accepted only if the vote is decisive:** at least 5 readings, at least 70% of them agreeing. This is the tracklet voting that makes the published pipeline work, because most single frames are illegible [spike §3].
- **It auto-fills only an empty label.** The coach's typing always wins, and a wrong fill costs one edit.

**J2. Candidates, all permissive in code, each carrying a training-data question (L2):**

- PARSeq (Apache-2.0), as pretrained;
- PARSeq, or a small digit CNN, trained on SoccerTrack v2's jersey labels (CC BY 4.0) plus synthetic digits rendered from DejaVu. This has the cleanest provenance, and our own model to maintain.
- `ocrs` (MIT/Apache, on `rten`).

The Koshkina & Elder pipeline is CC BY-NC and rejected [spike §3].

**J3. The gate:**

- **P7 starts with a spike:** about 40 of the coach's own labelled tracks (P2 and P6 highlights with typed labels are the ground truth), scored for each candidate.
- **If none clears the jersey bar (G4), P7 ships nothing** and the label stays typed. Nothing else in this spec depends on P7.

### B. Jobs, threads and cancelling

**B1. Every vision job is a worker thread in media**, shaped like `Transcriber`:

- **Jobs:** `Analyzer` for a source's match analysis, and `Tracker` for a highlight's range. A click-to-snap is not a job (T1).
- **Messages:** tagged with the job's generation and sent on the bus's single input channel.
- **Drop cancels without joining.**
- **A cancel flag is polled per frame and per inference,** so a vision job stops within about 100 ms. That is unlike whisper's roughly 12 s.
- **Every job thread, `Transcriber`'s included, starts through one helper that sends exactly one `Finished`,** turning a panic into `Failed`. With three kinds sharing one slot (B2), a job that never finished would stop all of them, and vision code indexes tensors and slices on data, which is BACKLOG #64's own trigger. This closes #64.
- **Every vision pipeline runs on `Gl::shared()`,** the process-wide surfaceless EGL display that export uses (CLAUDE.md). It never uses Slint's context and never a display of its own: a second surfaceless display's finalize `eglTerminate`s the shared one. Decoding is `decodebin3` with the video selected by caps, or the composite's `Decoder`, so the path stays zero-copy.

**B2. One heavy CPU job runs at a time, process-wide.**

- **Each kind keeps its own FIFO and its own running slot,** as transcription does today. `run_next_if_idle` starts a job only when every slot is empty and no recording, export or preview is going, taking the first waiting job in the order **tracking, transcription, analysis**.
- **A running job is cancelled only if it stops at once.**
  - A recording preempts whatever runs: Phase 10's rule, unchanged.
  - A track preempts a running analysis, or any job still downloading its model. Both stop within about 100 ms. The preempted job goes back to the **front** of its own queue.
  - **A track never preempts a whisper pass.** The cancel costs about 12 s of CPU on 8 threads, overlapping the track (BACKLOG #65), and the transcript would restart from zero. The track waits at the head of the line, and the highlight says "Waiting for transcription". This is the rule `set_transcribe_model` already follows.
- **The cost of preempting:** a preempted analysis restarts from zero. While the coach keeps tracking, an analysis can keep restarting, and it finishes once they stop for a couple of minutes. Keeping partial passes would be machinery for that.
- **Recording, export and preview still block every heavy job,** as they block transcription today.
- **Why one at a time:** whisper runs 8 threads, and two heavy jobs at once would each run at half speed while starving playback.

**B3. Thread budget.**

- **Inference uses 4 threads,** leaving half the logical CPUs for the UI, playback and decode.
- **The analysis decode is its own `decodebin3` pipeline.** The scan player keeps its own, and they share the VA decoder.
- **G4 includes "scanning stays at full rate while an analysis runs"**, measured against a scanning control in the same session, which is CLAUDE.md's UI budget rule.

**B4. Results are applied against the project as it is now.**

- A job carries its source's `relative_path` and index.
- **A result for a source that has since moved or been removed or relinked** is dropped. Suggestions and highlights key on `source_index`, which a move permutes (F4). The coach presses "Find goals" again.

### L. Models, runtimes and licences

**L1. The detector is D-FINE-N, with RTMDet-tiny as the fallback.**

- **Licences:** both are Apache-2.0 in code and weights, to be re-verified at pin. Both are fine-tuned from ImageNet-pretrained backbones (L2).
- **Weights:** COCO "person" weights, with no fine-tuning in v1.
- **Rejected:**
  - **Ultralytics YOLO:** it is AGPL, which is legal here but locks the project into AGPL forever, fine-tuned weights included.
  - **YOLO-NAS:** its weights are non-commercial.
  - **Roboflow `sports` weights:** they are Ultralytics-based and trained on data with unknown terms.
  - **Anything trained on SoccerNet:** the data is research-only.
- **If fine-tuning is ever needed,** SoccerTrack v2 (CC BY 4.0: amateur matches from fixed panoramic cameras, the same class of footage as Trace) is the dataset [footage].

**L2. Training-data licences, one row per model.**

**COCO- and ImageNet-descended Apache-2.0 weights are acceptable** (the user's decision on Q4, 2026-09-22). This table is the record of that decision: what each model was trained on, and the question it raised.

| Model | Code / weights | Trained on | The question |
|---|---|---|---|
| D-FINE-N (or RTMDet-tiny) | Apache-2.0 | COCO 2017. The annotations are CC BY 4.0; the images are Flickr photos under their uploaders' individual licences. | Whether weights learned from COCO's images carry any obligation. Nearly every permissive detector is COCO-trained and ships under its code's licence. **Accepted (Q4).** Checkpoints pretrained on Objects365 are avoided until its terms are checked. |
| D-FINE-N and RTMDet-tiny backbones | Apache-2.0 (PaddleClas HGNetV2 `stage1`; OpenMMLab CSPNeXt `imagenet_600e`) | ImageNet-1k (PaddleClas's SSLD stage 1 may also use unlabelled ImageNet-22k). The images are under ImageNet's terms of access: non-commercial research and education. | Whether weights descended from an ImageNet-pretrained backbone carry those terms. The same ancestry is nearly universal among permissive detectors. **Accepted with COCO (Q4).** |
| PARSeq, pretrained | Apache-2.0 | Synthetic text (MJSynth, SynthText) plus real scene-text sets with mixed, often research-only terms | Unclear provenance, which is why J2 lists a variant trained on SoccerTrack v2 |
| Digit model trained by us | ours | SoccerTrack v2 jersey crops (CC BY 4.0, which needs an attribution in `packaging/copyright` and the model card) plus digits we render ourselves | None beyond the attribution |
| `ocrs` | MIT/Apache | HierText (CC BY-SA 4.0) | Whether share-alike reaches the weights. Its obligations, if any, are compatible with AGPL, but it has to be checked |
| No audio model | – | – | The whistle and cheer detectors are DSP. YAMNet (Apache-2.0, trained on AudioSet: labels CC BY 4.0, YouTube audio under its uploaders' rights) is deferred until DSP proves too weak |
| No single-object tracker | – | – | Avoided: LaSOT, GOT-10k and TrackingNet provenance (T2) |

**L3. Choosing the runtime is P3's call, by a rule stated now.**

- **The candidates:**
  - `rten`: pure Rust with AVX2, and no native dependency or build-time download.
  - `ort` (CPU execution provider): the fastest mature option. It is built with `load-dynamic`, never its default `download-binaries`, which fetches ONNX Runtime from pyke's CDN in the build script. The `.deb` then bundles a `libonnxruntime.so` (about 20 MB, MIT) pinned by version and sha256, with a `packaging/copyright` stanza and a smoke-test check.
- **The rule:**
  - **The build never downloads.** CI, the release and a build from source fetch crates and nothing else, as whisper.cpp is vendored rather than fetched. `rten` meets this as it stands, while `ort` needs the packaging above, and that counts in its cost.
  - **Pick `rten` if** its latency on D-FINE-N at the chosen input is within 1.5× of `ort`'s and meets G4's throughput bars.
  - **Otherwise pick `ort`.**
  - **Never GPU:** the Gen9 iGPU isn't faster than the CPU here, and it would need `intel-opencl-icd` on a frozen driver branch [spike §1a].
  - **Never GStreamer inference elements:** none is packaged for 1.24.2 [spike §2].
- **The runtime is an unconditional dependency of `video-coach-media`,** like `whisper-rs`, with no feature gate. The decision is the same, and so is the reason: one build.

**L4. Models download on first use** through Phase 11's downloader:

- pinned URL, per-model sha256, `.part` then rename;
- stored in `$XDG_CACHE_HOME/coach-cuts/models/`;
- `fetch` is the permission, as for whisper;
- no test reaches the network (`fixtures::serve`).

**Where the files are hosted:** the published checkpoints are PyTorch files, so we export them to ONNX (a documented script under `tools/`, out of the Rust build) and host them as a release asset of this repository, pinned by hash. **Why download rather than bundle:** it is one mechanism for every model. At 4–40 MB, bundling would also be viable (spike §1c), and that remains the fallback if hosting becomes a burden. The button carries the prompt, as Transcribe's does: "Download 15 MB and find goals".

**With no model, analysis doesn't run a reduced pass.** Once P5 ships, the detector is part of the analysis: the job downloads it first, and a failed download fails the job and drops the queue behind it, which is Phase 11's rule for whisper. Falling back to the sound-and-motion stages would show tiers whose bars were accepted with the formation check behind them. Until P5 ships, and always if it is skipped, analysis has no model and nothing downloads.

### G. Ground truth, scoring and acceptance

**G1. The user tags whole matches in the app itself, one project per match.** `V` on the whistles that start and end each period, and `Z` or `X` on the frame the ball crosses the line (not the celebration). This happens **before** any detector has run on that match, so no suggestion can anchor the tags.

- **2–3 matches.** The four halves already on hand are two matches.
- **One project per match, with its scoreboard configured.** The scoreboard assigns start/stops by position and caps them at two per period (`start_stops_at_cap`), so two matches in one project would read as one long match. The four halves on hand are two projects of two sources each.
- **Kick-offs after goals go in a notes file, not in the project.** No event kind means "kick-off", and `V` would shift every period boundary after it. So for each goal the coach writes down the restart: the moment the ball is played from the centre spot. It goes in `kickoffs.txt` beside the local copy's `project.json` (G3), one line per restart: the source's 1-based index and the source time as `mm:ss`, with `#` for comments. Period-start kick-offs are already the `V` tags. The file stays on the user's disk and is never committed.
- **Near misses are not tagged.**
- **Wait for P0** before tagging, since tagging means moving around the match.
- **If a file starts after its half's kick-off or ends before its final whistle** (V-4), the coach leaves that tag out and adds a `# missing` line to the notes file. `auto_back_anchor_p1` already covers a missing opening kick-off in the first half.

**G2. One match tunes, and the rest accept.** Thresholds are chosen on the first tagged match (**the tuning match**) and judged only on the others (**held out**). Tuning and judging on the same 2–3 matches would pass anything. With only two matches that leaves one held out, which is thin, so the counts in G4 matter as much as the rates.

**G3. How the tags are read and scored.**

- **Reading:** the scoring tool reads each tagged project read-only with `store::read`, from the local folders listed in `COACH_GROUND_TRUTH` (separated by `:`, with the **first as the tuning match** and the rest held out, per G2), plus each folder's `kickoffs.txt`. It runs the **same `Analyzer` the app runs** on each source. It never writes a project.
- **The tool is an `#[ignore]`d harness test,** `video-coach-harness/tests/ground_truth.rs`, like `real_footage.rs`:

  ```bash
  COACH_GROUND_TRUTH=/local/match-a:/local/match-b cargo test -p video-coach-harness \
    --test ground_truth -- --ignored --nocapture --test-threads=1
  ```

  It decodes whole halves, so it runs on a local copy of each project folder, never on a network mount.
- **The scoring is pure,** in the harness's library (`video-coach-harness/src/score.rs`), not in core, because only the ground-truth test calls it: `score(truth, suggestions) -> ScoreReport`, pairing each truth with at most one suggestion: a goal with the suggestion whose window holds it (one run's windows are disjoint, D4), and a period event greedily by nearest time.

  | Event | A suggestion matches a truth when |
  |---|---|
  | Goal | The truth lies in the suggestion's `window` |
  | Seek (high tier) | The truth lies in `[seek, seek + 20 s]`, reported for matched goals. It decides whether a high-tier Seek uses `at` or the window start (D6). |
  | Period start or end | `\|seconds − truth\| ≤ 10 s` |

- **Output, per event kind and per tier:** true positives, false positives, false negatives, precision and recall.
- **Stage diagnostics,** so a miss can be traced to a stage:
  - for each truth goal: the truth restart from `kickoffs.txt`, and whether a still interval, a whistle, a restart and a cheer were found around it;
  - for each truth kick-off: detected or missed, and the detected `K`'s error;
  - for each matched goal, its distance from the seek point (how long the coach watches before the goal);
  - every near miss the rule discarded.
- **Nothing identifying is committed.** The tool prints to the terminal. Only aggregate numbers go into the spike doc (G5), with no team names, file names or shirt numbers, because the repository is public.

**G4. The acceptance bars, on the held-out matches.** A detector that misses its bar doesn't ship. Its numbers are recorded in the spike, and the phase is re-planned.

| Detector | Bar | Why |
|---|---|---|
| **Goals, all tiers** | Recall ≥ 90%, and **no held-out match missing more than one goal**. Precision ≥ 70%. | A missed goal is a wrong scoreboard the coach only notices by scanning, which is the job this feature removes. A false one costs a Dismiss. |
| **Goals, high tier** | Precision ≥ 90% | The rows the coach will learn to trust |
| **Goals, quiet tier** | Precision ≥ 40%, or the tier is hidden | The user's rule makes it lower confidence. Below 40% it's noise. |
| **Seek** | 90% of matched high-tier goals lie within 20 s after their Seek point | Otherwise high-tier Seek goes to the window start, like the quiet tier |
| **Periods** | Recall ≥ 90%, precision ≥ 80% | Four or so per match. They are cheap to confirm, and costly to miss because the clock needs them. |
| **Tracking (P6)** | The tracker is given only each range's first and last hand key. The interpolated box's centre (after T2's thinning) lies inside the coach's box on ≥ 90% of the **interior** hand keys (about 10 per range at P2's ~1 s spacing), over ≥ 10 hand-keyed 10 s ranges. At most one identity switch per 30 s, where a switch is a run of ≥ 2 consecutive missed interior keys. | Truth is P2's hand keys the tracker never saw. It is below the bar that the coach stops correcting. |
| **Jersey (P7)** | When it fills a label, it's right ≥ 95% of the time, and it fills ≥ 30% of tracks | An auto-fill that is wrong is worse than an empty field |
| **Throughput** | Analysis ≤ 5 min per 27 min half (≥ 5× realtime), on AC. Tracking ≥ 0.5× realtime. Cancel ≤ 0.5 s. A cold snap ≤ 1 s. | The coach waits on these |
| **UI** | Scanning holds full frame rate during an analysis | Recording and scanning come first |

**G5. Phase 3 (measure): build the analysis backend and measure it before any suggestion reaches the UI.** Phase 3 builds everything D1–D4 describe below the bus: media's audio and motion passes and `Analyzer`; core's `whistles`, `cheers`, `still_intervals`, kick-off pattern and confirmation rule; and the scoring tool (G3). It also builds the inference runtime and detector needed for V-2 and V-7. It stores nothing, adds no command and draws nothing. It ends with a spike doc, `docs/superpowers/spikes/<date>-match-vision-measurements.md`, holding aggregate numbers only. It answers:

1. **V-1:** the whistle, cheer and still-interval thresholds that the tuning match supports, and their precision and recall on the held-out matches.
2. **V-2:** at each truth kick-off (`kickoffs.txt` and the period-start tags), the detector's person boxes on the frame 2 s before `K`: how many fall on each side of the frame's vertical centre line, and whether the halfway line is in shot. This is the spike's biggest open question: does the virtual camera frame the kick-off? Kit clustering is not needed to answer it. It also sets `MIN_PER_KIT` (D5).
3. **V-3:** walk-back durations (each truth goal to its restart in `kickoffs.txt`), which set D4's `W`.
4. **V-4:** does each Trace file contain its half's opening kick-off and final whistle?
5. **V-5:** whistle audibility from the mast microphone, and whether an adjacent pitch's whistles show up, and the whistle's recall at truth kick-offs, which decides whether D3 requires it.
6. **V-6:** the motion pass's wall time per half.
7. **V-7:** detector latency, `rten` against `ort`, at 640 and 960 input (players 80–180 px tall shrink to about 27–60 px at 640), at B3's 4 threads, pinned, and on AC, over minutes rather than seconds (a 15 W chip throttles: Phase 10 S0's discipline). Also far-side person recall on 20 hand-checked kick-off frames, how often the stitch-seam ghost appears, and a cold snap end to end (T1).
8. **V-8:** whether the raw thumbnail difference separates walk-backs from play on the virtual camera, or needs global motion removed first.

---

## Crate responsibilities

| Crate | Contents |
|---|---|
| `video-coach-core` | The format bumps (F3): the new types, `MIN_READABLE_FORMAT_VERSION`, remapping on source edits, the mutators. `PlanEntry.clip_id: Option`, `ExportTarget::Reel` and the reel's plan. The chapter list. `PlayerHighlight`, `NormRect`, interpolation, `highlights_at`, the tracked-key thinning. The signals (whistles, cheers, still intervals), kick-off patterns, the confirmation rule, kit clustering, the formation test, and the single-track Kalman tracker and its gate. No new dependency: the core audit still lists exactly `serde`, `serde_json`, `thiserror` and `uuid`. |
| `video-coach-media` | The motion sampler and the crop sampler (GL scale and crop, then `gldownload`). Torso colour sampling. The inference runtime and the model table. `Analyzer` and `Tracker`, and the one-`Finished` job helper (B1). Highlights in `overlay.rs`. `ExportJob`'s optional per-entry media. `chapters.rs` (the `chpl` splice). |
| `video-coach-app` | Bus: the commands (tag and trim a reel, set a highlight key, edit, delete, track, analyse, dismiss or restore a suggestion), the heavy-job scheduler (B2), `EditHighlights` undo and its purge. UI: scrubber markers, `[` and `]`, the H tool, the highlight inspector, the suggestion rows, the reel row in the export sheet. |
| `video-coach-harness` | A reel export end to end, highlights reaching an export, preemption by recording and by tracking, a track waiting for transcription, and a panicking job. The P0 round trip in `real_footage.rs` and `ground_truth.rs` (both `#[ignore]`d), and the scorer (`score.rs`) it uses. |

`video-coach-core` touches no pixel and no model. Media hands it numbers: motion values, whistle samples, torso colours and person boxes. So everything that decides is tested on CI with synthetic input.

## Testing

- **Core:**
  - **Format:** a v7 file loads under the current version, a v6 file is still `LegacyProject`, and current + 1 is `TooNew`. Move and remove remap highlights and suggestions, and a highlight blocks removing its source.
  - **Reel plan:** lead-in and tail, the clamps at the source edges and at the previous goal, per-goal trims, match order, and the entry text with and without a scoreboard state.
  - **Chapters:** start times come from `start_frame`, and a plan of fewer than two entries has none.
  - **Highlights:** interpolation, the single-key span, and a box placed through `Zoom::source_point` under zoom drawing back onto the same picture pixels through `Zoom::transform`.
  - **Whistles:** synthetic tones in noise, the 150 ms floor, and the long and short split.
  - **Cheers and still intervals:** synthetic series.
  - **The confirmation rule:** all three cases, period start and end, and the 10 s de-duplication on re-run.
  - **Resolving:** an event resolves the earliest suggestion that holds it, and undo un-resolves it.
  - **Formation:** synthetic boxes, with outliers, a slanted split, and too few players.
  - **Tracker:** synthetic box sequences with crossings and a frame jump; thinning keeps the ends and every key interpolation misses by more than the tolerance, and never drops a coach key.
- **Media:**
  - The `chpl` splice on a real `Exporter` output (the encoder CI selects, with `avenc_aac` audio), read back by `ffprobe -show_chapters` with every frame still decoding; the no-room path; a file with no `udta`.
  - An entry with no media exports with game audio only, and a filler PiP.
  - Highlight pixels land inside the picture rect under zoom. These are properties, not golden images, as in Phase 9.
  - The motion sampler's rate and cancel on a fixture.
  - `Analyzer` and `Tracker` cancel within 0.5 s.
  - **A test detector kind,** like `TranscribeKind::Test`, returns canned boxes, so the queue and the formation path run on CI with no model.
- **Harness:**
  - A reel export end to end.
  - Recording preempts an analysis, which resumes afterwards.
  - A track preempts an analysis, which restarts afterwards.
  - A track waits for a running transcription, then runs before anything queued behind it.
  - A job that panics fails, and the queue moves on (a panicking test kind).
  - A stale track result is dropped.
  - **Scoring:** hand-built truth and suggestion sets.
  - `ground_truth.rs` and the P0 round trip in `real_footage.rs`, ignored.
- **Manual (batched):**
  - A real reel checked in VLC for chapters.
  - A highlight followed through scan, a recording and an export.
  - One full "Find goals" on a real half, timed.

## Risks

1. **The detectors may not clear their bars on this footage.** That is what P3 and G4 exist to find out, *before* P4 and P5 put anything in front of the coach. The fallback is honest: the tiers or kinds that fail stay hidden, and the manual Z / X / V path is untouched.
2. **The virtual camera may not frame kick-offs** (V-2). If it often doesn't, P5's formation check can't raise precision, and sound and motion carry the goal detector alone.
3. **Stoppages that look like kick-offs** (injuries, water breaks, an adjacent pitch's whistle). These are handled by the cheer tier, then by P5 if the bars need it.
4. **Tracker identity switches** in same-kit crowds. Handled by T3's correction model, which is the feature's real core, not its edge.
5. **The first version bump** (F1). If the guard change lands late, every v7 project breaks. It goes first in P1, with its test.
6. **Throughput on a 15 W chip under sustained load.** Measured over minutes in V-7, not seconds.
7. **Licence drift at pin time:** re-verify each model's licence and hash when it is pinned (L2), including the backbone's pretraining checkpoint, not only the detector's. The user has accepted COCO- and ImageNet-descended Apache-2.0 weights (Q4), so the check is that each pinned checkpoint is still Apache-2.0 and still descends only from those datasets; an Objects365 or other unreviewed ancestry goes back to the user.

## Deferred

- **Goals inside a clip export as chapters** (C2). A goal chapter would split its clip's chapter. Q6 asks.
- **Previewing a reel entry in the app.** "Reel starts here" is set from the scan picture, which is the footage itself.
- **A reel of one team's goals.** The user asked for every goal in one video. Revisit on request; it is `Reel` gaining a filter.
- **An automatic guess at the move's start** (the last stoppage, or a change of possession) **and assist labels ("assist #7").** The 30 s default and the coach's trims already meet the build-up requirement, and a guess that can come out *shorter* is the one way to cut an assist out silently. Stoppage times are also not stored (D6), so a guess needs either a stored signal list or a re-analysis. Change of possession and assists need ball tracking, which the ball's few pixels in the wide framing make doubtful, and jersey numbers. **Revisit** only with an asymmetric bar: the guess starts at or before the coach's own trimmed start on ≥ 95% of goals, measured against trims collected on tagged matches. Otherwise it may only ever lengthen the default.
- **Which side scored, from the kit that kicks off** (D5). The team that kicks off conceded, so the formation check could label a goal row "Home goal?". Dropped by the user (Q9): the coach confirms with Z or X while watching the goal, so the label saves no keystroke, and it would cost a stored kit colour, a format bump, a kit-to-side mapping, a scoring row and a bar. Revisit if a confirm path appears that doesn't involve watching the goal.
- **Detecting the painted halfway line** (D5). Revisit if the kits' separation proves ambiguous.
- **A kit-colour tie-break in the tracker** (T2). Same-kit crossings, the common switch, can't use it. Revisit if G4's tracking bar fails on opposite-kit identity switches.
- **Kick-offs as reel entries** (the user listed them as optional). Revisit on request.
- **Handheld phone and broadcast footage:** score-bug OCR (spike §4d), the tuning of the audio-only pattern, and the YAMNet audio tagger.
- **QuickTime `chap` tracks and a YouTube chapter text** (Q5).
- **Hiding one highlight in one clip** (Q2).
- **Sportscode XML import and export.** It is the de facto interchange format, but the user has no source for it [footage].
- **Fine-tuning the detector on SoccerTrack v2.** Only if V-7's recall demands it.
- **iGPU inference, live tracking under the pen, and GStreamer inference elements** (spike §6).

---

## Open questions for the user

The questions keep their numbers, so earlier references stay valid. Each open one has a default, and **the plan proceeds on it unless the user says otherwise.**

- **Q1. Which matches are the ground truth?** The four halves on hand are two matches, and a third makes the held-out set (G2) meaningful. And can the tagged projects be copied to local disk for the scoring runs? Those runs decode every half in full. And can you note each post-goal restart time in a text file as you tag (G1)?
  **Default (the plan proceeds on this unless the user says otherwise):** the four halves on hand, as two projects, with a third match added if one is recorded; each copied to local disk for scoring, with a `kickoffs.txt` beside it (G1, G3).
- **Q2. Should a highlight show in every clip that covers its moment** (H1), or should a clip be able to hide one?
  **Default (the plan proceeds on this unless the user says otherwise):** it shows in every clip that covers its moment. Hiding one per clip stays in Deferred.
- **Q3. Is the default reel cut right,** 30 s before each goal and 6 s after? It is the only automatic cut; the coach trims each goal from there.
  **Default (the plan proceeds on this unless the user says otherwise):** 30 s and 6 s (R2).
- **Q5. Where do the reel and the exports get watched?** VLC and mpv show `chpl` chapters. iPhones need a QuickTime chapter track, which is a real piece of work. YouTube needs timestamps in the description.
  **Default (the plan proceeds on this unless the user says otherwise):** `chpl` only (C3, C4). QuickTime `chap` tracks and YouTube chapter text stay in Deferred.
- **Q6. Match events as chapters in exported videos, and a whole-match export.** You asked for match events as chapters in exported videos. As specced, the goals reel gets a chapter per goal and a clip export gets a chapter per clip; periods (kick-off, half-time) never become chapters in a file (C2). Is that enough, or do you want:
  - **(a) a whole-match export** (the full game, no commentary, the scoreboard, and chapters at every goal and period) for sharing with parents? It is cheap once the reel exists (one entry per period), but it is a new kind of output.
  - **(b) a chapter at each goal *inside* a clip export**, which would split that clip's chapter in two and title the rest of the clip with the goal?

  **Recommendation, and the default (the plan proceeds on this unless the user says otherwise):** the reel plus per-clip chapters now. Yes to (a) if parents or players watch whole matches, since that export is where match-event chapters fit naturally. No to (b). Until the user says yes to (a), it stays in Deferred.
  **What changes:** as specced, nothing. (a) adds an `ExportTarget` whose plan has one entry per period and a chapter list built from match events, not plan entries. (b) changes C2's chapter list to interleave goal chapters within clip chapters.

## Decided (the user's answers, 2026-09-22)

- **Q4. COCO- and ImageNet-descended Apache-2.0 detector weights: acceptable.** Recorded in L2, whose per-model table is the record of the decision, and in Risk 7.
- **Q7. The reel holds confirmed goals only,** and its export row says "N suggested goals not confirmed" while goal suggestions are pending (R2).
- **Q8. Highlights may be placed outside a recording,** while scanning, and are saved with the footage. Pen drawings stay recording-only (Scope, H1, H3).
- **Fast scanning (S), 2026-09-22:** scanning only, never while recording; `L` faster and `J` slower, forward only, sound muted above 1×; a speed button too.
- **Q6 (a). Yes to a whole-match export** with chapters at goals and periods (W), asked for while tagging the first match. (b), goal chapters inside a clip's export, stays deferred.
- **Q9. No scoring-side detection from the kicking kit.** P5 is a formation check that only drops false kick-offs, is built only if P3 shows the precision bars need it, and stores nothing (D5). The side is in Deferred.
