---
name: measure-media
description: Measure GStreamer decode, seek, or throughput performance on real hardware for the Linux port, and record the result as a spike. Use for the Phase 2 gate, any "is this fast enough" question, or before trusting a media benchmark number.
argument-hint: "[video file path]"
---

# Measure media performance without fooling yourself

Start with the existing tool:

```bash
scripts/linux-gate-check.sh [file]
```

It reports decoder ranks, encoders, the GL mixer chain, capture, and — given a
file — GOP length, the selected decoder, whether frames reach GL zero-copy,
and KEY_UNIT/ACCURATE seek latency through the real display path. Prior
results: `docs/superpowers/spikes/2026-09-19-seek-latency.md`.

To find real footage: large `*.mp4`/`*.mov`/`*.mkv` in the user's home, then
`gst-discoverer-1.0 <file>` for codec and resolution. Real footage beats
synthetic; the user's camera writes HEVC 1440p30 with a 0.5 s GOP.

## Rules for any new benchmark

Every one of these produced a wrong number at least once:

1. **Check that the operation happened.** A rejected `seek_simple()` returns
   instantly and looks like a fast seek. Check the return value and require a
   landed frame (buffer probe on the sink).
2. **Select the video stream by caps.** `decodebin3 ! queue ! fakesink` links
   whichever pad appears first — often audio. That timed audio seeks and
   produced 775 buffer-pool CRITICALs that looked like a GStreamer bug. Use
   `decodebin3 ! video/x-raw(ANY) ! ...`.
3. **Measure through the app's real path.** Into a system-memory sink, every
   frame an accurate seek decodes forward is copied off the GPU: 447 ms vs
   92 ms for the same seek. Measure through `glupload ! glcolorconvert`, and
   confirm the caps into `glupload` say `memory:DMABuf`.
4. **Use `decodebin3`, never `decodebin`, and an EGL GL context.** Either
   mistake falls back to CPU copies (~11× slower). On X11 GStreamer defaults
   to GLX; set `GST_GL_PLATFORM=egl` to mimic the app.
5. **Force a real GL import.** A `fakesink` after `glupload` lets it *pass
   DMABuf through* without importing, which looks zero-copy and proves
   nothing. End the chain with
   `video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D` and check the
   uploader under `GST_DEBUG=glupload:6`: `DirectDmabufExternal` is the
   zero-copy import; `Raw Data` is a CPU copy.
6. **Make comparisons fair.** Same audio handling in every path, startup
   excluded (start the clock at the first frame).
7. **Sanity-check against physics.** Accurate seek cost ≈ frames per GOP ×
   per-frame decode time. A result faster than the decoder can possibly go
   (6 ms for a 120-frame GOP) is a broken benchmark, not a fast machine.
8. **Know your fixtures.** Synthetic `x265enc` files carry a 2-frame PTS
   offset, so accurate seeks land exactly 67 ms late. Huge files show
   cold-cache outliers on the first seek into a region.
9. **Compare hardware and software** by disabling the hardware decoder:
   `GST_PLUGIN_FEATURE_RANK=vah265dec:0,vah264dec:0`.

If python-gi fails to import (a CPython version mismatch happened once), use
the system `python3`, or write the bench against `gstreamer-rs` if the dev
headers are installed.

## Recording results

Write or update a spike in `docs/superpowers/spikes/` with the machine
(CPU, GPU, distro, GStreamer version), exact pipelines, a results table,
which findings change the design, and any traps hit. If a result overturns an
earlier claim, correct the earlier document and say what was wrong — don't
leave both versions standing. Update the spec and `CLAUDE.md` when a finding
becomes a rule.
