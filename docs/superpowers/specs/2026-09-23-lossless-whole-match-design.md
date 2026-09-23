# The lossless whole match: a copied file with the scoreboard on its own track

**Date:** 2026-09-23
**Status:** Draft, awaiting adversarial review. The user answered Q1 and Q2 on 2026-09-23: **the three-entry picker** (Default / Burned in / Separate track, where Default is per target — the whole match copied with a track, clips and reels burned), and **the mode is remembered in `Preferences`**, which is the v11 bump (M2).
**Builds on:** match vision spec W (the whole-match export, shipped), spec C (chapters, `chapters::splice`), Phase 9 (`ScoreboardContext`, the match clock), Phase 8 (the export run, `.part` and rename, spec E5/E6/E8).
**Evidence:** measurements taken on this machine on 2026-09-23 against the user's own two-file match. The footage's *properties* are quoted; its teams are not — the repository is public.

Labels, as in the match vision spec: **[measured]** was measured here, **[cited]** comes from a named source, **[estimate]** is arithmetic.

---

## Goal

The whole-match export re-encodes both halves to burn a scoreboard into the picture. The user's 56-minute match came out at **7.9 GB (~19 Mbit/s) from ~5 Mbit/s sources, in about an hour** [user]. The coach asked for the other trade: *"is there a way to not re-encode it and use different tracks to do the scoreboard" / "so it is lossless with overlay"*.

So: **copy the video and audio, join the halves, and carry the scoreboard as a timed-text track the player draws.** Chapters stay as they are.

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

**L1. One GStreamer graph, stream copy end to end.** Per source: `filesrc ! qtdemux`, then `queue ! h264parse` into a shared `concat`, and `queue ! aacparse` into a second `concat`. The two `concat`s feed one `mp4mux ! filesink`. A third pad carries the cues (**T**).

- **`queue` after every demux pad and before every mux pad is not optional** [measured]: without them the graph deadlocks on the first file, because `qtdemux`'s single streaming thread pushes video into an aggregator that is waiting for that same thread's audio.
- **`concat` with `adjust-base=true`** (its default) makes each source's segment start where the previous one ended, so the muxer sees one continuous timeline.
- **Nothing decodes and nothing touches the GPU.** The copy path needs no `Gl`, no display and no encoder, so it runs where CI runs.

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

**L5. The copy writes `<path>.part` and renames, like every other export.** Nothing else in the `.part`/rename/delete contract changes. The chapter splice runs on the `.part` before the rename, as today.

**L6. Sources must match, and a mismatch refuses the export.** Before the graph is built, each source is read (one `Discoverer` probe per file — the same call `probe.rs` already makes) and compared to the first:

| Must match | Why |
|---|---|
| Video codec | Obvious |
| Video `codec_data` (the `avcC`, byte for byte) | One `stsd` entry is written. It carries the SPS and PPS, and so also the profile, level, resolution and chroma. Comparing the bytes is one comparison instead of six, and it cannot be fooled. |
| Audio codec and `codec_data` (the `esds` ASC) | Same reason: sample rate, channels and object type all live in it. |
| Audio present-or-absent | See **E4**. |

- **A mismatch refuses, naming the file and the field:** *"the second video was recorded differently from the first (its H.264 parameters differ); pick Scoreboard: burned in to export it re-encoded."*
- **Why refuse rather than silently re-encode.** The coach asked for a copy that takes half a minute. Quietly spending an hour instead is the worst available answer, and the fallback is one picker away. This also keeps the code honest: there is exactly one re-encoding path, the one that already exists.
- The project's aspect gate (`Project::check_aspect`) already refuses a source whose display aspect differs from the project's, so the common mismatch never reaches here.

**L7. What the join does *not* fix.** The second file's AAC keeps its own 1024-sample encoder priming, so the sound at the join starts about 21 ms late relative to a perfectly spliced stream. **[measured]** total audio runs 3256.450 s against video's 3256.459 s — a 9 ms difference across the whole match, which is inaudible and below the app's own measured A/V offsets. Removing it would mean re-encoding the audio, which is the thing this spec exists to avoid.

### T. The scoreboard track

**T1. Two carriers, from one cue list: an embedded `tx3g` track and a sidecar `.srt`.**

Both are written for every target rendered in track mode, from the same `Vec<Cue>` (**U**).

- **Embedded**, so the scoreboard travels with the file when it is copied to a phone or a stick. **[measured]** `mp4mux` 1.24.2 has a `subtitle_%u` request pad taking `text/x-raw, format=utf8` and writes a `tx3g` / `mov_text` track with `disposition.default = 1`.
- **Sidecar**, named `<label> - <project>.srt` beside the `.mp4`, **because that is the one that actually shows**. **[measured]** VLC 3.0.20 reads the embedded `tx3g` track and lists it (`adding track[Id 0x3] subtitle (enable)`) but creates **no** subtitle decoder for it — the viewer has to turn it on. The sidecar with the matching basename is auto-detected (`autodetected subtitle: …/side.srt with priority 4`) and decoded without being asked.
- The sidecar is also the file YouTube takes as a caption upload [cited], which the embedded track is not.

**T2. What a cue looks like.** One line:

```
Rovers 1 - 0 Athletic · 14:05
```

`"{home} {home_score} - {away_score} {away} · {clock}"`, where `{clock}` is `format_clock`'s `main` with its `trailing` appended when the clock is in stoppage (`… · 45:00 +2:13`), `HT`/`BREAK` on a break, and `FT` after the last period. The team names are the configured ones, verbatim.

- **No fitting and no truncation.** The burned board fits its labels because its cells are a fixed width (spec W3); a subtitle line is not in a cell, and a player that runs out of width wraps. A long club name is the viewer's player's problem, not ours.
- **One line, no markup.** No `{\an8}`, no `<font>`: SRT positioning is a libass extension that VLC's own decoder does not honour, and a tag a player shows literally is worse than a line in the wrong place.

**T3. Where it appears: wherever that player puts subtitles.** Bottom-centre, in the viewer's own subtitle font and size. **[measured]** `mp4mux` sizes the `tx3g` sample entry's text box from the video (1920×162 for a 1080-line picture — the bottom strip), but most players ignore it and use their own subtitle placement. **A coach who wants the scoreboard in the corner, in the team's colours, picks "burned in"** — that is the whole point of the option.

**T4. How often it changes, and how many cues that is.** A cue per distinct line. The clock reads whole seconds, so the line changes once a second while a period runs and not at all during a break.

- **[measured]** A 54-minute match is about 3,256 cues. Muxing 3,400 one-second cues cost **194 KB** on disk and 1.5 s of wall time. Both are noise against 2.1 GB and 26 s.
- **Cues are contiguous:** each cue ends where the next begins, so the muxer writes no empty samples between them. (Feeding the same cues through `subparse` from an `.srt` produced 6,799 samples instead of 3,400 — one empty sample per gap. Pushing them ourselves avoids that.)

**T5. The cues are pushed from an `appsrc`, unbounded, and the pad is requested only when there are cues.**

- `appsrc name=cues format=time caps=text/x-raw,format=utf8 max-bytes=0` into `mux.subtitle_0`. The whole list is known before the run starts; it is pushed up front and the pad is EOS'd. 3,400 buffers of ~40 bytes is nothing to buffer.
- **An empty cue list requests no pad.** `mp4mux` is an aggregator: a requested pad that is never fed stalls the run, and one fed nothing writes an empty track. A project with no scoreboard gets a file with no text track (**E5**).

**T6. The sidecar is written after the rename, and a failure does not fail the export.** Like the chapter splice, it is reported: `ExportDone` gains `sidecar: Option<PathBuf>`, and the `bus: exported …` line logs it. An export that produced a good `.mp4` must not be thrown away because a 200 KB text file could not be written.

**T7. Which player shows what** [cited, except VLC which is measured].

| Target | Embedded `tx3g` | Sidecar `.srt` |
|---|---|---|
| VLC (desktop) | Listed, **off until the viewer picks it** [measured] | **Auto-loaded and shown** [measured] |
| mpv | Shown (it is a subtitle track like any other) | Auto-loaded (`--sub-auto=exact` is the default) |
| Phone — VLC / ExoPlayer-based players | Shown | Shown if it is copied alongside |
| Phone — iOS, Photos, AirDrop | `tx3g` is QuickTime's own timed text; AVFoundation offers it under subtitles | Usually not copied |
| Phone — Google Photos | Ignored | Not copied |
| TV — USB stick or DLNA | Varies by set; often ignored | Often ignored |
| YouTube | Ignored on upload | **This is the file you upload as captions** |

The honest summary for the user: **on a computer it just works; on a TV it is a coin toss.** The burned option is there for the audience that can't.

### U. Where the cue text comes from

**U1. One pure function in `video-coach-core`, over the compilation's frames.**

```rust
pub struct Cue { pub start: f64, pub end: f64, pub text: String }

pub fn scoreboard_cues(compilation: &Compilation, scoreboard: &ScoreboardContext) -> Vec<Cue>;
```

For each output frame `n`, take `plan.entries[frames[n].entry].source_index` and `frames[n].source_time`, call `ScoreboardContext::state_at`, render **T2**'s line, and run-length-encode the result. A run from frame `a` to frame `b` becomes a cue from `a / OUTPUT_FPS` to `(b + 1) / OUTPUT_FPS`. Frames where `state_at` is `None` produce no cue — a gap, which is right: before kick-off there is no match to show.

- **`state_at(source_index, source_time)` per frame is the rule, not a shortcut around it** (CLAUDE.md, Phase 9). A whole-match entry has one source and plays it straight through, but the same function serves a clip that freezes — and there the run-length encoding is what makes the clock hold through the pause instead of running on. `core`'s existing pause test covers the same invariant for the burned board.
- **No media dependency, no new type but `Cue`.** It reads `Compilation`, which is already core's.
- **Cost:** about 97,700 `state_at` calls for a 54-minute match — exactly the number the burned export already makes, and there it is dwarfed by encoding. Here it is the one piece of real work on a 26 s copy, and it is still a fraction of it [estimate].

**U2. Output time is the plan's time, for both carriers.** The cue times come from frame indices at `OUTPUT_FPS`, never from a sum of source durations (CLAUDE.md, Phase 8).

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
| **Separate track** | `Track` for every ticked target. Clips and reels still re-encode — for the drawings, the inset, the zoom and the text bar — but the scoreboard is not painted into those pixels. |

- **Why three and not two.** The user's rule is a *per-target* default (track for the whole match, burned for a clip), and two values cannot express "I have not chosen" separately from "I chose burned". The per-target rule is one pure function, `default_scoreboard_mode(&ExportTarget) -> ScoreboardMode`, stated once and read by the sheet and the job builder alike.
- **The picker's value is stored as `Option<ScoreboardMode>`** — `None` is Default.

**M2. It is remembered in `Preferences`, and that is a format bump to v11.**

```rust
#[serde(default)]
pub last_export_scoreboard: Option<ScoreboardMode>,
```

- **Why `Preferences` and not `state.json`.** The other two export pickers (`last_export_resolution`, `last_export_quality`) are already there, and they are there because the choice belongs to *this match*, not to the machine. Splitting the three pickers across two stores to dodge a version number would be the worse design. The whisper model is in `state.json` for the opposite reason — it is the coach's, machine-wide.
- **The bump is additive and needs no migration** (match vision F2): an `Option` with a field-level `#[serde(default)]`, where `None` is exactly what an older file means. `MIN_READABLE_FORMAT_VERSION` stays 7, and `write`'s one-time `project.json.v<old>` backup (F1) covers the way back.

**M3. The mode reaches media as one field.** `ExportJob` gains `scoreboard_mode: ScoreboardMode` and `cues: Vec<Cue>` (empty in `Burned` mode, or when there is no scoreboard).

- `ExportJob::scoreboard` keeps its meaning and is still built in both modes: the reel's entry text and the whole match's chapter titles come from it (spec R2, W3) whether or not the board is painted.
- **Only the drawing is conditional.** `overlay.rs` draws the scoreboard when the mode is `Burned` and skips it when it is `Track`. Highlights, strokes and the text bar are untouched.

**M4. The preview always draws the scoreboard.** The mode is an export choice. The preview is where the coach checks the edit, and a preview that hid the board to match an export setting would just be a worse preview.

**M5. The row detail reflects the effective mode, and the picker refreshes the rows.** Changing the picker calls `export_targets` again — one callback. The mode changes what the whole-match row *means*, so it must change what the row *says*.

### X. The export target, the run and the sheet

**X1. `ExportTarget::WholeMatch` stays; it gains a second renderer.** No new target, no new row, no second progress model.

- `ExportTarget::WholeMatch` + `ScoreboardMode::Track` → the copy (**L**).
- `ExportTarget::WholeMatch` + `ScoreboardMode::Burned` → today's encoded path, unchanged.
- Every other target re-encodes in both modes.
- The choice is made in `composite::export::run`, which branches once, at the top, into `copy::run` or the existing `export`. They share `part_path`, the rename, the delete-on-failure, the chapter splice and the sidecar write.

**X2. `ExportTargetRow` carries a `detail: String` instead of `count` + `unit`.** The pluralisation currently sitting in `main.rs` moves into `export_targets`, which is the only place that knows what a row counts. What the rows read:

| Target | Detail |
|---|---|
| Whole match, track mode | `"54:16 · copied, not re-encoded · 1920×1080"` |
| Whole match, burned mode | `"54:16 · re-encoded"` |
| All clips / a tag | `"7 clips · 12:40"` (unchanged) |
| A reel | `"5 goals · 03:00"` (unchanged) |

- **"2 videos" goes away.** It was the honest warning when the row meant an hour of encoding (spec W4); it is the wrong thing to say about a copy. The running time stays, because that is what the coach is choosing.
- **The size on the copied row is the source's,** because a copy cannot be resized. The Resolution and Quality pickers still apply to the other ticked targets; they do nothing to a copied one, and the row says so by naming the size it will actually produce.

**X3. `ExportMessage::Progress` keeps its meaning: output frames of the plan.**

- A pad probe on the video branch reads each buffer's running time and reports `round(seconds × OUTPUT_FPS)`, clamped to `plan.total_frames()`, throttled to whole-percent changes as today. EOS reports the total.
- **Why not bytes.** The run's model is frames across targets, its rate window is frames per wall second, and "Finishes at …" divides remaining frames by that rate (spec E5). Reporting the copy in the same unit means the sheet, the rate window and the estimate all keep working with no change — a run that copies the match and then renders three clips still has one honest number.
- The rate will read about 3,600 output frames per wall second instead of 20. The sheet shows no estimate until the rate is steady, and a copy is over in half a minute, so nothing needs smoothing.

**X4. Cancel is unchanged.** The copy polls the same `AtomicBool`, stops the pipeline, and `run` deletes the `.part`. Cancelling a copy that has already finished still reports it done, as the run's contract says. A cancelled copy leaves no sidecar: the sidecar is written after the rename.

**X5. `ExportDone` reports `encoder: "copy"`** and a default `Diagnostics` (the copy selects no decoder, uploads nothing and has no GL platform). `chapters` and the new `sidecar` are reported as usual. The `bus: exported …` line reads the same shape, so nothing that reads the log changes.

### N. What a copy cannot carry

**Say it once, plainly, and put it in the sheet.** A copied whole match is the camera's own pixels. **Nothing that is drawn can be in it:**

- player highlights (the ring and the label),
- pen drawings,
- the webcam or avatar inset,
- zoom,
- the text bar,
- and the scoreboard itself, which is why it rides as a track.

Of these, only **player highlights** are a loss against today's burned whole match — a whole-match entry already has no clip, so it never had drawings, an inset, zoom or a caption (spec W2).

**What the coach uses instead: a clip, or the goals reel.** Those are the exports that exist to carry the coaching on top of the footage, and they burn it in by default. The whole match is the film; the clip is the lesson.

The sheet says it on the picker, in one line under it when the mode is Track: *"Highlights and drawings can't ride a copied file — use a clip or a reel for those."*

### E. Edge cases

**E1. One source only.** The row is present and the copy still earns its place: it adds the scoreboard track and the chapters to a single file. `concat` with one input is a pass-through. Nothing special-cases it.

**E2. Sources that differ in codec, parameters or size.** Refused, naming the file (**L6**). The coach's way out is the Burned picker, which the message names.

**E3. A source with no video.** Cannot happen: `probe` refuses it at add time (`ProbeError::NoVideo`).

**E4. Audio.**

- **No source has audio** → no audio pad is requested and the output has none.
- **Every source has audio, with identical `codec_data`** → copied.
- **Mixed** → refused, like any other mismatch. Splicing encoded silence in would mean running `avenc_aac`, whose ASC will not match the other file's, which is the same mismatch one layer down.

**E5. No scoreboard set up.**

- **No cue list, so no text track and no sidecar.** The copy runs. Inventing "Home 0 - 0 Away" would be a claim about a match nobody tagged.
- **Chapters fall back to what they already do** (`whole_match_chapters`): the tagged events in the panel's plain wording, or one chapter per source named after the file when nothing is tagged.
- The whole-match row's detail is unchanged; it is not the place to teach the scoreboard.

**E6. A tagged match with no periods yet** (a goal tagged, no kick-off). `state_at` is `None` throughout, so there are no cues, as for E5. The chapters still carry the goals.

**E7. A very long match.**

- Cues: about 60 an hour × 60 → a 3-hour recording is ~11,000 cues, about 650 KB [estimate from the measured 194 KB / 3,400].
- Chapters: `MAX_CHAPTERS` is 255 and a match has tens. Untouched.
- **Above 4 GB the muxer must write `co64` rather than `stco`.** The measured 2.1 GB file used `stco`. A 3-hour match at 5 Mbit/s is ~6.7 GB and crosses it. `mp4mux` switches automatically, but this is not measured here — see Open questions.

**E8. A break in the middle of a half's file** (one file holding both halves). Nothing here cares: the cue list follows `state_at`, which follows the tagged events, and a break simply becomes one long `… · HT` cue.

---

## Crate responsibilities

| Crate | Holds |
|---|---|
| `video-coach-core` | `Cue`, `scoreboard_cues`, `cues_to_srt`, `ScoreboardMode`, `default_scoreboard_mode`, `Preferences::last_export_scoreboard`, the v11 bump. All pure; no media dependency. |
| `video-coach-media` | `composite/copy.rs`: the copy graph, the source-compatibility check, the cue `appsrc`, the progress probe. `ExportJob::{scoreboard_mode, cues}`, `ExportDone::sidecar`, the sidecar write. `overlay.rs` skips the board in track mode. `chapters::splice` unchanged. |
| `video-coach-app` | The sheet's third picker, `ExportTargetRow::detail`, refreshing the rows when the picker changes, writing the choice back to `Preferences`. |
| `video-coach-harness` | The end-to-end run over the bus: tick the whole match in track mode, get a file with a copied stream, a text track, a sidecar and chapters. |

Nothing moves between crates, and no new dependency appears in any of them.

---

## Testing

**No network, no camera, no microphone, no real footage.** The user's match is not committed; the repository is public and the footage shows children.

**Core unit tests** (`crates/video-coach-core/tests/cues.rs`):

1. **The cue list of a two-source match.** Kick-off, a goal, a half-time, a second-half start, full time. Assert: no cue before the first start; the score turns over on the goal's own frame; the break is **one** cue reading `HT`; `FT` after the last period; every cue contiguous with the next within a run.
2. **A pause holds the clock.** A clip entry that freezes produces one long cue, not a running one — the same invariant the burned board's pause test pins (BACKLOG #27).
3. **Stoppage** appends `+M:SS`.
4. **No scoreboard configured** → an empty list.
5. **`cues_to_srt`** formats hours, milliseconds and numbering; an empty list is an empty string.
6. **`default_scoreboard_mode`** is `Track` for `WholeMatch` and `Burned` for every other variant.

**Media tests** (`crates/video-coach-media/tests/copy.rs`), all on generated fixtures:

7. **The remux is lossless.** Two `CounterKind::H264Mp4BFrames` fixtures (H.264 with B-frames in MP4, and an edit list — the trap this codebase already keeps a fixture for). Copy them. `decode_counters` must read `0..N` then `0..M` with nothing missing, repeated or out of order. `ffprobe` must report the same `codec_name`, `profile`, `width`, `height` as the input, and a video packet count equal to the sum of the inputs'.
8. **The text track is there.** With a cue list, `ffprobe` reports a third stream with `codec_tag_string=tx3g` and the expected sample count; the sidecar `.srt` exists beside the `.mp4` and round-trips through `cues_to_srt`.
9. **The chapters survive the copy.** `ffprobe -show_chapters` reads them back, at the expected times. This is the check that `reserved-max-duration` was set (**L4**); without it the splice skips and the test fails.
10. **A mismatch refuses cleanly.** Two fixtures at different sizes → `ExportError::Failed` naming the second file, **no `.part` and no file at the target path**.
11. **Audio.** A new fixture is needed: H.264 + AAC in MP4 (`avenc_aac`, already a runtime dependency). Two of them copy; one with audio and one without refuses; two without audio produce a video-only file.
12. **Cancel.** Cancel mid-copy on a long-enough fixture: `ExportError::Cancelled`, no `.part`, no output.
13. **No cues, no track.** An empty cue list produces a two-stream file and no sidecar — and, importantly, does not hang.

`ffprobe` is already a test-only dependency (`packaging/build-deps.txt`, match vision C3). These tests fail without it; they never skip.

**Harness test** (`crates/video-coach-harness/tests/whole_match.rs`): one run over the bus in track mode, asserting the run's progress reaches `total_frames`, the sheet's row detail, and that the file and sidecar land in `exports/`.

**What needs the user's eyes**, on their own match, once:

- Open the copied file in VLC on the laptop: the sidecar loads by itself and the score and clock track the play.
- Scrub across the join — no stutter, no flash, sound continuous.
- `[` / `]` and the chapter list land where they should.
- Play it on whatever the parents actually use: the TV, a phone, a share. Report which of them shows the line. That answer decides whether the sidecar alone is enough, or whether anything more is worth building.

---

## Deferred

1. **A styled track (ASS in Matroska).** It would give a coloured box in the corner, closer to the burned board. It costs a second container, a second chapter path and a second set of player assumptions, and the coach who wants that already has "burned in". Revisit only if the user asks for a styled overlay that is still switchable.
2. **`avc3` with in-band parameter sets,** which would let two halves with *different* SPS join without re-encoding. Deferred because it is not needed for the design footage (the parameter sets are byte-identical) and `avc3` has weaker player support than `avc1`.
3. **Re-encoding only the source that doesn't match,** instead of refusing. Real but rare, and it turns a predictable half-minute into an unpredictable hour.
4. **Removing the second file's AAC priming** at the join (**L7**): 21 ms, inaudible, and the fix is an audio re-encode.
5. **Naming the track "Scoreboard".** MP4 has no track-name box that players agree to show. `und` language, no name.
6. **A second text track carrying the transcript** (Phase 10). The machinery here would take it unchanged; nobody has asked.
7. **Positioning tags in the sidecar** (`{\an8}`), which libass-based players honour and VLC's own decoder does not. Revisit if the user says the line is in the way.

## Open questions

Each has a recommended default; the plan proceeds on it unless the user says otherwise.

**Q1. Three picker entries, or two?** The three-entry picker (Default / Burned / Separate track) exists so that "track for a whole match, burned for a clip" can be the default without the coach choosing per target. Two entries would be simpler but would force one default for everything.
**Recommended:** three, as **M1** describes. If the "Default" entry confuses in use, collapse to two with `Burned` as the default and let the whole-match row's live detail advertise the copy.

**Q2. `Preferences` (a v11 bump) or `state.json`?**
**Recommended:** `Preferences`, v11 — it keeps the three export pickers in one place, and the bump is additive (**M2**).

**Q3. Sample the scoreboard per output frame, or once a second?** Per frame is ~97,700 calls on a 54-minute match; 1 Hz is 3,256, but a score change would land up to a second late.
**Recommended:** per frame. It is the same call the burned export already makes, it needs no second code path, and it is exact.

**Q4. Ship the sidecar for clips and reels too, or only for the whole match?**
**Recommended:** for every target rendered in track mode. One rule, and the sidecar is what actually shows (**T1**).

**Q5. Does a copied file above 4 GB mux correctly?** `mp4mux` should write `co64`; the measured file was 2.1 GB and used `stco`. A 3-hour match would cross it.
**Recommended:** measure it once during the plan with a generated long fixture rather than reasoning about it. If it fails, refuse above 4 GB with a message rather than writing a broken file.

**Q6. Should the Resolution and Quality pickers grey out when only a copied target is ticked?**
**Recommended:** no. They still apply to any other ticked target, and greying them on a tick change is more UI state than the confusion is worth. The row's detail already names the size the copy will produce.

**Q7. Does the copied whole match still deserve to be un-ticked by default** (spec W1, "the longest render there is")? It is now the *shortest*.
**Recommended:** leave it un-ticked. It is still 2 GB the coach did not ask for, and the row is right there.
