# Coach Cuts: try-everything checklist

Work down the list in order. It follows a normal session: install, open a project, scan the game, record, tidy clips, preview, export, scoreboard, transcripts.

- **[must work]** marks the few checks where a failure means something is really broken, not just rough. If one of these fails, stop and report it.
- **[cam+mic]** means have the webcam and microphone plugged in.
- **Where the app's log goes:** launched from the menu, the app writes its log to `~/.xsession-errors`. To see the last line of a given kind, run `grep "bus: loaded" ~/.xsession-errors | tail -1` in a terminal.

## 1. Install

- [ ] **Install the package.** Run `sudo apt install ~/Downloads/coach-cuts_0.4.0_amd64.deb`. It replaces the copy you have. **Right:** apt finishes without errors. (To build one yourself instead, run `packaging/build-deb.sh` in the `coach-cutups` folder; the result lands in `target/debian/`.)
- [ ] **[must work] In the menu.** Open the Mint menu and type "Coach Cuts". **Right:** it's listed with its own icon, not a blank or generic one. Launch it. **Right:** the running window shows the same icon in the panel. If you pin the launcher to the panel, the open window groups onto that pin instead of appearing as a second, separate button.

## 2. First launch and your project

- [ ] **Create a project.** On first launch the window says "No project open". Click **Open Project…** and choose a new, empty folder. **Right:** the folder picker works, the project opens, and `project.json` appears in that folder. **Ctrl+O** opens the same picker.
- [ ] **Reopen it.** Quit, then launch again from the menu. **Right:** the same project comes back on its own.
- [ ] **Window size.** Resize the window, quit and relaunch. **Right:** it reopens at the size you left it. Then shrink it as small as it goes. **Right:** every button (including **Export…** and **Devices…**) and the colour swatches are still visible and clickable. The right-hand panel scrolls if it doesn't fit.

## 3. The game video: add it and scan it

- [ ] **Add the game.** Click **Add Source Video…** and pick your game file. **Right:** it appears under Sources with its length, and the picture shows.
- [ ] **Add several at once.** In a fresh project, click **Add Source Video…** and select both halves together (Ctrl-click or Shift-click). **Right:** both are added, in file-name order.
- [ ] **[must work] Hardware decode.** Right after adding the video, run `grep "bus: loaded" ~/.xsession-errors | tail -1`. **Right:** the line contains `vah265dec` (or `vah264dec` for an H.264 file), `memory:DMABuf` and `egl`. Anything else (for example `avdec_…`, `SystemMemory` or `glx`) means playback is using the slow path, so send the line.
- [ ] **Two videos as one game.** Add a second file, such as the second half. Drag it above or below the first to reorder, then play across the join. **Right:** playback carries straight on into the next file without hanging, and the readout and scrubber count the two as one timeline. The **×** beside a source removes it. It is greyed out, with a tooltip saying why, once a clip or match event uses that video.
- [ ] *(Optional)* **Add a wrong-shaped video**, such as a portrait phone clip or one with a different aspect ratio. **Right:** it's refused with a message, not added.
- [ ] **Play and skip.** **Space** plays and pauses. **←/→** (or **A/D**) skip 3 s and **Shift+←/→** skip 10 s. **Right:** each one does exactly that.
- [ ] **Scrub.** Drag the scrubber. **Right:** the picture follows live while you drag, and stops exactly where you let go.
- [ ] **[must work] Play after scrubbing.** While playing, drag the scrubber back and forth quickly a few times, let go, then press Space. **Right:** it plays, and the time readout (like `12:34 / 45:00`) counts up. Do the same while paused. (This is the bug you hit first time round.)
- [ ] **Scrub lands where it says.** Pause, then scrub to a few places in a half. **Right:** the picture and the readout agree every time.
- [ ] **Step one frame.** Pause, then press **.** a few times, then **,** a few times. **Right:** the readout shows tenths while paused (`12:34.5 / 27:10`) and they move with each press; each press moves the picture by exactly one frame; **,** brings you back to where you started.
- [ ] **Fast scanning.** While playing, press **L** repeatedly: 2×, 4×, 8×, 16×, 32×. **J** goes back down. The speed button beside Play does the same, wrapping back to 1× after 32×. **Right:** the readout shows the speed (`· 8×`), the picture keeps moving even at 32× and stays with the readout, the sound is muted above 1×, and pressing Space pauses on the frame you saw. Play again starts at 1×. **R** while fast starts the recording at 1×. Also say whether 32× feels usable for finding a kick-off.
- [ ] **Keys after sliders.** Drag the scrubber, then the **Volume** slider (which should change the game sound), then press Space and the arrows. **Right:** the keys still play and skip, and don't nudge the slider.
- [ ] **Hold → while playing** for a couple of seconds, then let go. **Right:** the video jumps ahead steadily and settles without a visible jump backwards. Also tell us if holding the key feels laggy or overshoots.
- [ ] **Zoom and pan.** Try each of these:
  - Ctrl + two-finger scroll zooms toward the pointer.
  - A readout like "1.75×" appears once you're zoomed past 1×.
  - Plain two-finger scroll pans, and so does click-drag.
  - **3** zooms in and **2** zooms out, around the pointer when it's over the picture.
  - **1** or **Ctrl+0** goes back to the full picture.

  **Right:** all of that works, and the black bars stay black: the zoomed picture never spills into them.

## 4. Recording commentary [cam+mic]

- [ ] **Pick devices.** Click **Devices…**. **Right:** it opens next to the button, shows "Looking for devices…" briefly, then lists cameras and microphones. The laptop's infrared face-login camera is not listed. Pick your webcam and mic, close the popover and reopen it. **Right:** your picks are still ticked.
- [ ] **[must work] Record a take.** Press **R** and check each of these:
  - The button area shows "Preparing recording…" (maybe also "Waiting for audio…").
  - Then a red **Recording** label appears with a running time.
  - The level bar moves when you talk.
  - The webcam light is on only while recording.
  - **Your webcam shows live** in the picture's bottom-right corner within about a second, where the inset lands in the export. It disappears when you stop.

  Press **R** again to stop (**Esc** and the **Stop** button also stop). **Right:** a clip named like `1-00:12:34` (source number, then the game time where you started) appears under Clips. The button's tooltip reads "Record (R)", and "Stop recording (R or Esc)" while recording.
- [ ] **Cancel and key repeat.** Press R, then press R again during "Preparing…". **Right:** it cancels and no clip appears. Holding R down doesn't flicker recording on and off.
- [ ] **Listen to the start of a take.** Record a few seconds, then preview it with headphones on. **Right:** it starts cleanly. **Tell us** if you hear a thump, pop or click right at the start — an audio analyzer suggests there may be one.
- [ ] **Clap test.** Start a take and clap once, clearly in view of the webcam. Keep this clip, because the preview and export checks use it.
- [ ] **Live webcam view.** While recording, check the inset's colours look natural and it isn't stretched. Run `top` in a terminal during a take and **tell us** the `coach-cuts` CPU figure (we expect roughly 40–60% of one core in total). Then compare with the export: export a clip and check the inset sits in the same place you saw it while recording.
- [ ] **Picture and lip sync.** Record in a normally lit room. Open the newest `.mkv` in the project's `recordings/` folder in a video player. **Right:** the picture is reasonably sharp, not blocky, and your lips match your voice.
- [ ] *(Optional, needs a USB camera or mic)* **Unplug a device.** Choose it in Devices…, unplug it, then press R. **Right:** a notice line says it fell back to the default device, instead of the recording failing. Plug it back in, and it's picked again.
- [ ] **Crash safety.** While recording, run `pkill -9 coach-cuts` in a terminal. **Right:** the newest `.mkv` in `recordings/` still plays up to the kill. **Expected:** after you relaunch, that take is *not* in the clip list. That's by design, not a bug.

## 5. Drawing [cam+mic]

Drawing only works while recording, so record a take for these.

- [ ] **Draw.** Press and drag on the picture. **Right:** a red line follows the pointer closely. A single click leaves a dot.
- [ ] **Pen colours.** Click each swatch at the bottom left (red, fluorescent yellow, neon green, blue, white, hot pink) and draw with it. **Right:** each line is that colour with a thin dark edge that keeps it readable on grass and kits. Quit and relaunch. **Right:** the colour you last picked is still chosen. Also try a drag that starts on your webcam inset. **Right:** it draws.
- [ ] **Auto-clear.** With **Auto-clear** ticked (the default), a drawing fades 5 s after you lift. Untick it, and drawings stay until you click **Clear** or press **C**. Both also wipe a line you're halfway through.
- [ ] **Navigation still works.** While recording, two-finger scroll still pans and Ctrl+scroll still zooms. Only dragging draws.
- [ ] **Edges.** Drag off the edge of the picture. **Right:** the line runs along the edge and doesn't go into the black bars.
- [ ] **Stop mid-line.** While still holding the button down, press R to stop. **Right:** the half-drawn line disappears and never shows up in the replay.
- [ ] **Not recording.** Dragging pans and draws nothing, and **Clear** and **Auto-clear** are greyed out. **Right:** a drag shows the hint "Drawing works while recording — press R" next to the controls.

## 6. Clips: naming, tags, notes, undo

- [ ] **Select and jump.** Click a clip. **Right:** the right-hand panel shows its Name, Tags, "Show webcam in export", Notes and Transcript. Esc deselects it, and the panel then shows your tag list instead. Double-clicking a clip jumps the game video to the clip's start, paused. Right-clicking gives **Jump to clip start / Preview clip / Export video… / Delete clip**.
- [ ] **Typing never triggers a shortcut.** In the project name field and in the clip's name, tags, notes and transcript fields, type words containing r, c, z, x, v, a, d, 1, 2, 3 and spaces. **Right:** everything lands in the field. Nothing records, plays, clears, tags a goal or zooms.
- [ ] **Leaving a field saves it.** Rename clip A, then click clip B without pressing Enter. **Right:** A keeps its new name and B shows its own. Enter keeps you in the name or tags field. Esc leaves the notes field. Clicking the video or empty sidebar space leaves any field.
- [ ] **Tag suggestions.** Once some clips have tags, start typing a tag on another clip. **Right:** matching tags are suggested. Tab takes the top one and clicking takes any of them. Esc closes the list, and a second Esc leaves the field.
- [ ] **Filter by tag.** With no clip selected, click a tag in the tag list. **Right:** the clip list shows only those clips ("Filtered: …"). Clicking the tag again, or the ✕ on the filter chip, shows them all again.
- [ ] **Order.** Drag a clip up or down the list, then click **Sort by position**. **Right:** dragging moves it, sorting puts clips in game order, and Ctrl+Z undoes each.
- [ ] **Undo and redo.** Ctrl+Z inside a field undoes your typing. Outside a field, it undoes the last clip change. Ctrl+Shift+Z or Ctrl+Y redoes.
- [ ] **Delete and bring back.** Delete several clips with the Delete key, then press Ctrl+Z repeatedly. **Right:** each clip returns to its old place, and still previews with its commentary.
- [ ] **Record while typing** [cam+mic]. Start typing a note, then click **Record**. **Right:** what you typed is kept, the clip panel greys out during the recording, and R/Space still work.

## 7. Preview

- [ ] **Open a preview.** Select a clip and click **Preview** (or right-click, then Preview clip). **Right:** it starts within a fraction of a second.
- [ ] **[must work] The replay matches what you did.** Preview a take where you paused, skipped, zoomed, panned and drew. **Right:** each of those happens at the same moment it did live. Every drawing appears on the same spot (on the player you drew it on) and fades when it did. Your webcam inset shows in the corner, and your voice matches your lips in it. On the clap clip, the clap sound lands on the frame where your hands meet. **Expected:** no game sound in preview, only your commentary. That's planned for later.
- [ ] **Preview controls.** Space pauses and resumes. The scrubber lands exactly where you drop it, and skips work. Your commentary goes quiet while you drag the scrubber.
- [ ] **End of clip.** Let it play to the end. **Right:** it stops on the last frame. **Tell us** whether that stop happens promptly or about a second late.
- [ ] **Close.** Press **Esc** or **Close Preview**. **Right:** you're back on the game video where you left it, and the preview's last frame doesn't linger. With no preview open, Space plays the game video.
- [ ] **Blocked while previewing.** **Record** and **Export video…** are greyed out.
- [ ] **Switch clips.** Preview one clip, then another straight away. **Expected:** one frame of game video may flash in between. Report it only if it's worse than that.
- [ ] *(Optional, needs a non-16:9 video)* **Drawings stay on the picture** and don't stretch into the black bars.

## 8. Export

- [ ] **The export sheet.** Click **Export…**. **Right:** the sheet lists **All clips**, one row per tag, and the selected clip. Everything is ticked except the single clip. Click Export. **Right:** one file per ticked row appears in `<project>/exports/`, named like `All clips - <project>.mp4` and `<tag> - <project>.mp4`. Right-clicking a clip and choosing **Export video…** opens the same sheet with only that clip ticked.
- [ ] **During the run.** **Right:** each row shows its progress, and a finish time appears after a few seconds and turns out roughly right. Record, Preview and Export video… are greyed out, and the rest of the app still responds.
- [ ] **Cancel.** Click **Cancel export** mid-run. **Right:** videos that already finished are kept, and no half-written file (ending in `.part`) is left in `exports/`. Running the same export again replaces the files without complaint.
- [ ] **Watch it.** Play an export in a video player. **Right:**
  - The caption bar reads `n / total | name | tags`.
  - Pauses freeze on the frame you paused on, skips jump, and a slow zoom pan is smooth.
  - The drawings appear at the same moments and in the same places as when you drew them.
- [ ] **[must work] Listen to it.** **Right:**
  - The game sound plays only while the clip was playing, and is silent during your pauses.
  - Your commentary runs the whole way through, with no clicks where clips join.
  - Game sounds match the picture (a kick sounds when the ball is struck).
  - On the clap clip, the clap in the webcam inset matches its sound.
- [ ] **Webcam inset.** Untick "Show webcam in export" on some clips and export a mix. **Right:** the inset appears only on the ticked clips.
- [ ] **Resolution and quality.** Export the same row at 720p and 1080p, and at Low and High. **Right:** the files differ sensibly in size and sharpness, and the sheet remembers your last choice.
- [ ] **Long names.** Export a clip with a very long name or lots of tags. **Right:** the caption bar ends in "…" instead of running off the edge.
- [ ] **Speed.** **Right:** a 1-minute clip exports in well under a minute. `grep "bus: exported" ~/.xsession-errors | tail -1` names `vah265dec`, `DMABuf` and `vah264lpenc`.
- [ ] *(Optional)* **YouTube.** Upload one export. **Right:** it still looks right after YouTube re-encodes it.

## 9. Scoreboard

- [ ] **Set up the match.** In the Match panel, click **Set up teams…**. Enter both team names, their colours as hex codes and the match format. **Right:** the colour swatches follow what you type, **Save** greys out for a blank name or a bad hex code, and **Cancel** leaves the saved setup unchanged.
- [ ] **A long team name.** Enter "Wolverhampton Wanderers". **Right:** the name shrinks to fit rather than being cut to "Wolver…". **Your call:** if it looks too small to read, say so.
- [ ] **Tag while scanning.** Press **Z** for a home goal, **X** for an away goal and **V** for a period start or stop (the Match panel has the same buttons). **Right:** the score and clock follow the playhead, and each event lands at the moment you were watching. Each event row's → button jumps to it and its × deletes it, and Ctrl+Z undoes a tag or a delete.
- [ ] **Tag while recording** [cam+mic]. Press Z, X and V during a take. **Right:** each one tags, and no dialog pops up over the recording. Once every period is tagged, V does nothing and its button is greyed out.
- [ ] **Video that starts after kick-off.** Use footage that starts partway into the first half, and tick **My video starts after kick-off** in the setup sheet. **Right:** before you tag half-time, the clock counts from the start of the footage. After you press V at half-time, the clock corrects itself so the half ends at the full period length. **Expected:** the board shows `HT` there, never `45:00`. If every period was already tagged when you ticked the box, a yellow warning names the leftover start/stop.
- [ ] **Quarters.** Set a format with quarters. **Right:** the break label `BREAK` fits inside its box, clear of the team names and the board's edge.
- [ ] **On the video.** Preview and export a clip after setting up teams. **Right:** the board sits top-left, is readable full-screen, shows the right score at that moment, and shows the same match time before and after a pause in the clip. In stoppage time it shows a `+M:SS` tail.

## 10. Transcripts

Stopping a recording no longer transcribes on its own: you press **Transcribe** on the clip. The first run needs the internet.

- [ ] **[must work] The model picker.** In the clip's Transcript row, click the small dropdown (it says `small.en`), choose `base.en`, then switch back. **Right:** the popup opens and your pick sticks. Quit and relaunch. **Right:** it's still your pick. This is the one control nothing automatic has ever clicked.
- [ ] **[must work] The first download** [mic]. On a clip with commentary, the button reads **Download 488 MB and transcribe** (148 MB for `base.en`). Click it. **Right:** "Downloading the speech model… N%" counts up, then "Transcribing…" shows with a timer, then the words appear. The next clip just says **Transcribe**, because the download happens once per model.
- [ ] **The words** [mic]. **Right:** it's what you said. **Expected:** names and proper nouns come back garbled. That's what trying `base.en` versus `small.en` is for.
- [ ] **Silence** [mic]. Record 10 seconds of silence and transcribe it. **Expected:** "No speech found", or a made-up "Thank you." or "(electronic beeping)". That's known and accepted. Tell us if it's more annoying in practice than it sounds.
- [ ] **Speed.** Time a real take. **Expected:** with `small.en`, a 5-minute take takes about 7 minutes. `base.en` is faster. Say whether that wait is OK.
- [ ] **Queue and cancel.** Press Transcribe on two clips. **Right:** the second one says "Queued". Press **Cancel**. **Right:** it stops at once and the queued one is dropped too. **Expected:** the laptop's CPU (and fan) stays busy for about 12 s afterwards. Say if that feels broken.
- [ ] **[must work] Record while transcribing** [cam+mic]. Start a transcript, then press R while it's running. **Right:** recording starts immediately, with no freeze. After you stop, that clip's transcript starts again from the beginning.
- [ ] **Edit and undo.** Change a word in a transcript, click away, then press Ctrl+Z. **Right:** it undoes *your* edit. The app writing the transcript is never an undo step. Relaunch. **Right:** your edited transcript is still there.

## 11. Goals reel, chapters and highlights

New in 0.2.0. The first save of an older project upgrades it, so it can no longer be opened by 0.1.x.

- [ ] **[must work] Your old projects open.** Open a project made with 0.1.x and play it. **Right:** everything is where you left it. Save something, then look in the project folder. **Right:** `project.json.v7` sits beside `project.json` — that's the backup, and putting it back is the way to return to 0.1.x, losing anything changed since.
- [ ] **[must work] Scrub lands.** On a Trace half, scrub to a few places. **Right:** the picture and the readout agree.
- [ ] **Frame steps.** Paused, **.** and **,** move one frame forward and back, and the tenths in the readout follow. **Right:** while playing they do nothing.
- [ ] **Fast scanning.** While playing, **L** speeds up (2× … 32×) and **J** slows down; the button beside Play does the same. **Right:** the readout shows the speed, the picture keeps up, the sound is muted above 1×, and a pause puts you back at 1× on the frame you were watching.
- [ ] **[must work] The whole match.** In **Export…**, tick **Whole match** and export. **Right:** both halves end to end, the clock and score burned in and correct throughout, game sound with no commentary, no webcam inset and no caption bar. In VLC (Playback → Chapter) the chapters read "Kick-off", "<your team> goal 1-0", "Half time", "Second half", "Full time".
- [ ] **A reel per team.** **Right:** the sheet has a row per side that scored, named from Set up teams… ("<team> goals"), and an **All goals** row only when both scored. Each exports just that side's goals, numbered `1 / 3` within that reel.
- [ ] **The reel row.** With goals tagged, open **Export…**. **Right:** there's an **All goals** row reading "N goals · m:ss", unticked — you tick it when you want the reel. With no goals there's no row. In a project with goals and no clips, the Export… button still works.
- [ ] **The reel file.** Tick **All goals** and export. In `All goals - <project>.mp4`, **right:** one piece per goal, about 26 seconds each, with the caption `2 / 5 | Home goal | 1-0`; the score on the board turns over on the goal's own frame; no webcam inset; game sound with no commentary. A highlight you placed during the build-up shows in that piece.
- [ ] **Trims.** On a goal's row in the Match panel, park where you want the goal's clip to begin and click **⇤**, then park at the end and click **⇥**. **Right:** the span text updates, **↺** puts it back to −20 s / +6 s, a start *after* the goal is refused with a message, and Ctrl+Z undoes each one.
- [ ] **[must work] Chapters.** Open the reel, and a multi-clip export, in VLC (Playback → Chapter). **Right:** one chapter per goal (or per clip), named with the caption's text. **Tell us** where these get watched — phone, TV, YouTube — so chapters can be aimed at that.
- [ ] **Marks and jumps.** **Right:** the scrubber shows a tick per goal in the scoring team's colour, and a white one per period start and stop. **]** and **[** jump between them, and do nothing while recording.
- [ ] **[must work] A highlight.** Press **H**, pause, and drag a box around a player. **Right:** a ring appears at their feet with a label pill. Then:
  - type `7` in the selected row's label field: the pill reads `#7`;
  - play a second on, pause and drag again; play from before the first key. **Right:** the ring glides from one box to the other;
  - zoom in and pan. **Right:** the ring stays on the player and never spills into the black bars;
  - check the same ring in a preview and in an exported video;
  - drag while playing. **Right:** nothing is placed, and it says "Pause to place a highlight (Space)".
- [ ] **While recording** [cam+mic]. Mid-take, pause, press **H** and ring a player. **Right:** it lands, and **Esc** leaves the tool without stopping the take.
- [ ] **Edit and undo.** Try **Delete key here** (on and off a key frame), the row's delete, and recolouring from a pen swatch, pressing Ctrl+Z after each. **Right:** each undoes on its own. A source that a highlight uses can't be removed, and says why.
- [ ] **Your call:** the ring's size and line thickness, whether the pill stays readable full-screen, and whether keys a second apart are close enough on a panning shot. (If they aren't, that's what the tracking phase is for.)

## 12. Recording with an avatar instead of the webcam

New in 0.3.0. Pick a picture of yourself and the app records your voice only — the camera is never opened — with the picture in the corner, breathing as you talk.

- [ ] **Pick one.** Open **Devices** and, under **Inset**, click **Choose…**. Pick a PNG or JPEG (your gravatar is the case this was built for). **Right:** a round thumbnail appears with the file's name. The picture is copied into the project folder, so your original is untouched.
- [ ] **[must work] Record without a camera.** Press **R**. **Right:** the take starts with no camera light and no camera permission prompt, and the corner shows your picture as a circle straight away. Talk: the circle swells on words and settles between them. It stays on screen for the whole take.
- [ ] **In the finished video.** Preview that clip, then export it. **Right:** the same circle, in the same corner a webcam would be in, pulsing with your voice. It should look the same as what you saw while recording.
- [ ] **Try a photo that isn't square** — a portrait phone photo. **Right:** it fills the same circle in the same place, cropped to its middle, not floating somewhere else or squashed.
- [ ] **Both kinds in one project.** Record one clip with an avatar and, after **Remove**ing the image, one with the webcam. Export both. **Right:** each clip shows what it was recorded with.
- [ ] **Take the picture away.** Delete the copy from the project folder while the app is open. **Right:** Devices says it's missing, the clip's inspector says the clip will export with no inset, and exporting still works — you just get no corner.
- [ ] **Transcription still works** on an avatar clip (there's sound but no picture in the recording).
- [ ] **Your call:** how much the circle grows, and how quickly it settles after a word. Both are one-line constants, so say "more", "less", "snappier" or "calmer" and I'll change them.

## 13. The whole match, copied instead of re-encoded

New in 0.4.0. The export sheet has a third picker, **Scoreboard**, under Resolution and Quality.

- [ ] **[must work] The whole match, copied.** Leave **Scoreboard** on **Default**, tick **Whole match**, export. **Right:** it finishes in a minute or two, not an hour, and the file is about the size of your two halves put together (~2 GB, not 8). A note in the sheet says the whole match is copied and that highlights and drawings can't ride a copy.
- [ ] **[must work] The scoreboard, in VLC.** Open the exported match in VLC. **Right:** the score and match clock appear as subtitles, updating as the match goes, and Subtitle → Sub Track can switch them off. There's a `.srt` file beside the `.mp4` — that's what VLC is reading.
- [ ] **Tell us where you actually watch these.** Play the same file where the parents or players would: phone, TV, Google Photos, YouTube. **Right or not:** the subtitles may or may not show. Whether they do decides if this is enough or if the scoreboard needs to be inside the picture there. **This is the one answer that decides the next step.**
- [ ] **Chapters still work** in the copied file (Playback → Chapter): "Kick-off", "<team> goal 1-0", "Half time".
- [ ] **Burned in, on purpose.** Set **Scoreboard** to **Burned into the picture** and export the whole match again. **Right:** it re-encodes (the slow way), the board is in the picture, and there's no `.srt` beside it — the old one is cleaned up.
- [ ] **Clips are unchanged.** Export a clip with **Separate track** chosen. **Right:** the clip still has its scoreboard burned in — a clip can't carry a subtitle track — and no `.srt` appears beside it.
- [ ] **File sizes.** The quality levels changed: at **Medium** a match is about 2.7 GB where it used to be 10 GB, and **High** about 4.5 GB where it was 19 GB. **Right:** the picture still looks good to you, and the burned-in scoreboard text is crisp. Say if Medium looks worse than you expect.

---

**Reporting back:** for anything that fails or feels wrong, note the item's heading, what you did, what you expected, and what happened. Include the last few lines of `~/.xsession-errors` if the app misbehaved.
