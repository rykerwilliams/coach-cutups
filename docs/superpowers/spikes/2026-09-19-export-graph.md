# Spike: the export graph on real hardware

**Date:** 2026-09-19
**Question:** Does the spec's hybrid export graph work, and is it zero-copy? A decode pipeline's buffers are pushed unchanged, with only the PTS restamped, into a GL + encoder pipeline. The spike also covers freeze semantics, sub-pixel zoom, encoder choice and quality, frame identity, and seek versus pull-forward decoding.
**Answer:** **Yes. The hybrid graph works zero-copy under EGL and exports the user's footage at ~108 fps (3.6× realtime).** The encoder side is the bottleneck, not decode or the pump. Two findings change the spec:

- the only hardware encoder on this machine does **constant-QP only**, so quality must be expressed as QP, not bitrate;
- a source time has to be compared with the buffer's **stream time**, not its raw PTS. With raw PTS, an edit-listed file exported every frame two frames late.

## Machine

The same laptop as the seek-latency spike:

- Intel Core i7-10610U (Comet Lake), UHD Graphics;
- Ubuntu 24.04, kernel 6.8, GStreamer 1.24.2, X11;
- VA driver: `intel-media-va-driver` 24.1.0+dfsg1, the free build.

Bench: a scratch gstreamer-rs 0.25 program, plus ffmpeg for the quality measurements. It is not committed. Its structure matches the spec's frame driver (see "Reproducing").

**Test inputs:**
- **Camera footage:** the user's HEVC 2560×1440 @ 30 file, with a 0.5 s GOP and no B-frames.
- **Fiducial file:** a generated H.264 1920×1080 @ 60 file with a 2 s GOP and 3 B-frames. It has a 16-bit frame counter burned in as black/white blocks (ffmpeg `drawbox` with `enable='mod(floor(n/2^b),2)'`).
- **Pan fixture:** a 2560×1440 blurred-noise still, for the pan test.

**Workload:** 60 s of output made of four 15 s "clips". Each clip is `freeze 1 s` (the no-predecessor freeze), then `(play 2 s, freeze 1 s) ×4`, then `play 2 s`. Source time jumps 37 s between clips, which gives 4 seeks and 1,204 decoded frames for 1,800 output frames. The clock starts at the first push, after the first seek.

## The graph measured

```
decode:  filesrc ! parsebin ! vah265dec ! video/x-raw(memory:DMABuf) ! appsink sync=false max-buffers=2
            (appsink answers the ALLOCATION query with GstVideoMeta — see finding 3)

pump:    frame_at(source_time) → buffer.copy() → set pts = n/30, dts = NONE, duration = 1/30 → appsrc.push

encode:  appsrc format=time block=true max-buffers=4
         ! glupload ! glcolorconvert ! video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D
         ! gltransformation ortho=true ! video/x-raw(memory:GLMemory),width=1920,height=1080
         ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 ! gldownload ! video/x-raw,format=NV12
         ! queue max-size-buffers=4
         ! vah264lpenc rate-control=cqp qpi=Q qpp=Q qpb=Q key-int-max=60
         ! h264parse ! video/x-h264,stream-format=avc,alignment=au ! mp4mux faststart=true ! filesink
```

Both pipelines share one EGL `GLDisplay` + `GLContext`, supplied through `need-context` as the player does. Here that context is GLES2 on an X11 EGL display.

## Results: throughput (60 s of output, HEVC camera footage)

| Variant | fps | Notes |
|---|---|---|
| Decode + pump + GL, ending in GL memory (no download, no encode) | **303** | Ceiling for everything before the encoder |
| … + `gldownload` NV12 → fakesink | 232 | GPU→CPU readback of the 1080p output |
| … + `vah264lpenc`, **no queue** before the encoder | 71 | Download and encode serialized on one thread |
| … + `queue` + `vah264lpenc` QP 24 (**recommended**) | **108** | 3.6× realtime; 68 MB (9 Mbps) |
| same, play-only (no freezes) | 104 | 90 MB |
| same, freeze-only (1 frame held for 60 s) | 112 | 4.7 MB |
| Boundary in **GL memory** (`glupload ! glcolorconvert` in the decode pipeline) | 106 | Equivalent; see "Boundary choice" |
| Boundary in **system memory** (`video/x-raw,format=NV12` from the decoder) | 72 | Maps VA surfaces through the CPU |
| DMABuf boundary under **GLX** | fails | `not-negotiated`, and a blocking `appsrc` then hangs; see finding 4 |
| One pipeline, linear play (`decoder ! [queue] ! GL ! queue ! vah264lpenc`) | 68–70 | Not faster; structurally unsuitable anyway (see below) |
| x264enc `medium` 12 Mbps, same graph | **11.8** | 0.39× realtime |
| x264enc `veryfast` 12 Mbps | 36 | 1.2× realtime |

**The frame pump is not the cost.** Pulling decoded frames, including decode time not hidden by the pipeline, totalled 0.03–0.35 s out of 16 s. The encode side sets the rate: GL readback (232 fps alone) plus VA encode (~225 fps alone at 1080p) contend for the same GPU.

**The queue before the encoder is required**: without it, fps drops from 108 to 71.

**A GL→VA DMABuf handoff is not available.** `gldownload` offers `memory:DMABuf`, but `vah264lpenc` only accepts DMA_DRM `NV12:0x0100000000000002` (Y-tiled) and the link fails. So the encoder's input is a CPU readback of the finished 1080p frame (3 MB), not of the decoded source. That is GStreamer-owned and measured at ~4 ms/frame. It does not violate the "decoded video never enters a Rust-owned buffer" rule.

## Findings that change the design

### 1. VA post-processing is not available on this machine, so there is no GL-free path

`vapostproc` and `vaapipostproc` are absent. `vainfo` lists no `VAEntrypointVideoProc` on Ubuntu's free iHD build; `intel-media-va-driver-non-free` is packaged but not installed. The spec's VA-native alternative (`vapostproc` crop/scale → `vah264lpenc`) cannot be built on the reference laptop with stock packages, so **the GL graph is the design, not one option of two**.

### 2. The hardware encoder is CQP-only, so quality is a QP, not a bitrate

- `vah264lpenc` exposes only `rate-control=cqp`, the driver's advertised set. There is no `vah264enc`, because the driver has only the low-power `EncSliceLP` entrypoint.
- The legacy `vaapih264enc tune=low-power` lists CBR and VBR in its enum, but fails at runtime with `unsupported CBR rate control` / `unsupported VBR rate control`.
- `target-usage` (1/4/7) changed neither size nor quality.

**Quality at 1080p**, on 30 s of play-only camera footage, measured against a lossless reference rendered through the same GL chain:

| Encoder | Setting | Mbps | PSNR-Y | SSIM-Y | fps |
|---|---|---|---|---|---|
| vah264lpenc | QP 18 | 31.8 | 46.8 | 0.988 | 100 |
| vah264lpenc | QP 22 | 15.4 | 43.6 | 0.981 | 107 |
| vah264lpenc | QP 26 | 8.4 | 41.4 | 0.974 | 110 |
| vah264lpenc | QP 30 | 4.9 | 39.3 | 0.965 | 110 |
| x264 medium | 6 Mbps | 5.2 | 39.5 | 0.970 | 12.7 |
| x264 medium | 12 Mbps | 10.9 | 41.9 | 0.979 | 9.6 |
| x264 medium | 24 Mbps | 22.9 | 44.6 | 0.987 | 7.2 |
| x264 veryfast | 6 Mbps | 5.3 | 39.1 | 0.969 | 34 |
| x264 veryfast | 12 Mbps | 11.3 | 41.6 | 0.979 | 28 |

- **VA vs x264 medium:** at equal PSNR, VA needs roughly 15–30% more bits than x264 medium.
- **VA vs x264 veryfast:** VA roughly matches veryfast.
- **Speed:** VA is 10× faster than x264 medium.

**Freezes make the mean bitrate content- and edit-dependent.** The mixed 60 s export at QP 24 averaged 9 Mbps; the same QP on play-only averaged 12 Mbps. A fixed QP therefore cannot promise the spec's 6/12/24 Mbps ladder. It can promise a quality level.

**Recommendation:** express the export presets as a quality level:

| Preset | VA QP | x264 equivalent |
|---|---|---|
| low | 28 | `bitrate=6000` |
| medium | 24 | `bitrate=12000` |
| high | 20 | `bitrate=24000` |

- These QPs land near the ladder's bitrates on this footage (QP 22 ≈ 15 Mbps, QP 26 ≈ 8 Mbps).
- Keep `ExportSettings.bitrate` only for the software fallback, or drop it.
- Make the x264 fallback **`veryfast`, not `medium`**: medium runs at 0.39× realtime on this laptop, so a 10-minute export would take ~26 minutes, and it buys ~0.3 dB.

### 3. The decode `appsink` must advertise `GstVideoMeta`, or DMABuf will not negotiate

With `appsink caps=video/x-raw(memory:DMABuf)`, `vah265dec` fails with `DMABuf caps negotiated without the mandatory support of VideoMeta` and posts `not-negotiated`. The fix is `AppSinkCallbacks::propose_allocation(|_, q| { q.add_allocation_meta::<VideoMeta>(None); true })`. DMA_DRM buffers need the meta for plane offsets and strides.

### 4. The boundary caps must be chosen for the GL platform; nothing negotiates across it

In one pipeline, a decoder that cannot hand DMABuf to GL silently falls back to system memory. Across an `appsink`/`appsrc` boundary nothing negotiates end to end.

- **Under GLX:** the DMABuf boundary is simply `not-negotiated`, and a blocking `appsrc` push then waits forever. The implementation must watch the encode bus for errors while pushing; do not rely on `push_buffer`'s return value.
- **Under EGL:** the uploader is **`DirectDmabufExternal`** on every frame (`GST_DEBUG=glupload:6`, 450/450 frames), the real zero-copy import. The shallow `buffer.copy()` keeps the memory as `dmabuf`.

### 5. Compare source times with stream time, not raw PTS

The fiducial file, like any ffmpeg/x264 MP4 with B-frames, carries an edit list. `qtdemux` turns it into a segment with `start = 0.0333 s` and `time = 0`: every buffer's PTS runs 2 frames (33 ms) ahead of its presentation time. The player's position query and the seek API both use stream time.

- **Raw PTS:** the pump compared source times with raw PTS and chose the frame two earlier on **1,676 of 1,800** output frames. It also re-seeked on every freeze frame (125 seeks instead of 4), because the frame an accurate seek landed on appeared to be *later* than the target.
- **Stream time:** with `segment.to_stream_time(pts)`, **1,800 of 1,800 frames were exact**. That held for anchors on a frame boundary (5.0 s), between frames (5.01 s), and on the f64 value of a 60 fps boundary (5.016666666666667 s), with both the DMABuf and the GL-memory boundary.

The user's camera files have no edit list and no B-frames, so PTS equals stream time for them. Phone and editor-exported files usually do have one. The seek-latency spike's "synthetic x265 files carry a 2-frame PTS offset" is the same effect, seen from the other side.

A second trap: after an ACCURATE seek, the first buffer's timestamp is **clipped to the seek target** (its stream time read 5.010 s for the frame at 5.000 s). Its content is the correct frame. Do not use that first buffer's timestamp to identify the frame; compare later frames as usual.

### 6. Convert f64 seconds to nanoseconds by rounding, never with `as u64`

Anchors are stored as f64 seconds. For every frame of two hours of 30 or 60 fps timestamps, as `qtdemux` computes them:

- `(s * 1e9) as u64` gives back **pts − 1 ns for 1.7–1.9% of frames**. "The last frame with PTS ≤ s" then selects the *previous* frame, which is exactly the "drawings one motion step behind" bug.
- `.round()` and `Duration::from_secs_f64` / `ClockTime::from_seconds_f64` round-trip all of them.

`crates/video-coach-media/src/player/mod.rs:442` and `player/tests.rs:462` use the truncating form today.

## Q2: freeze semantics

Re-pushing one held buffer with a new PTS each time works:

- `vah264lpenc` accepted 1,800 identical frames, encoding them as near-empty P-frames plus an I-frame per GOP (4.7 MB per 60 s at QP 24 vs 90 MB for play).
- `glupload` re-imports the same DMABuf each time without complaint.
- The fiducial output shows every frozen frame is the correct source frame.

**Cost:** freezes save decoding but not encode-side work. Every output frame still goes through GL, download and encode, which is why freeze-only (112 fps) is barely faster than play (104). Holding one decoded surface does not starve the decoder's pool.

## Q3: sub-pixel zoom

The test was a 20 s pan at scale 3 over the noise fixture. Per-frame displacement was measured by phase correlation with a parabolic sub-pixel peak.

| Path | Mean step | Step std | Steps < 0.1 px | Deviation from a straight path |
|---|---|---|---|---|
| `gltransformation` (float scale and translation) | 0.98 px | **0.005 px** | 0% | rms **0.018 px**, max 0.044 |
| `videocrop` (integer) + `videoscale`, the CPU fallback | 0.46 px | 0.88 px | **79%** | rms 0.62 px, max 1.07 |

`gltransformation` is smooth; integer crop stair-steps: 2.14 px jumps separated by 3–4 still frames. The spec's acceptance test passes on the GL path.

**Calibration:** with `ortho=true`, `translation-x` is a **fraction of the output width**, applied after scaling about the centre (`-0.1` moved the image 192 px at 1920 wide). So for a letterboxed source filling width `dw` of output width `W`: `translation-x = -pan_x · scale · dw / W`. The Y sign is not verified here; GL's Y axis may be inverted, so test it in the implementation.

Apply zoom from a **buffer probe on `gltransformation`'s sink pad, keyed on the buffer's PTS**, not from the pump thread. With queues in the graph, a property set by the pump would land on whichever frame GL happens to be processing.

## Q6: seek vs pull-forward

Measured in the decode pipeline alone (DMABuf appsink, no copies). Medians over 8 start positions:

| Distance ahead | HEVC 1440p30, 0.5 s GOP: pull / seek | H.264 1080p60, 2 s GOP: pull / seek |
|---|---|---|
| 1 frame | 2.4 / 12.8 ms | 4.3 / 105 ms |
| 0.25 s | 10.8 / 12.3 ms | 19.6 / 105 ms |
| 0.5 s | 21.5 / 12.4 ms | 38.2 / 84 ms |
| 1 s | 42.0 / 13.7 ms | 75.2 / 61 ms |
| 2 s | 83.0 / 11.8 ms | 151 / 99 ms |

- **Cost per frame:** pulling forward costs ~1.3 ms per frame.
- **Cost per seek:** an accurate seek costs about one partial GOP (worst case 20 ms and 147 ms).
- **Break-even:** ≈ 0.25 s on the camera footage and ≈ 1.2 s on the 2 s-GOP file. The spec's ~2 s threshold is too generous for short-GOP footage (83 ms of pulling vs a 12 ms seek). Pulling forward is never catastrophic, though.

**Recommendation:** threshold ≈ 0.5 s. It hardly matters in practice: freezes consume no source time, so within a clip the source is contiguous. The pump only faces the choice at clip boundaries and skips, tens per export, and at most ~100 ms each.

## Boundary choice: DMABuf vs GL memory

Both boundaries are zero-copy and exact, and the GL-memory boundary (106 fps) performs the same as DMABuf (108 fps).

The GL-memory boundary puts `glupload ! glcolorconvert` in the decode pipeline, **which is the player's `SinkKind::Gl` video sink verbatim**:

- it negotiates DMABuf-or-system by itself, so under GLX it degrades instead of failing;
- it removes the VideoMeta requirement (finding 3);
- it needs both pipelines on one GL share group, which the app provides anyway.

A held freeze frame then costs a 14.7 MB RGBA texture instead of a decoder surface; irrelevant.

**Recommend the GL-memory boundary.** The spec's "push the GstBuffer unchanged" still holds: the pushed buffer is GL memory, restamped.

## Rejected: one pipeline with a restamping pad probe

- **Freezes:** a probe can pass, drop or modify a buffer, but not multiply it. Emitting N copies means pushing from inside a probe on the streaming thread.
- **Seeks:** a flushing seek flushes the encoder and `mp4mux` mid-file.
- **Speed:** measured on linear play, it was not faster anyway (68–70 fps vs 104–108 for the hybrid). The cause was not isolated.

The hybrid's decode/encode split is what makes seeks harmless.

## Recommended graph

```
decode (per source):  filesrc ! parsebin ! <hw decoder> ! glupload ! glcolorconvert
                      ! video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D ! appsink sync=false max-buffers=2
                      (= the player's GL video sink; shared GL context)

pump:                 frame_at(stream-time) — last frame with stream_time ≤ s; seek if behind or > ~0.5 s ahead
                      buffer.copy(); pts = n·1s/30 (integer), dts = NONE, duration = 1s/30; push

encode:               appsrc ! gltransformation ortho=true [zoom from a sink-pad buffer probe, keyed on PTS]
                      ! …1920×1080 GLMemory ! (glvideomixer with cam + overlay pads — not measured here)
                      ! glcolorconvert ! NV12 GLMemory ! gldownload ! queue
                      ! vah264lpenc rate-control=cqp qp{i,p,b}=<preset QP> key-int-max=60
                        | x264enc speed-preset=veryfast bitrate=<ladder>   (fallback)
                      ! h264parse ! video/x-h264,stream-format=avc,alignment=au ! mp4mux faststart=true ! filesink
```

Watch the encode bus for errors while pushing.

## Caveats

- **Mixer not measured.** `glvideomixer`, the PiP and overlay pads were not in the graph. They add GL work before the download, so expect somewhat less than 108 fps.
- **Letterbox only on the same aspect.** Letterboxing was only exercised 16:9 into 16:9.
- **Rough quality metric.** PSNR/SSIM are a rough proxy, not VMAF. The reference went through the same GL scaler, so the numbers isolate the encoder.
- **Other drivers differ.** Rate-control support is driver-specific. The non-free iHD driver, AMD (Mesa) or newer Intel may offer CBR/VBR and VPP. The CQP-only conclusion is about this machine and the stock Ubuntu driver, so probe `rate-control` at runtime.
- **Single-pipeline cause unknown.** The single-pipeline throughput gap was not investigated.

## Reproducing

The scratch bench is a Rust program with three commands:

- `export`: runs the decode pipeline, the pump and the encode pipeline, with the segment plan above;
- `seekcost`: measures seek against pull-forward;
- `single`: the one-pipeline variant.

Also used:

- the fiducial generator: ffmpeg `testsrc2` plus 16 `drawbox` bits, encoded with `libx264 -g 120 -bf 3`;
- readback: ffmpeg decodes to `gray` and the bench samples the centre of each block.

Check the uploader with `GST_DEBUG=glupload:6`: every frame must say `DirectDmabufExternal`.
