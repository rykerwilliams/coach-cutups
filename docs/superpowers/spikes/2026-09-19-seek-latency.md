# Spike — seek latency on real hardware (Phase 2 gate)

**Date:** 2026-09-19
**Question:** The spec's Phase 2 gate says the scan player falls back to libmpv if accurate seek exceeds ~250 ms with a hardware decoder confirmed. Does GStreamer pass on real hardware with real footage?
**Answer:** **Yes, by a wide margin — GStreamer stays the scan player.** But only on a zero-copy decode path, which on X11 requires **both `decodebin3` and an EGL GL context** (see "Correction 2"). The measurement came out wrong four times on the way there. Those are recorded below because each one is a trap the implementation can fall into too.

## Machine

Laptop: Intel Core i7-10610U (Comet Lake, 15 W), Intel UHD Graphics (GT2), Ubuntu 24.04, kernel 6.8, GStreamer 1.24.2, X11 session. A low-power 2020 iGPU — deliberately not a favourable test machine.

## Results — measured through the app's real display path

Pipeline: `filesrc ! decodebin3 ! video/x-raw(ANY) ! glupload ! glcolorconvert ! queue ! fakesink`, measured in PAUSED. Each seek's acceptance is checked and a landed frame is required. Ten seeks spread across each file, offset so targets rarely coincide with a keyframe.

| Source | Codec | GOP | Decoder | Caps into GL | ACCURATE median / worst | KEY_UNIT median / worst |
|---|---|---|---|---|---|---|
| User's camera | HEVC 2560×1440 @ 30 | 0.5 s | `vah265dec` | DMABuf | **10 / 22 ms** | 2.6 / 45 ms |
| 10-hour recording | H.264 1920×1080 @ 60 | 2.0 s | `vah264dec` | DMABuf | **92 / 149 ms** | 6.4 / 273 ms |
| Synthetic | HEVC 3840×2160 @ 30 | 2.0 s | `vah265dec` | DMABuf | 191 / 336 ms | 37 / 54 ms |

**The user's actual footage passes with ~12× margin.** Both real sources pass. The only case over budget is the worst-case seek on 4K with a 2-second GOP on this 15 W iGPU, and KEY_UNIT during scrubber drag (the spec's hybrid policy) covers it at 37 ms median.

KEY_UNIT worst-case outliers on the 12 GB file (~270–380 ms) are almost certainly cold disk reads when jumping hours into a file much larger than the page cache; the medians are single-digit.

## Findings that change the design

### 1. Zero-copy decides seek latency, not only throughput

The same file, the same hardware decoder, the same seeks:

| Output of the decoder | ACCURATE median / worst, 2 s GOP 1080p60 |
|---|---|
| System memory (e.g. into a plain `fakesink`) | 447 / 784 ms — **fails** |
| VA memory or DMABuf into GL | 92 / 152 ms — **passes** |

An accurate seek decodes forward from the previous keyframe — up to one GOP of frames — and when the decoder's output is system memory, **every one of those frames is copied off the GPU**, even though all but the last are discarded. That copy was 80% of the cost. The spec's rule "decoded video never enters a Rust-owned CPU buffer" was motivated by throughput; it is equally a seek-latency requirement.

### 2. `decodebin3` is required; `decodebin` is not an acceptable fallback

Decode + upload throughput at 1440p, audio consumed identically in each path, startup excluded:

| Path | Throughput | Caps into GL |
|---|---|---|
| `decodebin` | 117 fps | **system memory** |
| `decodebin3` | 739 fps | DMABuf |
| explicit `qtdemux ! h265parse ! vah265dec` | 722 fps | DMABuf |

`decodebin` auto-plugs the same hardware decoder but negotiates system memory into `glupload` — ~6× slower, and (per finding 1) ~5× slower to seek. The spec named it as the fallback for `decodebin3`; that is removed.

### 3. On this distro, hardware decoders already win by default

`vah265dec` and `vah264dec` are ranked **PRIMARY + 1 (257)**, above software `avdec_*` at PRIMARY (256), so `decodebin3` selects hardware with no intervention. The spec assumed the opposite ("on most distro builds the software decoders outrank the hardware ones"). The decoder probe stays as a safety net for other distros and older GStreamer, but the pessimism was wrong for Ubuntu 24.04 / GStreamer 1.24. (The legacy `vaapih265dec` is present at rank NONE — correctly ignored.)

### 4. The libmpv fallback would not have fixed a slow result

When a seek is slow *with* hardware decode and zero-copy, the cost is decoding one GOP of frames. libmpv with `hwdec=vaapi` uses the same VA-API decoder and must decode the same frames. The spec's kill criterion pointed at the player; the variables that actually matter are **GOP length × per-frame decode cost × whether the output stays on the GPU**.

## Traps hit while measuring — each is also an implementation trap

1. **A rejected seek looks like a fast seek.** The first gate script never checked `seek_simple()`'s return value. It happened to report correct numbers, but could not have told the difference.
2. **`decodebin3 ! fakesink` links whichever pad appears first — often audio.** One run timed *audio* seeks, printed 775 `gst_buffer_pool_acquire_buffer` CRITICALs from the unlinked video decoder, and produced an impossible 6 ms accurate seek on a 120-frame GOP. It briefly looked like a `decodebin3` bug; it was a missing `video/x-raw(ANY)` caps filter. The app must select the video stream explicitly.
3. **Measuring into a system-memory sink overstates seek latency ~5×** (finding 1). The opposite trap: **a `fakesink` after `glupload` lets it pass DMABuf through without importing**, so "DMABuf into GL" can look fine while the real GL import would copy. Force `GLMemory` caps downstream and check the uploader name (Correction 2). The first real-hardware numbers for the 2 s GOP file said "fails even on hardware"; that was the download, not the decode.
4. **Synthetic x265 files carry a 2-frame PTS offset** (B-frame delay with no edit list), so every accurate seek on them lands exactly 67 ms past the target. A property of the fixture, not of seeking; real footage lands within one frame.

## Correction 2 — the first laptop "zero-copy" results measured passthrough, not GL import

The first laptop pass ended its pipelines in a plain `fakesink` after `glupload ! glcolorconvert`. Nothing downstream required GL memory, so `glupload` chose **`Dmabuf Passthrough`**: it forwarded the DMABuf without importing it into GL at all (`GST_DEBUG=glupload:6`). "Caps into GL: `memory:DMABuf`" was true only for a pipeline that never touched GL. The Phase 2 review caught it.

Re-measured with the app's real sink requirement, `video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D`, which forces a real import:

| Decode path | GL platform | Caps into `glupload` | Uploader | Steady-state throughput (1440p HEVC) |
|---|---|---|---|---|
| `decodebin3` | **EGL** | `memory:DMABuf` | `DirectDmabufExternal` (zero-copy import) | **651 fps** |
| `decodebin3` | GLX (**the default on X11**) | `NV12` system memory | `Raw Data` (CPU copy) | 58 fps |
| `decodebin` | EGL | system memory | `Raw Data` | 61 fps |
| `decodebin` | GLX | system memory | `Raw Data` | 62 fps |

**Missing either requirement costs ~11×.** On this X11 laptop GStreamer defaults to GLX, and GStreamer 1.24's DMABuf importer needs EGL — so without an explicit EGL context the "hardware" path silently copies every frame through the CPU. In the app, the EGL context is Slint's (Skia renderer), shared with GStreamer; see the Phase 2 spec.

Seek latency through the real import path under EGL is unchanged from the table above — camera footage **8.8 / 20.5 ms**, 2 s-GOP 1080p60 **88.5 / 152 ms** — because an accurate seek's cost is decoding forward from the keyframe, and importing the one landed frame is cheap. Under GLX the camera footage seeks in 41 / 54 ms: still within budget, but ~5× slower.

The earlier throughput table (`decodebin` 117 vs `decodebin3` 739 fps) measured **decode alone** (passthrough) and is superseded by this one for any claim about the display path.

## Correction 1 — the earlier container-only spike

An earlier version of this document, measured on a GPU-less CI container with software decode, concluded that KEY_UNIT's ~105 ms was a "hardware-independent floor" leaving hardware only ~145 ms of headroom. **That was wrong.** KEY_UNIT still decodes the keyframe itself, and in software that is most of the cost; on hardware, KEY_UNIT is 2–6 ms. The software numbers themselves reproduced on the laptop almost exactly (container 440 / 617 ms, laptop 450 / 620 ms), which is a useful cross-check that both benches measured the same thing.

## Reproducing

`scripts/linux-gate-check.sh <file>` — sections 1–4 report decoder ranks, encoders, the GL mixer chain and capture; section 5 reports GOP length, the selected decoder, whether frames reach GL zero-copy, and KEY_UNIT / ACCURATE latency through the GL path.
