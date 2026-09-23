# The lossless whole match: a copied file with the scoreboard on its own track

**Date:** 2026-09-23
**Status:** Reviewed (simplify + correctness applied). The user's decisions stand: **the three-entry picker** (Default / Burned in / Separate track, where Default is per target — the whole match copied with a track, clips and reels burned), and **the mode is remembered in `Preferences`**, which is the v11 bump (M2).
**Builds on:** match vision spec W (the whole-match export, shipped), spec C (chapters, `chapters::splice`), Phase 9 (`ScoreboardContext`, the match clock), Phase 8 (the export run, `.part` and rename, spec E5/E6/E8).
**Evidence:** measurements taken on this machine on 2026-09-23 against the user's own two-file match. The footage's *properties* are quoted; its teams are not — the repository is public.

Labels, as in the match vision spec: **[measured]** was measured here, **[cited]** comes from a named source, **[estimate]** is arithmetic.

---

## Goal

The whole-match export re-encodes both halves to burn a scoreboard into the picture. The user's 56-minute match came out at **7.9 GB (~19 Mbit/s) from ~5 Mbit/s sources, in about an hour** [user]. The coach asked for the other trade: *"is there a way to not re-encode it and use different tracks to do the scoreboard" / "so it is lossless with overlay"*.

So: **copy the video and audio, join the halves, and carry the scoreboard as a timed-text file the player loads beside it.** Chapters stay as they are.

**[measured]** The same match, copied: **2.10 GB in 26.6 s** (9.8 s with the files in page cache), every video packet byte-identical but one, every audio packet byte-identical. That is 1/3.8 of the size and about 1/130 of the time.

## Scope and product rules

The user's decisions. This spec follows them.

- **Lossless stream copy for the whole match.** No re-encode on that path.
- **The scoreboard is a player-drawn track,** switchable off by the viewer.
- **Clips and reels keep burning their overlay in** by default. They re-encode anyway.
- **Player highlights and pen drawings are not carried.** They are pixels. See **N**.
- **The scoreboard's carriage is an option on the export,** not a property of one target (the user, 2026-09-23: *"maybe it is an option for scoreboard to be subtitle track instead of burned"*). See **M**.

## The footage this is designed for [measured]

Both halves of the user's match, read with `ffprobe` and a box dump. The two files agree on everything that matters:

| | Half 1 | Half 2 |
|---|---|---|
| Video | H.264 **High, level 4.0**, 1920×1080, `yuv420p`, `avc1` | same |
| Video `stsd` (incl. `avcC` SPS/PPS) | 183 bytes | **byte-identical** |
| Media timescale | 90000 | 90000 |
| Audio | AAC-LC, 48 kHz, stereo, `mp4a` | same |
| Audio `stsd` (incl. `esds`/ASC `11 90`) | 91 bytes | **byte-identical** |
| Duration / frames | 1623.3955 s / 48,697 | 1633.0632 s / 48,987 |
| Rate | 5.03 Mbit/s | 5.05 Mbit/s |

- **The "irregular ~29.997 fps" is not jitter.** `stts` runs 99 samples of 3000 ticks then one of 3030 (at 90000), repeating: 29.997 fps on average, every delta an exact multiple of 1/3000 s.
- **`ctts` has one entry with offset 0** — no composition reordering to carry.
- **The edit list is harmless here:** one version-0 `elst` entry, `media_time = 0`, rate 1.0. There is no PTS offset to undo. (CLAUDE.md's stream-time rule still holds everywhere else; it just isn't what bites at the join.)
- **Each file is an HLS remux:** 1626 and 1636 top-level boxes, one `mdat` per segment, `moov` at the front.

---

## Decisions

### L. The lossless join

**L1. One GStreamer graph, stream copy end to end.** Per entry: `filesrc ! qtdemux`, then `queue ! h264parse` into a shared `concat`, and `queue ! aacparse` into a second `concat`. The two `concat`s feed one `mp4mux ! filesink`. There is no third pad: the scoreboard is a sidecar file, not a muxed track (**T**).

- **`queue` after every demux pad and before every mux pad is not optional** [measured]: without them the graph deadlocks on the first file, because `qtdemux`'s single streaming thread pushes video into an aggregator that is waiting for that same thread's audio.
- **`concat` with `adjust-base=true`** (its default) makes each source's segment start where the previous one ended, so the muxer sees one continuous timeline.
- **Nothing decodes and nothing touches the GPU.** The copy path needs no `Gl`, no display and no encoder, so it runs where CI runs.

**L1b. The copy iterates the plan's entries, in entry order** — `job.compilation.plan.entries`, taking each entry's file as `job.sources[entry.source_index]` — **never `project.source_videos`.** The whole-match plan filters out a source with no usable duration (`whole_match_entries`, `whole_match.rs:29`), so the two lists can differ, and the plan is the one the frame count, the progress denominator and the chapter times are all computed from. The copy is only ever chosen for `ExportTarget::WholeMatch`, whose entries are one whole `Play` segment over `[0, duration]`; that is what makes a stream copy a faithful rendering of the plan, and it is stated here because nothing in the graph could notice a trimmed entry.

**L2. It is lossless, and that is measured, not assumed.**

- 97,684 output video packets = 48,697 + 48,987 exactly. 152,642 audio packets = 76,094 + 76,548 exactly. No frame is lost, added or duplicated at the join [measured].
- Every video packet's byte length is identical to its source's **except one**: the second half's first keyframe grows by **34 bytes**, which is `h264parse` writing the parameter sets in-band at the resync point. Every audio packet is identical throughout [measured].
- Output duration 3256.459 s against an expected 3256.4587 s [measured].
- Output: 2,103,220,259 B, against 2,103,085,696 B of input. The container overhead of joining two files is **+134 KB**.

**L3. Pin the track timescales.** Set `trak-timescale=90000` on the video pad and `trak-timescale=<sample rate>` on the audio pad.

- Left automatic, `mp4mux` chose 1/3000 for this footage. That happens to represent every source delta exactly (3030/90000 = 101/3000), so the measured round trip is exact — but only by luck. A timescale that does not divide the source's rounds every sample duration, and the error does not have to cancel.
- 90000 is the source's own timescale and the conventional MPEG video clock; it represents 30, 29.97, 25 and 24 fps exactly.

**L4. The `moov` goes first, in reserved space, exactly as the encoded export does.** Set `reserved-max-duration` to the summed source durations plus a tenth plus a second, the formula `composite/export.rs` already uses.

- **This is what keeps chapters working.** `chapters::splice` needs `moov` before `mdat` with a `free` box after it to shrink. **[measured]** With the reserve set, the copied match's top-level boxes are `ftyp / free / moov / free(4.72 MB) / uuid / free(8) / mdat` — the layout the splice requires, unchanged.
- `faststart` is the wrong tool: it writes the whole `mdat` to `$TMPDIR` first, which for this file is 2.1 GB of temporary I/O and a leak on a crash. The encoded path rejected it for the same reason.
- The reserve costs about **10.6 MB** of `free` on a 54-minute match (0.5%). `reserved-bytes-per-sec` defaults to 550 **per track**; the measured `moov` came to 1.22 MB against 5.4 MB reserved, so the default has ample margin and is left alone.
- **The run logs what is left of the reserve.** `mp4mux` exposes `reserved-duration-remaining`; read it at EOS and print it on the `bus: exported …` line. That is the standing check that the margin is still ample on a longer match, and it costs one property read (**E7**).

**L5. The copy writes `<path>.part` and renames, like every other export.** Nothing else in the `.part`/rename/delete contract changes. The chapter splice runs on the `.part` before the rename, as today.

**L6. The compatibility gate lives in the copy graph, from the pads' own caps.** There is no second `Discoverer` pre-pass: `probe.rs` returns only `duration_seconds` and `display_aspect` (`probe.rs:20-25`) — it never returns caps, and adding a caps-returning probe would mean opening and closing every file twice for information the graph is about to negotiate anyway. The gate reads the caps of each entry's parser src pad as they are negotiated, and it is both **absolute** and **relative**:

| Check | Rule | Why |
|---|---|---|
| **Container** | Every entry demuxes through `qtdemux`. A file it produces no usable pad from is refused. | The app accepts `mkv`, `webm`, `avi`, `mts`, `mov`, `m4v` and anything else the prober can read. Only MP4/QuickTime can be copied into `mp4mux` from `qtdemux`, and a project of Matroska sources would otherwise reach the graph and fail with a GStreamer error the coach can't act on. |
| **Video codec** (absolute) | `video/x-h264` on every entry. | `mp4mux` takes H.265, VP9 and more, but the whole path is designed and measured on H.264, and the parameter-set comparison below is `avcC`-shaped. Anything else is refused rather than half-supported. |
| **Audio codec** (absolute, when there is audio) | `audio/mpeg, mpegversion=4` (AAC) on every entry. | Same reason, and the same `esds` comparison. |
| **Video `codec_data`** (relative) | Byte-identical to the first entry's. | One `stsd` entry is written. It carries the SPS and PPS, and so also the profile, level, resolution and chroma. Comparing the bytes is one comparison instead of six, and it cannot be fooled. |
| **Audio `codec_data`** (relative) | Byte-identical to the first entry's. | Sample rate, channels and object type all live in it. |
| **Audio present-or-absent** (relative) | The same on every entry. | See **E4**. |

- **Why the absolute checks are load-bearing, not belt-and-braces.** **[measured]** Concatenating a 320×240 and a 640×480 H.264 file through this graph produced **no error and no warning**: `mp4mux` wrote one file, with one `stsd` describing every sample — and so describing most of them wrongly. Nothing downstream will refuse a mismatch for us. The gate is the only thing standing between the coach and a silently broken 2 GB file.
- **A refusal names the file, the field and the way out:** *"the second video was recorded differently from the first (its H.264 parameters differ); choose **Scoreboard: burned in** to export it re-encoded."* A container or codec refusal reads the same way: *"this project's videos aren't H.264 in MP4, so they can't be copied; choose **Scoreboard: burned in**."*
- **Why refuse rather than silently re-encode.** The coach asked for a copy that takes half a minute. Quietly spending an hour instead is the worst available answer, and the fallback is one picker away. This also keeps the code honest: there is exactly one re-encoding path, the one that already exists.
- The project's aspect gate (`Project::check_aspect`) already refuses a source whose display aspect differs from the project's, so the commonest mismatch rarely reaches here — but it compares aspect, not size, so 1280×720 and 1920×1080 pass it and this gate catches them.
- **A refusal leaves nothing behind.** It is an `ExportError::Failed` from `run`, which already deletes the `.part` (`composite/export.rs:279-284`).

**L7. What the join does *not* fix: a ~10 ms A/V drift that accumulates per source.** **[measured]** the copied match's audio runs 3256.450 s against video's 3256.459 s — 9 ms across two sources.

- **This is not AAC encoder priming.** It is each source's own video-vs-audio *track duration* delta: an MP4's two tracks rarely end on the same instant, and `concat` with `adjust-base` offsets each stream independently by the length of what came before it *on that stream*. So the audio timeline slips against the video timeline by each source's own delta, and the slips add up: two sources gave 9 ms, and n similar sources would give roughly n × 4.5 ms [estimate].
- **The bound worth stating:** it is one source's track-duration delta per source, not one AAC frame (21 ms) at the join, and it is cumulative, not fixed. At ~5 ms a source a match would need dozens of files before it reached the ~40 ms where lip sync starts to be noticed [cited, EBU R37]. The app's own measured preview offsets are 2–7 ms, so this is within the noise the coach already watches.
- **No code change.** Removing it would mean re-timestamping or re-encoding the audio, which is the thing this spec exists to avoid. It is written down so that nobody re-derives it as priming, and so that a future many-source project has a number to check against.

### T. The scoreboard track

**T1. One carrier: a sidecar `.srt` beside the `.mp4`.** Written from the cue list (**U**) for the whole match rendered in track mode, and for nothing else.

- **Why the sidecar and not an embedded track.** **[measured]** VLC 3.0.20 reads an embedded `tx3g` track and lists it (`adding track[Id 0x3] subtitle (enable)`) but creates **no** subtitle decoder for it: the viewer has to go and turn it on. The sidecar with the matching basename is auto-detected (`autodetected subtitle: …/side.srt with priority 4`) and decoded without being asked. A scoreboard the coach has to find in a menu is not a scoreboard.
- **And the reason to prefer an embedded track turned out to be false.** **[measured]** `mp4mux` writes a **zero-length sample after every cue**: 3,400 one-second cues came out as **6,799 samples**. Pushing the cues ourselves does not avoid the empty samples — the muxer inserts them either way. So the embedded track costs a mux pad, an `appsrc`, the "a requested pad that is never fed stalls the aggregator" hazard and two tests, and buys a track that players list but do not show.
- The sidecar is also the file YouTube takes as a caption upload [cited], which the embedded track is not.
- **The embedded `tx3g` track is Deferred (Deferred 1),** gated on the user's own report from their TV, phone and share. If the sidecar turns out not to travel, the cue list is already there and the track is a small addition.

**T2. What a cue looks like.** One line:

```text
Rovers 1 - 0 Athletic · 14:05
```

`"{home} {home_score} - {away_score} {away} · {clock}"`, where `{clock}` is `format_clock`'s `main` with its `trailing` appended when the clock is in stoppage (`… · 45:00 +2:13`), `HT`/`BREAK` on a break, and `FT` after the last period. The team names are the configured ones, verbatim.

- **No fitting and no truncation.** The burned board fits its labels because its cells are a fixed width (spec W3); a subtitle line is not in a cell, and a player that runs out of width wraps. A long club name is the viewer's player's problem, not ours.
- **One line, no markup.** No `{\an8}`, no `<font>`: SRT positioning is a libass extension that VLC's own decoder does not honour, and a tag a player shows literally is worse than a line in the wrong place.

**T3. Where it appears: wherever that player puts subtitles.** Bottom-centre, in the viewer's own subtitle font and size. **A coach who wants the scoreboard in the corner, in the team's colours, picks "burned in"** — that is the whole point of the option.

**T4. How often it changes, and how many cues that is.** A cue per distinct line. The clock reads whole seconds, so the line changes once a second while a period runs and not at all during a break.

- **[measured]** A 54-minute match is about 3,256 cues, which is about **194 KB** of `.srt`. Noise against 2.1 GB.
- **Cues are contiguous:** each cue ends where the next begins. That matters for the file's readability, not for the muxer, which is no longer in this path.

**T5. The cue times are presentation times, and `mp4mux` may write an `edts` on the video track.** When the first video sample's DTS is negative — which B-frames cause, and the design footage does not have (`ctts` has one entry, offset 0) — `mp4mux` writes an edit list to shift presentation back to zero. A player honours it, so the picture's timeline and the sidecar's agree. A player that *ignores* the `edts` shows the picture early by the first frame's composition offset, typically one or two frames, which no whole-second clock can show. Worth knowing; nothing to do.

**T6. The sidecar is written after the rename, and a failure does not fail the export.**

- **Its path is `job.path.with_extension("srt")`.** That carries the muxer's own name cleaning (`file_name` replaces `/` and `:`, `bus/export.rs:216-219`) and the run's `" (2)"` de-duplication (`de_duplicate`, `bus/export.rs:484`) for free, and it is the matching basename that makes a player auto-load it.
- **A stale sidecar is removed whenever a run writes none,** in every mode: a burned export, a copy with no cues, a re-export of a project whose scoreboard was deleted. Otherwise the coach exports once with a scoreboard, deletes it, exports again, and the old score plays over the new film. Writing and removing are the same step, in the same place: *the file that belongs beside this output is this string, or nothing.*
- Like the chapter splice it is reported: `ExportDone` gains `sidecar: Option<PathBuf>`, and the `bus: exported …` line logs it. An export that produced a good `.mp4` must not be thrown away because a 200 KB text file could not be written.

**T7. Which player shows it** [cited, except VLC which is measured].

| Target | Sidecar `.srt` |
|---|---|
| VLC (desktop) | **Auto-loaded and shown** [measured] |
| mpv | Auto-loaded (`--sub-auto=exact` is the default) |
| Phone — VLC / ExoPlayer-based players | Shown if it is copied alongside |
| Phone — iOS Photos, AirDrop, Google Photos | Not copied |
| TV — USB stick or DLNA | Often ignored |
| YouTube | **This is the file you upload as captions** |

The honest summary for the user: **on a computer it just works; on a phone or a TV it is a coin toss.** The burned option is there for the audience that can't — and the user's own report (**the hands-on step**) decides whether the embedded track is ever worth building.

### U. Where the cue text comes from

**U1. One pure function in `video-coach-core`, over the compilation's frames.**

```rust
pub struct Cue { pub start: f64, pub end: f64, pub text: String }

pub fn scoreboard_cues(compilation: &Compilation, scoreboard: &ScoreboardContext) -> Vec<Cue>;
```

For each output frame `n`, take `plan.entries[frames[n].entry].source_index` and `frames[n].source_time`, call `ScoreboardContext::state_at`, render **T2**'s line, and run-length-encode the result. A run from frame `a` to frame `b` becomes a cue from `a / OUTPUT_FPS` to `(b + 1) / OUTPUT_FPS`. Frames where `state_at` is `None` produce no cue — a gap, which is right: before kick-off there is no match to show.

- **`state_at(source_index, source_time)` per frame is the rule, not a shortcut around it** (CLAUDE.md, Phase 9). A whole-match entry has one source and plays it straight through, but the same function serves an entry that freezes — and there the run-length encoding is what makes the clock hold through the pause instead of running on. `core`'s existing pause test covers the same invariant for the burned board.
- **No media dependency, no new type but `Cue`.** It reads `Compilation`, which is already core's.
- **Cost:** about 97,700 `state_at` calls for a 54-minute match — exactly the number the burned export already makes, and there it is dwarfed by encoding. Here it is the one piece of real work on a 26 s copy, and it is still a fraction of it [estimate].

**U2. Output time is the plan's time.** The cue times come from frame indices at `OUTPUT_FPS`, never from a sum of source durations (CLAUDE.md, Phase 8).

- The copy's real timeline and the plan's differ by the plan's per-entry quantization: entry 2 starts at `ceil(1623.3955 × 30)/30 = 1623.400 s` in the plan against 1623.3955 s in the file — **4.5 ms**, bounded at one frame per source [measured/estimate].
- A clock that reads whole seconds cannot show a 4.5 ms error. Nothing is corrected for it.

**U3. `cues_to_srt(&[Cue]) -> String` is pure and lives beside it.** `HH:MM:SS,mmm --> HH:MM:SS,mmm`, blank-line separated, `\n` endings, numbered from 1. Media writes the string to the sidecar; nothing about SRT formatting lives in media.

**U4. Nothing here is cached across a source edit.** `ScoreboardContext` is built once per run by the bus and frozen, exactly as it is today (Phase 9: a relink can change a duration and so every later offset). The cue list is derived from it inside the job.

### M. The option: burned, or on a track

**M1. One choice, in the export sheet, beside Resolution and Quality.**

```rust
pub enum ScoreboardMode { Burned, Track }
```

The sheet's third picker, labelled **"Scoreboard"**, with three entries:

| Entry | Meaning |
|---|---|
| **Default** | Per target: `Track` for the whole match, `Burned` for everything else. |
| **Burned into the picture** | `Burned` for every ticked target. The whole match re-encodes, as it does today. |
| **Separate track** | `Track` for every ticked target. Clips and reels still re-encode — for the drawings, the inset, the zoom and the text bar — but the scoreboard is not painted into those pixels, and they get no sidecar (**T1**). |

- **Under the picker, when "Separate track" is chosen,** one line, which is the only place this trade is explained: *"Copied, not re-encoded; highlights and drawings can't ride a copy."*
- **Why three and not two.** The user's rule is a *per-target* default (track for the whole match, burned for a clip), and two values cannot express "I have not chosen" separately from "I chose burned". The per-target rule is one pure function, `default_scoreboard_mode(&ExportTarget) -> ScoreboardMode`, stated once and read by the sheet and the job builder alike.
- **The picker's value is stored as `Option<ScoreboardMode>`** — `None` is Default.

**M2. It is remembered in `Preferences`, and that is a format bump to v11.**

```rust
pub last_export_scoreboard: Option<ScoreboardMode>,
```

- **The field takes no attribute.** `Preferences` carries `#[serde(default)]` **on the container** (`project.rs:73-75`), so a key missing from an older file is filled from the hand-written `Default` impl — one copy of the defaults, not two. `project.rs`'s module comment says field-level `#[serde(default)]` is the hazard and that `Preferences` deliberately avoids it; adding one here would contradict the file it sits in.
- **Why `Preferences` and not `state.json`.** The other two export pickers (`last_export_resolution`, `last_export_quality`) are already there, and they are there because the choice belongs to *this match*, not to the machine. Splitting the three pickers across two stores to dodge a version number would be the worse design. The whisper model is in `state.json` for the opposite reason — it is the coach's, machine-wide.
- **What the bump actually costs, honestly.** The change is additive and needs no migration code, but `CURRENT_FORMAT_VERSION = 11` is not free: `store::read` refuses anything **above** the build's current version as `TooNew` (`store.rs:21`), so the first save by this build makes the project unopenable by 0.3.x. The way back is the one-time backup `store::write` already keeps: **`project.json.v10` beside it** — put it back as `project.json` and the older build opens it, losing what changed since. `MIN_READABLE_FORMAT_VERSION` stays 7, so every project the user has still opens here.

**M3. The mode reaches media through the job it already has: no new `ExportJob` field.**

- **In `Track` mode the bus sets `job.scoreboard = None`.** Media reads `job.scoreboard` in exactly one place — the per-frame overlay state (`composite/export.rs:381-384`, and the preview's own at `preview.rs:443`) — so `None` is precisely "don't draw the board", which is already a state the code handles (a project with no scoreboard configured). No `scoreboard_mode` field, no `if mode == Burned` inside the drawing loop, no third state to keep consistent.
- **This does not cost the chapters or the entry text.** The whole match's chapter titles come from `CompilationPlan::chapters`, built in core from the project's own events (`whole_match_chapters`), and a reel entry's text bar comes from the plan entry's `text`, built in core too. Neither reads `ExportJob::scoreboard`. (An earlier draft of this spec claimed they did; they do not.)
- **Two new fields, and neither is the mode.**
  - `job.cues: Vec<Cue>` — the payload. The bus computes the cue list from the `ScoreboardContext` it already builds, *before* it blanks `job.scoreboard`, and hands media the finished list. Empty means no sidecar.
  - `job.render: Render { Encode, Copy }` — **which renderer**, which `run` branches on once (**X1**). It is not `ScoreboardMode`: `Burned` and `Track` both `Encode` for every target but the whole match, and the mode still reaches the picture through `scoreboard` alone. The job stays a complete description of one output — nothing about what to produce is left in the caller's head — and media never learns that a picker exists.

  The bus sets `Render::Copy` only for `WholeMatch` in `Track` mode; `default_scoreboard_mode` decides what `Default` means. That is the whole of the mapping, in one function in `bus/export.rs`.

**M4. The preview always draws the scoreboard.** The mode is an export choice. The preview is where the coach checks the edit, and a preview that hid the board to match an export setting would just be a worse preview.

**M5. The rows do not change when the picker does.** See **X2**: the row says what it counts and how long it runs, and neither depends on the mode. The one sentence that does depend on the mode lives under the picker, where the choice is (**M1**), so changing the picker changes exactly one line of text and needs no re-list.

### X. The export target, the run and the sheet

**X1. `ExportTarget::WholeMatch` stays; it gains a second renderer.** No new target, no new row, no second progress model.

- `ExportTarget::WholeMatch` + `ScoreboardMode::Track` → the copy (**L**).
- `ExportTarget::WholeMatch` + `ScoreboardMode::Burned` → today's encoded path, unchanged.
- Every other target re-encodes in both modes.
- The choice is made in `composite::export::run`, which branches once, at the top, on `job.render` (**M3**) into `copy::copy` or the existing `export`. They share `part_path`, the rename, the delete-on-failure, the chapter splice and the sidecar write — so the `.part` contract has one implementation, not two.

**X2. `ExportTargetRow` keeps `count`, `unit` and `seconds`.** The row reads "2 videos · 54:16" in both modes, as it does today.

- A `detail: String` carrying "· 1920×1080" was considered and dropped: `SourceRef` stores `duration_seconds` and `display_aspect`, **not** width and height, so the size would need either a new stored field or a `Discoverer` probe per row, on every sheet open, for a number the coach cannot act on.
- **"2 videos" stays.** It was written as the honest warning that this is the longest render there is (spec W4), and in burned mode it still is. In track mode it is merely true rather than pointed, which is a price worth paying for not rebuilding the row model.
- The one thing the coach genuinely needs to know — that a copy carries no drawings — is one sentence under the picker (**M1**), stated once, where the choice is made.

**X3. `ExportMessage::Progress` keeps its meaning: output frames of the plan.**

- A pad probe on the video branch reads each buffer's running time and reports `round(seconds × OUTPUT_FPS)`, clamped to `plan.total_frames()`, throttled to whole-percent changes as today. EOS reports the total.
- **Why not bytes.** The run's model is frames across targets, its rate window is frames per wall second, and "Finishes at …" divides remaining frames by that rate (spec E5). Reporting the copy in the same unit means the sheet, the rate window and the estimate all keep working with no change — a run that copies the match and then renders three clips still has one honest number.
- **The rate window is cleared when a target finishes.** A copy runs at ~3,600 output frames a wall second against an encode's ~20. `RateWindow` keeps a 5-second window of samples (`core/src/export.rs:65-92`), and `Active` keeps one for the whole run (`bus/export.rs:235`), so without this the three clips queued behind a copy inherit its rate and the sheet promises they will finish almost immediately, then walks the estimate back for the next five seconds. One line in `Active::finish_target`: `self.rate = RateWindow::default()`. The sheet already shows no estimate until the window is wide enough, so the gap between targets is silent rather than wrong.

**X4. Cancel is unchanged.** The copy polls the same `AtomicBool`, stops the pipeline, and `run` deletes the `.part`. Cancelling a copy that has already finished still reports it done, as the run's contract says. A cancelled copy leaves no sidecar — the sidecar is written after the rename — and it removes no existing one either: a cancel must not delete a file the last good export wrote.

**X5. `ExportDone` reports `encoder: "copy"`** and a default `Diagnostics` (the copy selects no decoder, uploads nothing and has no GL platform). `chapters` and the new `sidecar` are reported as usual, and the remaining `moov` reserve (**L4**) joins the line. The `bus: exported …` line keeps its shape, so nothing that reads the log breaks.

### N. What a copy cannot carry

**Say it once, plainly, and put it in the sheet.** A copied whole match is the camera's own pixels. **Nothing that is drawn can be in it:**

- player highlights (the ring and the label),
- pen drawings,
- the webcam or avatar inset,
- zoom,
- the text bar,
- and the scoreboard itself, which is why it rides as a sidecar.

Of these, only **player highlights** are a loss against today's burned whole match — a whole-match entry already has no clip, so it never had drawings, an inset, zoom or a caption (spec W2).

**What the coach uses instead: a clip, or the goals reel.** Those are the exports that exist to carry the coaching on top of the footage, and they burn it in by default. The whole match is the film; the clip is the lesson.

The sheet says it in one line under the picker (**M1**): *"Copied, not re-encoded; highlights and drawings can't ride a copy."*

### E. Edge cases

**E1. One source only.** The row is present and the copy still earns its place: it adds the scoreboard sidecar, and the match's tagged chapters if there are any. `concat` with one input is a pass-through. Nothing special-cases it.

- **With nothing tagged, a single-source copy gets no chapters at all.** `whole_match_chapters` falls back to one chapter per source only for two or more entries (`whole_match.rs:83`) — a lone chapter would just repeat the file. With period or goal tags it gets those, however few, because they are real moments rather than a restatement of the file name.

**E2. Sources that differ in codec, parameters or size, or that are not H.264 in MP4.** Refused, naming the file and the way out (**L6**).

**E3. A source with no video.** Cannot happen: `probe` refuses it at add time (`ProbeError::NoVideo`).

**E4. Audio.**

- **No source has audio** → no audio pad is requested and the output has none.
- **Every source has audio, with identical `codec_data`** → copied.
- **Mixed** → refused, like any other mismatch. Splicing encoded silence in would mean running `avenc_aac`, whose ASC will not match the other file's, which is the same mismatch one layer down.

**E5. No scoreboard set up.**

- **No cue list, so no sidecar** — and any sidecar left beside that path by an earlier export is removed (**T6**). The copy runs. Inventing "Home 0 - 0 Away" would be a claim about a match nobody tagged.
- **Chapters fall back to what they already do** (`whole_match_chapters`): the tagged events in the panel's plain wording, or one chapter per source named after the file when nothing is tagged (and none at all for a single source — **E1**).
- The whole-match row is unchanged; it is not the place to teach the scoreboard.

**E6. A tagged match with no periods yet** (a goal tagged, no kick-off). `state_at` is `None` throughout, so there are no cues, as for E5. The chapters still carry the goals.

**E7. A very long match.**

- Cues: about 60 a minute → a 3-hour recording is ~11,000 cues, about 650 KB [estimate from the measured 194 KB / 3,400].
- Chapters: `MAX_CHAPTERS` is 255 and a match has tens. Untouched.
- **Above 4 GB the muxer must write `co64` rather than `stco`,** and the 64-bit offsets make the `moov` grow. The measured 2.1 GB file used `stco`; a 3-hour match at 5 Mbit/s is ~6.7 GB and crosses it. **This needs no test and no gate:** `mp4mux` switches automatically, and if the grown `moov` ever exceeded `reserved-max-duration`'s reserve the muxer posts an error, which fails the export and deletes the `.part` — a refusal, not a broken file. What the run does instead is **read `reserved-duration-remaining` at EOS and log it** (**L4**), so the margin is a number in the log rather than a guess. A 6 GB fixture to prove what the muxer already guarantees would cost minutes of CI and gigabytes of scratch for nothing.

**E8. A break in the middle of a half's file** (one file holding both halves). Nothing here cares: the cue list follows `state_at`, which follows the tagged events, and a break simply becomes one long `… · HT` cue.

---

## Crate responsibilities

| Crate | Holds |
|---|---|
| `video-coach-core` | `Cue`, `scoreboard_cues`, `cues_to_srt`, `ScoreboardMode`, `default_scoreboard_mode`, `Preferences::last_export_scoreboard`, the v11 bump. All pure; no media dependency. |
| `video-coach-media` | `composite/copy.rs`: the copy graph, the caps gate, the progress probe. `ExportJob::{cues, render}`, `ExportDone::sidecar`, the sidecar write and the stale-sidecar removal. `chapters::splice` unchanged; `overlay.rs` unchanged. |
| `video-coach-app` | The sheet's third picker and its one explanatory line, blanking `job.scoreboard` in track mode, filling `job.cues`, clearing the rate window between targets, writing the choice back to `Preferences`. |
| `video-coach-harness` | The end-to-end run over the bus: tick the whole match in track mode, get a copied file with chapters and a sidecar. |

Nothing moves between crates, and no new dependency appears in any of them.

---

## Testing

**No network, no camera, no microphone, no real footage.** The user's match is not committed; the repository is public and the footage shows children.

Four tests earn their place, plus the core table.

**Core** (`crates/video-coach-core/tests/cues.rs`): **one table test over `scoreboard_cues` and `cues_to_srt`**, with a row per case: a two-source match (kick-off, goal, half-time, second-half start, full time) asserting no cue before the first start, the score turning over on the goal's own frame, one `HT` cue over the break, `FT` after the last period and contiguity within a run; a freeze holding the clock; stoppage appending `+M:SS`; no scoreboard giving an empty list; and `cues_to_srt`'s hours, milliseconds, numbering and empty-input case.

`default_scoreboard_mode` gets no test of its own: a test that restates a two-arm `match` pins nothing that the compiler doesn't.

**Media** (`crates/video-coach-media/tests/copy.rs`), on generated fixtures:

1. **A copy of two sources is lossless, chaptered and subtitled.** Two `CounterKind::H264Mp4BFrames` fixtures (H.264 with B-frames in MP4, and an edit list — the trap this codebase already keeps a fixture for), with audio, and a cue list. Assert in one run: `decode_counters` reads `0..N` then `0..M` with nothing missing, repeated or out of order; `ffprobe` reports the inputs' `codec_name`, `profile`, `width` and `height` and a packet count equal to the sum of the inputs'; `-show_chapters` reads the plan's chapters back at the expected times (which is what proves `reserved-max-duration` was set — without it the splice skips); and the `.srt` sits beside the `.mp4` and round-trips through `cues_to_srt`.
2. **A mismatched pair refuses cleanly.** Two fixtures at different sizes → `ExportError::Failed` naming the second file, **no `.part` and no file at the target path**. (The measurement in **L6** is why this test exists: without the gate this pair produces a file, silently.)
3. **Cancel.** Cancel mid-copy on a long-enough fixture: `ExportError::Cancelled`, no `.part`, no output, and no sidecar.
4. **An empty cue list writes no sidecar, removes a stale one, and does not hang.** Put an `.srt` at the target's sidecar path first; after the run it is gone and the `.mp4` is there.

`ffprobe` is already a test-only dependency (`packaging/build-deps.txt`, match vision C3). These tests fail without it; they never skip.

**Harness** (`crates/video-coach-harness/tests/whole_match.rs`): one run over the bus in track mode, asserting the run's progress reaches `total_frames` and that the file and the sidecar land in `exports/`.

**Packaging:** `concat` joins `packaging/smoke-test.sh`'s element list, as every element the code names by hand does.

**What needs the user's eyes**, on their own match, once:

- Open the copied file in VLC on the laptop: the sidecar loads by itself and the score and clock track the play.
- Scrub across the join — no stutter, no flash, sound continuous.
- `[` / `]` and the chapter list land where they should.
- **Play it on whatever the parents actually use: the TV, a phone, a share. Report which of them shows the line.** That answer, and only that answer, decides whether the embedded `tx3g` track (Deferred 1) is ever built.

---

## Deferred

1. **The embedded `tx3g` track.** A mux pad fed from an `appsrc`, so the scoreboard travels inside the file. Deferred because VLC lists it and will not enable it, and because `mp4mux`'s zero-length sample after every cue removes the only reason to push the cues ourselves (**T1**). **Gated on the user's report** from a TV, a phone and a share. If it is built: pin `trak-timescale` on the subtitle pad (1000 is the conventional text timescale, and it makes every SRT millisecond exact) rather than leaving the muxer to pick, for the same reason **L3** pins the other two.
2. **A styled track (ASS in Matroska).** It would give a coloured box in the corner, closer to the burned board. It costs a second container, a second chapter path and a second set of player assumptions, and the coach who wants that already has "burned in". Revisit only if the user asks for a styled overlay that is still switchable.
3. **`avc3` with in-band parameter sets,** which would let two halves with *different* SPS join without re-encoding. Deferred because it is not needed for the design footage (the parameter sets are byte-identical) and `avc3` has weaker player support than `avc1`.
4. **Re-encoding only the source that doesn't match,** instead of refusing. Real but rare, and it turns a predictable half-minute into an unpredictable hour.
5. **Correcting the per-source A/V drift** (**L7**): ~5 ms a source, and the fix is re-timestamping or re-encoding the audio. Revisit only if a project with many sources is measured past ~40 ms.
6. **A sidecar for clips and reels.** One rule is tempting, but a 12-second clip already has its own text bar, and a subtitle line under it repeating the score is clutter. The whole match is the target that has no other way to show the board.
7. **Naming the track "Scoreboard".** MP4 has no track-name box that players agree to show, and a sidecar has no name at all beyond its file name.
8. **A second text track carrying the transcript** (Phase 10). The cue machinery here would take it unchanged; nobody has asked.
9. **Positioning tags in the sidecar** (`{\an8}`), which libass-based players honour and VLC's own decoder does not. Revisit if the user says the line is in the way.
10. **Greying the Resolution and Quality pickers when only a copied target is ticked.** They still apply to any other ticked target, and greying them on a tick change is more UI state than the confusion is worth.
11. **Ticking the whole match by default** now that it is the *shortest* render rather than the longest. It is still 2 GB the coach did not ask for, and the row is right there.
