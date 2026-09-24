# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Versions 0.1.0 to 0.5.0 were built and installed by hand and never published as
releases, so there is nothing to download for them. Their dates are the day each
one was cut, and 0.1.1's handful of changes are listed under 0.1.0. The first
published release is 0.6.0.

## [Unreleased]

### Added

- **A chapter list to paste into a YouTube description.** Every export that has
  chapters now writes a plain `.chapters.txt` beside the video: one line per
  chapter, starting at `0:00`. Paste the whole thing into the video's
  description and YouTube turns it into a chapter strip under the player.
  (Uploads ignore the chapters stored inside the file, which is why the list
  exists.) Chapters closer together than ten seconds are left out, because
  YouTube refuses the whole list otherwise.

## [0.5.0] - 2026-09-24

### Added

- **Type or paste in match events instead of tagging them live.** **Edit
  events…** in the Match panel takes one event per line — `2 14:05 home goal`,
  meaning the second video, 14 minutes 5 seconds in — and a whole list at once.
  Each line is echoed back as understood, already there, or refused with the
  reason, and adding them all is a single undo.
- **Fix a wrong event by retyping one line.** Click an event in the list and it
  opens as a single line: the video, the time and the kind. Enter applies it and
  the list re-sorts; Esc leaves it alone. A goal keeps the trims you gave it.
- **A colour picker for team kits.** The swatches in **Set up teams…** now open
  a picker with a row of common kit colours, a hue strip and a shade area. You
  can still type a hex code, and the picker follows it.
- **Every exported file says what it is.** The video carries its title and the
  day the footage was recorded and, once you have set up teams and tagged
  kick-off, the final score and both team names — so a video player or an upload
  can show them without you typing anything.

### Changed

- **A goal's clip starts 20 seconds before the goal** rather than 30, which is
  closer to where the build-up actually begins. You can still trim each goal.

## [0.4.0] - 2026-09-23

### Added

- **The whole match exports as a straight copy of your footage.** It finishes in
  well under a minute instead of an hour, at about a quarter of the size, with nothing
  lost — the original picture and sound are copied rather than recorded again.
  Drawings and highlights can't ride a copy, and the export sheet says so.
- **The scoreboard as a subtitle track.** A copied match comes with the clock
  and score as subtitles — both inside the file and as an `.srt` beside it — so
  VLC shows them as the match goes and Subtitle → Sub Track switches them off.
  The `.srt` can also be attached to a YouTube upload, where the board shows the
  same way. A new **Scoreboard** picker in the export sheet chooses how the board
  travels.

### Fixed

- **Exports are about half the size at every quality.** The encoder had been
  running with no discipline about file size — a 56-minute match at High came out
  at about 7.9 GB. It is now about 4.5 GB, and about 2.7 GB at Medium. The
  picture, and the burned-in scoreboard text, are as crisp as before.
- **Machines with no hardware video encoding were ignoring the quality you
  picked.** They exported everything at roughly the same low quality whatever
  you chose. Low, Medium and High now each mean something on those machines too.

## [0.3.0] - 2026-09-23

### Added

- **Record with a picture of yourself instead of the webcam.** Choose a photo
  under **Inset** in **Devices…** and the camera is never opened — no camera
  light, no permission prompt. Your picture sits in the corner as a circle and
  swells as you talk, live while you record and in the finished video. The
  picture is copied into the project folder, so your original is untouched.

## [0.2.1] - 2026-09-22

### Added

- **Export the whole match as one video,** with the clock and score burned in
  and no commentary or webcam inset.
- **A goals reel per team.** Each side that scored gets its own reel, named from
  **Set up teams…**, and an **All goals** row appears when both sides scored.
- **Chapters named for whoever's watching** — "Kick-off", "Rovers goal 1-0",
  "Half time" — rather than the caption bar's wording.

## [0.2.0] - 2026-09-22

### Added

- **A goals reel.** Every goal becomes one video, each goal cut with the
  build-up before it. Trim where a goal's piece starts and ends, per goal, from
  its row in the Match panel.
- **Chapters in every exported file,** so you can jump straight to a clip or a
  goal in a video player.
- **Goal and period marks on the scrubber,** each goal in the scoring team's
  colour, with `[` and `]` to jump from one to the next.
- **Player highlights.** Press **H**, pause, and drag a box around a player: a
  ring appears at their feet with a label you can type into. Place another box
  further on and the ring glides between them — while you watch, in previews and
  in exports. It stays with the player when you zoom and pan.

### Changed

- **A project made by an earlier version is upgraded the first time you save
  it.** The old file is kept beside the new one in the project folder, so you
  can go back to the earlier version if you ever need to, losing whatever
  changed since.

## [0.1.0] - 2026-09-22

The first Linux release, as a `.deb` for **Ubuntu 24.04 and Linux Mint 22** on
x86-64.

### Added

- **Projects.** A project is a folder: your settings, your commentary
  recordings, and your exports. The game video stays where it is. The app
  reopens your last project on its own.
- **Your game film as one timeline.** Add one video or several — both halves,
  say — reorder them, and play straight across the join. The readout and
  scrubber count them as one match.
- **Scanning.** Space plays and pauses. ←/→ (or A/D) skip 3 seconds and
  Shift+←/→ skip 10. `,` and `.` step back and forward one frame while paused,
  with tenths in the readout. **J** and **L** scan down and up through 2×, 4×,
  8×, 16× and 32×, and pausing puts you on the frame you were watching.
- **Zoom and pan.** Ctrl + two-finger scroll zooms toward the pointer, plain
  scroll or a click-drag pans, **3** and **2** zoom in and out, and **1** goes
  back to the full picture.
- **Recording commentary.** Press **R** and talk over the film with your webcam
  and microphone. Everything you do to the picture while recording — play,
  pause, seek, zoom, pan — replays in step afterwards. Press **R** or **Esc** to
  stop, and a clip appears named for the moment you started.
- **Drawing while you record.** Drag on the picture to draw: six pen colours,
  each with a dark edge so it reads on grass and on kits. Drawings fade five
  seconds after you lift, or stay until you press **C** if you untick
  **Auto-clear**. They replay where and when you drew them.
- **Clips you can find again.** Name them, tag them, add notes, and filter the
  clip list by tag. Reorder them by dragging, or sort them into game order.
  Ctrl+Z and Ctrl+Shift+Z undo and redo.
- **Preview.** Play a clip back with your commentary, your drawings and your
  webcam inset, all in step with the film, before you export anything.
- **Scoreboard and match clock.** Set up the two teams, their colours and the
  match format, then tag goals and the start and end of each period with **Z**,
  **X** and **V**. The clock and score follow the playhead and are drawn into
  previews and exports. If your footage starts after kick-off, tick the box in
  the setup sheet and the clock still reads correctly.
- **Transcripts.** Transcribe a clip's commentary on your own computer with
  [whisper.cpp](https://github.com/ggml-org/whisper.cpp). Choose the more
  accurate model or the faster one; the app downloads it the first time and
  keeps it, and nothing else leaves your machine. The transcript is yours to
  edit.
- **Export.** One `.mp4` per target — every clip, the clips with one tag, or a
  single clip — at 720p or 1080p, into the project's `exports/` folder. Each
  clip carries a caption bar with its number, name and tags, and the webcam
  inset can be turned off clip by clip. A running export shows its progress and
  can be cancelled without losing the files that already finished.
- **Hardware video where your machine has it,** for playback and export, with a
  slower software fallback where it doesn't.

