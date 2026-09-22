# Vision layer research: auto match events + player highlights

**Date:** 2026-09-22 · **Read-only research.** No repo files changed, no cargo builds, nothing installed, no models downloaded.
**Local checks run** (tiny, all verified on this machine): `lscpu`, `gst-inspect-1.0`, `apt-cache policy`, and one 30-frame 160×90 `mp4mux` file in the scratchpad to test in-place chapter insertion (§5).

Labels used below: **[verified]** = checked on this machine or in source. **[cited]** = from a named source, not reproduced here. **[estimate]** = my arithmetic, not measured. Treat every estimate as a hypothesis for an S0-style spike.

---

## 0. What the repo and the machine actually give us

**Machine** [verified]
- **CPU:** i7-10610U, 4c/8t, max 4.9 GHz. It has **AVX2 + FMA but no AVX-512 and no VNNI**, so INT8 quantization gains less here than vendor numbers suggest.
- **GPU:** `CometLake-U GT2 [UHD Graphics]`, Gen9.5, 24 EU, `/dev/dri/renderD128`.
- **Vulkan** (Mesa ANV, `intel_icd.json`, mesa 25.2.8) is present.
- **OpenCL for Intel is NOT installed:** only `ocl-icd-libopencl1`, and no `/etc/OpenCL/vendors`. `intel-opencl-icd 23.43` is available from the Ubuntu archive. The OpenVINO GPU plugin needs it, so the iGPU path means a new system package.
- **Ubuntu 24.04 packages:** no `onnxruntime` or `openvino` package in the archive. `libopencv-dev` is 4.6.0.

**GStreamer 1.24.2** [verified via `gst-inspect-1.0`]
- **Inference elements:** none installed (`onnxinference`, `ssdobjectdetector`, `gvadetect`, `tensordecodebin`, `openvino` are all absent).
- **`analyticsoverlay` / `objectdetectionoverlay`** (plugins-bad) are installed. They only *draw* `GstAnalytics` metadata; they don't produce it.
- **`mp4mux`** implements `GstTagSetter`, `GstTagXmpWriter` and `GstPreset` only, **no `GstTocSetter`**, so it has no chapter support.
- **No `vapostproc`**, so GPU scaling means GL elements (`glcolorscale`, `gltransformation`), which the app already uses.
- **Audio:** `spectrum`, `level`, `audiocheblimit` and `audiochebband` exist (plugins-good).

**Repo facts that shape the design** [verified]
- **Decode throughput:** decode alone is 739 fps (`decodebin3`, DMABuf) and 651 fps through EGL (`spikes/2026-09-19-seek-latency.md`). GL readback is about 232 fps at 1080p (`spikes/2026-09-19-export-graph.md`). Decode plus a GPU downscale plus a small `gldownload` is **not** the bottleneck of any vision pass. Inference is.
- **Match events** (Phase 9): `MatchEventKind { StartStop, HomeGoal, AwayGoal }`, positioned by `source_index + source_seconds` (`crates/video-coach-core/src/scoreboard.rs:137-160`).
  - **A post-goal kick-off is not an event in this model.** Only period starts and stops are. "Detect kick-offs" therefore means period boundaries (2–4 per match), plus kick-offs used as *evidence* for goals.
  - **Goals must be attributed Home/Away.** Audio can never do that. Scoreboard OCR can.
- **Coordinate spaces:** strokes are normalized to the **picture (content) rect in output space** (`crates/video-coach-media/src/overlay.rs:7-14`) and keyed by **record time** (`stroke_replay::visible_strokes(clip, at_record_time)`). A player highlight is a different kind of object: its position is in **source-frame space** and keyed by **source time**. It must go through the zoom transform (`core::zoom`) to reach the picture. Keying by the displayed frame's source time (`FrameSpec::source_time`, which `scoreboard.rs` already uses) makes a highlight freeze correctly when the source freezes during a commentary pause.
- **Phase 10 precedent:**
  - whisper-rs vendors whisper.cpp and has no feature gate.
  - One worker thread per job and a serial FIFO queue on the bus thread.
  - `Drop` cancels without joining.
  - A one-method seam keeps the queue tested on CI without a model.
  - `small.en` measured **0.73× realtime** (`spikes/2026-09-21-whisper-throughput.md`).
- **Phase 11 S3 precedent:**
  - The model downloads on first use, after a prompt, via `souphttpsrc ! filesink`.
  - The sha256 comes from `glib::Checksum` in a pad probe, with `.part` then rename.
  - The files land in `$XDG_CACHE_HOME/coach-cuts/models/`, and a sha mismatch deletes the file and fails.
  - A vision model can reuse this unchanged. Only the model table and the hashes are new.
- **Phase 10's 16 kHz mono audio Reader** (`composite/audio.rs` `Reader`, extended for transcription) covers the whistle band (2–4.5 kHz, below the 8 kHz Nyquist limit). Audio detection needs no new decode path.

---

## 1. Player detection + tracking on this laptop (CPU/iGPU only)

### 1a. Throughput: this is an offline pass, never live

**No published benchmark exists for this exact chip.** The closest data:

| Source | Hardware | Model | Latency |
|---|---|---|---|
| Ultralytics YOLO11 docs [cited] | "CPU ONNX" (server CPU; Ultralytics measures on EC2) | YOLO11n @640 (6.5 GFLOPs) | **56 ms** |
| same | same | YOLO11s @640 (21.6 GFLOPs) | 90 ms |
| Frigate hardware docs [cited] | **HD 620** (Gen9, 24 EU, same class as this UHD) | YOLOv9 @320 | **~35 ms** |
| same | HD 620 | MobileNetV2 SSD | 15–25 ms |
| same | Iris Xe (Gen12, 96 EU) | YOLOv9-t @640 | 14 ms |
| Ultralytics OpenVINO docs [cited] | Core Ultra X7 358H | YOLO26n OpenVINO FP32 | 4.1 ms (irrelevant class of hardware) |

**[estimate] for this laptop, FP32, a YOLO11n / D-FINE-N-class model (~6–7 GFLOPs @640):**
- **CPU (ORT or OpenVINO-CPU, 8 threads, on AC):** ~40–90 ms per 640×640 inference, so **11–25 fps**. At 1280 input it is ~4× that, **3–6 fps**. A 15 W chip throttles over minutes, so pin the conditions as Phase 10 S0 did.
- **iGPU (OpenVINO GPU, FP16):** Frigate's HD 620 figure of ~35 ms @320 scales to roughly **100–150 ms @640**. **The Gen9 GT2 iGPU is not faster than the CPU for this.** It would also contend with `vah265dec` and GL on the same GPU. It needs `intel-opencl-icd` installed, and Intel moved Gen9–Gen11 to the frozen `legacy1` compute-runtime branch (24.35) [cited: intel/compute-runtime LEGACY_PLATFORMS.md]. OpenVINO's model server has an open "ProgramBuilder build failed" crash on this exact GPU (openvino model_server #3635, an LLM workload) [cited].
- **Verdict:** **target the CPU and skip the iGPU.** The iGPU adds a system dependency on a frozen driver branch for no expected gain.

**Small players are the real problem, more than FLOPs.**
- **The size problem:** in a 2560×1440 sideline wide shot a player is roughly 40–100 px tall. Downscaled to 640 wide, that is **10–25 px**. Nano/small models reach only ~25% recall on ~32 px objects at 640 input [cited: small-object blog, joelhuang.dev].
- **What it forces:** full-frame detection needs 1280 input or 2×2 tiling (SAHI-style), which costs 4×. That means **roughly 3–6 fps, about 0.1–0.2× realtime.**
- **What that rules out:** whole-match full-frame detection is 90 min × 30 fps = 162k frames. Even at 1 fps sampling with 4 tiles it is **~25–30 min per match** [estimate]. **Don't build any feature that needs whole-match player detection on phone footage.**

**Click-to-track sidesteps both problems:**
- **Crop, don't downscale:** the coach's click gives a position, so each step runs the detector on a **native-resolution 640×640 crop around the predicted position**. At native resolution the player is 40–100 px, well inside the model's range.
- **One inference per frame:** at ~40–90 ms that is **~0.4–0.8× realtime**, so a 10 s highlight takes ~12–25 s of analysis. Detecting every 2nd frame and interpolating with a Kalman filter halves it. Tolerable **as an offline job with progress**, like transcription. **Not live under the pen.**
- **Other people in frame:** COCO "person" also detects referees, coaches and parents in the foreground of a sideline shot. The click, and a crop around the target, make that mostly irrelevant.

### 1b. Tracking approach (single clicked target, not full MOT)

The feature tracks **one clicked player per highlight**. Full multi-object tracking (ByteTrack/BoT-SORT) solves a bigger problem than we have. Options:

1. **Detector on ROI + Kalman + association: recommended.** Predict the box with a constant-velocity Kalman filter, detect in the crop, and pick the detection with the best IoU/distance to the prediction. Optionally break ties with a jersey-colour histogram so the tracker keeps to the right team. This is SORT reduced to one track, about **150–250 lines of pure logic**: a 7- or 8-state Kalman filter plus a gating rule.
   - It can live in **`video-coach-core`**, because it operates on boxes, not pixels, and can be tested on CI with synthetic box sequences.
   - Crates exist if wanted: `jamtrack-rs` (ByteTrack, BoT-SORT, OC-SORT, pure Rust), `trackforge`, `similari-trackers-rs`, `edgefirst-tracker`, `mot-rs` [cited: crates.io/lib.rs]. Their licences weren't checked individually; check them before adopting. Writing the one-track version is simpler than adopting a multi-object tracking framework.
2. **Single-object tracker (SOT), no detector:** CSRT/KCF (OpenCV contrib), or NanoTrack/VitTrack (tiny ONNX models of ~1–2 MB, cheap per frame).
   - Ubuntu 24.04's OpenCV is **4.6.0**, which predates `TrackerNano` (4.7) and `TrackerVit` (4.9). The OpenCV route means a heavy C++ dependency, and still an old tracker.
   - SOTs **drift and switch identity** when same-kit players cross, which in football is the common case, not the edge case.
3. **Recommended hybrid: detector-in-ROI tracking plus coach corrections.** No tracker survives 20 s of a youth-football scrum. **The UX has to treat the track as a suggestion the coach can correct:**
   - scrub, re-click on the right player, and the track re-runs from that keyframe;
   - the stored result is a list of keyframed boxes, with every automatic box replaceable.
   - This matters more to the feature's success than the choice of model.

**Data shape** (pure, core):
- A `PlayerHighlight { id, label: Option<String>, color: Rgba, source_index, samples: Vec<(source_seconds, NormRect)>, corrections: Vec<…> }` stored in `project.json`, with a **formatVersion bump**. Note `store::read`'s exact-version guard (Phase 10 S3 amendment).
- Preview and export **only read stored samples**: interpolate at `FrameSpec::source_time`, map through `Zoom` into the picture rect, and rasterize an ellipse or ring in `overlay.rs`.
- **No inference ever runs in the preview or export path.** Export stays deterministic and its throughput unchanged: the overlay is already ~3.6 ms per frame, 9× realtime (`spikes/2026-09-19-compositing-throughput.md`).

### 1c. Runtimes usable from Rust

| Runtime | Licence | Notes for this project |
|---|---|---|
| **`ort`** (pyke) → ONNX Runtime | crate MIT/Apache-2.0; ORT MIT | Still `2.0.0-rc.13` [cited: ort.pyke.io]. The default `download-binaries` fetches a prebuilt ORT **at build time**, CPU EP only; pyke's prebuilts don't list OpenVINO [cited]. The OpenVINO EP means building ORT from source. ORT isn't in the Ubuntu archive, so packaging has to bundle `libonnxruntime.so` (~20 MB) or link statically. Fastest mature CPU path. |
| **`openvino`** crate (intel/openvino-rs) | Apache-2.0 | Supports `runtime-linking` (dlopen), so it **builds with no OpenVINO present** [cited]. OpenVINO-CPU is typically faster than ORT-CPU on Intel. But the runtime is a large non-distro install from Intel's apt repo or a tarball (~100+ MB). Heavy for a .deb. |
| **`rten`** (+ `ocrs`) | MIT/Apache-2.0 | **Pure Rust, AVX2 SIMD**, no native deps, no build-time download. `rten-examples` includes **YOLO, DETR/RT-DETR, TrOCR, Silero VAD, Whisper** [cited]. No published speed comparison with ORT, so **measure it** (likely slower). Best fit for this repo's build-simplicity values *if* it is fast enough. |
| `tract` (Sonos) | MIT/Apache-2.0 | Pure Rust, solid for small CNNs. No YOLO-class numbers found; unverified. |
| `candle` / `burn` | MIT/Apache-2.0 | candle has a YOLOv8 example. burn's `wgpu` backend could reach the iGPU through **Vulkan (ANV is present)** without OpenCL. Neither has credible conv-throughput numbers for this hardware; unverified. |
| OpenCV `dnn` (`opencv` crate) | Apache-2.0 (OpenCV), MIT (crate) | Distro packages exist (4.6), but it's a big C++ dependency plus bindgen. Only worth it if CSRT is also wanted. Not recommended. |

**Recommendation:** measure **`rten` against `ort` (CPU EP)** on one permissive model in an S0 spike, then pick. Either way it is a plain dependency in `video-coach-media` (or a new `video-coach-vision` crate), **never in core**. The model is downloaded on first use via the Phase 11 S3 downloader. At these sizes (4–40 MB) **bundling in the .deb is also viable**, which settles BACKLOG #22's question more cheaply for vision than for whisper.

### 1d. Model licences

| Model | Licence | Params | COCO AP | Verdict |
|---|---|---|---|---|
| Ultralytics YOLOv8 / YOLO11 / YOLO26 | **AGPL-3.0**, code **and** weights; Enterprise licence otherwise [cited: ultralytics.com/license] | 11n: 2.6 M | 11n: 39.5 | **Legally compatible** with AGPL-3.0-or-later: the combined work ships as AGPL-3.0, and Ultralytics' condition (open the whole project, including weights) is already met. The cost is **lock-in**: Coach Cuts could never be relicensed or dual-licensed without buying an Enterprise licence, and fine-tuned weights inherit AGPL. `ultralytics/inference` (a Rust ORT wrapper with an OpenVINO feature) is AGPL too. **Usable, but not preferred.** |
| **D-FINE-N / DEIM-D-FINE-N** | Apache-2.0 (per repo; re-verify at pin) | ~4 M | ~43 | **Preferred:** permissive, CNN backbone, similar FLOPs to YOLO11n with better AP. |
| RT-DETR (lyuwenyu) R18 | Apache-2.0 | 20 M | 46.5 | Heavier; OK as a fallback. |
| RF-DETR N/S/M/L | Apache-2.0; **XL/2XL are "PML 1.0"**, not open [cited: roboflow/rf-detr] | **30.5 M** even for Nano (DINOv2 ViT) | 48.4 (Nano @384) | Permissive, but a ViT backbone is expensive on a 4-core CPU. Avoid XL/2XL. |
| YOLOX-Nano/Tiny | Apache-2.0 | 0.9 / 5 M | 25.8 / 32.8 | Permissive, weak; OK for crop-level detection. |
| RTMDet-tiny (OpenMMLab) | Apache-2.0 | 4.8 M | 41 | Permissive alternative. |
| YOLOv7 / YOLOv9 (WongKinYiu) | GPL-3.0 | – | – | Compatible with AGPL-3.0 (GPLv3 §13), same lock-in as AGPL. |
| YOLO-NAS (Deci) | Code Apache, **weights non-commercial** | – | – | **Incompatible: reject.** |
| Jersey pipeline (Koshkina & Elder) | **CC BY-NC 3.0** [cited: repo] | – | – | **Incompatible: reject.** |

**Dataset trap:** fine-tuning on SoccerNet is **not** clean. SoccerNet video requires an NDA and is research-oriented. Weights trained on it would carry unclear terms. COCO-pretrained "person" is legally clean and good enough for crop-level detection.

---

## 2. GStreamer-native inference

- **`onnxinference`** (plugins-bad, revamped in 1.24; emits `GstTensorMeta` for "tensor decoder" elements):
  - It is **not packaged in Ubuntu 24.04** [verified absent], because ONNX Runtime isn't in the archive.
  - Its execution providers are **CPU, CUDA, VeriSilicon, MIGraphX, HIP, DirectML, Vitis AI: no OpenVINO** [cited: gstreamer docs, current version].
  - Its sink caps are system-memory RGB, so a `gldownload` precedes it anyway.
  - Using it means building gst-plugins-bad from source against a self-built ORT. **Not viable** for a project on distro GStreamer 1.24.2.
- **Intel DL Streamer** (`gvadetect`/`gvatrack`, MIT): needs Intel's OpenVINO stack from Intel's repos and ships its own GStreamer build (1.24.9 per the release notes, [cited]). That is a second GStreamer beside the system one and the Gen9 legacy-driver question on top. **Reject.**
- **`GstAnalyticsRelationMeta` / ODMtd / TrackingMtd** exist in 1.24's `libgstanalytics`, and `objectdetectionoverlay` is installed. They only matter if an inference *element* produces the metadata. The app already owns a better overlay (tiny-skia, PTS-keyed, zoom-aware). **No benefit.**
- **Does in-graph inference align better with "GStreamer owns pixels"? No, and pulling small tensors to Rust doesn't violate the rule.**
  - The rule exists because full-frame *resampling* in Rust costs 37.6 ms per frame versus 3.6 ms for vectors (the compositing spike).
  - The compliant shape keeps every full-frame op in GL: `decodebin3 → glcolorconvert/gltransformation (crop around target) → glcolorscale to 640×640 RGBA → gldownload → appsink`. Rust receives an already-small tensor (1.6 MB per frame) and runs inference on a worker thread.
  - This mirrors the existing export pump (appsink pull, bounded waits) and matches CLAUDE.md's "Rust owns … the vector overlay layer only". Inference is analysis, not compositing.
  - **Watch point:** a crop from the full-res frame must be done by `gltransformation`/`glvideomixer` geometry, not by slicing a downloaded 1440p frame in Rust.

---

## 3. Jersey number recognition

- **State of the art:** Koshkina & Elder, CVPRW 2024 [cited]. They report **87.4% tracklet-level accuracy on SoccerNet** and **91.4% on hockey**.
  - The pipeline is heavy: a ResNet-34 legibility classifier, **ViTPose** for torso ROI, Centroid-ReID for outlier removal, and **PARSeq** scene-text recognition, voting over the whole tracklet.
  - SoccerNet's challenge data is **broadcast** crops. The difficulty the paper names is that the number is "not visible in most frames", so the gain comes from voting over a tracklet.
  - The code is **CC BY-NC 3.0**, which is incompatible.
- **On this footage:**
  - At 1440p in a sideline wide shot a player is ~40–100 px tall, so a back number is **~8–20 px tall**, often motion-blurred, and front numbers are small or absent.
  - It is only readable when the coach's camera is zoomed in and the player faces away. Most frames of a tracklet will be illegible; the legibility classifier would mostly say "no".
  - Realistic end-to-end success on phone footage is **low (well under 50%), unmeasured**. Broadcast close-ups are better.
- **Cost:** running PARSeq-small (~24 M params) and a pose model per frame of one tracked player is feasible offline (a few hundred ms per frame [estimate]). Rebuilding the pipeline with permissive parts (PARSeq Apache-2.0, ViTPose Apache-2.0) and our own legibility classifier needs **labelled training data we don't have**.
- **Verdict:** **drop auto-reading for v1.** On click, let the coach type the label ("7"), one keystroke, rendered "#7". Revisit jersey OCR only for broadcast footage, and only after tracking ships.

---

## 4. Goal and kick-off detection

### 4a. Audio: whistle (DSP, not ML)
- **Signal:**
  - Referee whistles are narrowband and tonal. Reported peaks vary by source: 3.5–4.5 kHz, 2.3–2.9 kHz measured in one study, 3.3–4.2 kHz in another [cited: IJCA whistle paper; JDSAI 2023].
  - Spectral-energy methods produce **"significant false detections"** when cheering is near the band [cited].
  - A classic Goertzel/FIR detector reported **324 detected, 42 false positives, 26 misses** (~88% precision, ~93% recall) on broadcast audio [cited: IJCA 2010]. A later STFT + ML paper claims F1 ≈ 0.98 [cited via search snippet; the abstract has no numbers, unverified].
- **Design that fits the repo:**
  - A **pure function in `video-coach-core`**: `fn whistles(samples_16k_mono: &[f32]) -> Vec<Whistle { start, duration, freq }>`.
  - Per STFT frame (512-pt, 16 ms hop), find the peak in 2–5 kHz. Require **tonality** (peak ≥ ~15–20 dB over the band median) and **pitch stability** (±150 Hz) sustained for ≥ ~150 ms.
  - Classify by duration: a short tweet < 0.5 s, long ≥ ~0.8 s. Three long whistles in a row is the full-time pattern.
  - Needs only `rustfft` (MIT/Apache, pure Rust, **not** a media dep) or a hand-written Goertzel bank.
  - CI-testable with synthetic tones plus noise, like the export's "1.000 s tone" test.
  - Cost is trivial: 90 min of 16 kHz audio is ~340k frames, **under a second of CPU** [estimate]. The decode reuses Phase 10's Reader.
- **The semantic problem (the real one):**
  - A youth match has **dozens of whistles**: fouls, throw-ins, offside, and **adjacent pitches at tournaments**, which on a phone mic are nearly as loud.
  - A whistle ≠ a kick-off. Even a perfect whistle detector gives **low precision for "kick-off"**, perhaps 10–30% if every long whistle is offered [estimate].
  - It gives **high recall** for period start and end, because referees reliably whistle them.
  - Phone AGC, wind noise and the coach's own voice degrade it further. None of this is measured on the user's footage.
- **Optional ML second stage:** an AudioSet tagger (YAMNet, Apache-2.0, ~3.7 M params; or PANNs CNN14, MIT) has "Whistle", "Cheering", "Crowd" and "Applause" classes, and costs almost nothing on a CPU. Worth it only if the DSP stage's false positives prove bad.

### 4b. Audio: cheering
- Broadcast work reports ~87% goal precision from audio alone, but broadcast has a commentator and a 10,000-person crowd [cited: PMC4485672 summary].
- A sideline phone hears **a few dozen parents** who also cheer saves, near-misses and their own child's touches. Expect **far lower precision on amateur footage** (unmeasured).
- Useful only as one input to a combined score.

### 4c. Vision on a single sideline phone camera
- **Ball-in-goal: infeasible.** The ball is a few pixels, the camera pans, the goal is often out of frame or at the far end, and there's no calibration.
- **Kick-off formation** (all players in own halves, ball on the spot): needs full-frame detection of ~22 small players (the cost problem in §1a) **plus** knowing where the halfway line is in a panning, uncalibrated handheld shot. **Infeasible reliably. Drop.**
- **Cheap vision cue worth a spike:** a **low-motion "reset" interval**. Before a kick-off, players stand still for 5–30 s.
  - How: global motion energy from tiny GPU-downscaled frames (e.g. 160×90 gray via GL at 2 fps). Cheap: ~11k tiny frames per match.
  - Free kicks and corners also produce stillness, so it only works as corroboration.
- **Fixed high camera** (static, full pitch): formation detection becomes plausible after a one-time click on the centre spot and goal mouths, but players are tiny, so tiled detection is needed (~0.1× realtime). A later phase, if ever.

### 4d. Broadcast: scoreboard/clock OCR (the reliable one)
- **Method:**
  - The coach drags a rectangle over the score bug once. Better: two rectangles, score and clock.
  - Sample **1 fps**, crop and scale in GL, `gldownload` a tiny gray crop, and compute a mean-abs-diff against the last stable crop. A change that stays stable for N seconds triggers OCR.
  - 90 min at 1 fps = 5,400 tiny crops, **seconds of work**.
- **OCR choice:** digits on a fixed broadcast font are the easiest OCR there is.
  - **Template matching** of 0–9, learned from the first frames once the coach confirms the initial score, needs no model.
  - `ocrs` (Rust on `rten`, MIT/Apache, Latin only, "early preview") or PaddleOCR PP-OCR rec ONNX (Apache-2.0, ~10 MB) are the robust fallback.
  - Tesseract (Apache-2.0, `tesseract-ocr 5.3.4` in the archive) works but adds a C++ dependency.
- **What it yields:**
  - **Which side's number went up gives HomeGoal vs AwayGoal directly**, the attribution audio can never give.
  - **Clock reads 0:00 / 45:00 and starts running** gives period starts (StartStop) with high confidence.
- **Failure modes:** replays and graphics that hide or animate the bug, a changing bug design, sponsor wipes. Requiring stability over a few seconds handles most of them.
- **Realistic precision/recall on true broadcast:** high, **~95%+ for goals when a bug is present** [estimate, unmeasured]. A score only ever increments by one, so implausible reads can be rejected.

### 4e. What combines best (and realistic numbers)
- **Broadcast with a score bug:** OCR alone gives goals with attribution and period starts from the clock. Audio adds little. **Precision and recall should both be high.**
- **Phone sideline footage:** the strongest cheap pattern is structural. **A goal is followed by a centre kick-off 30–120 s later, preceded by a stillness interval and a whistle.**
  - Score candidates as: [cheer burst] → (30–120 s gap) → [still interval] → [whistle] → [motion resumes].
  - Period boundaries: the first and last long whistles, a long silence or recording gap (half-time), and the full-time three-whistle pattern.
  - Honest expectation: **recall ~70–90%, precision ~30–60%** [estimate; zero measurements on the user's footage]. That's fine for **suggestions a coach confirms in seconds**, and not fine for anything auto-applied.
  - Home/Away must be chosen by the coach at confirm time: two buttons on the suggestion.
- **The biggest unknown is data, not models.** Before designing thresholds, collect 2–3 of the coach's own matches with hand-labelled whistles, goals and period boundaries. That labelled set is the S0 spike, like whisper's throughput measurement.

---

## 5. MP4 chapters

- **GStreamer can't do it:**
  - `mp4mux` 1.24.2 has **no `GstTocSetter`** [verified: gst-inspect]. It has no chapter-track or `chpl` support.
  - `matroskamux` supports TOC, but export is MP4.
  - gst-plugins-rs' ISO muxer isn't installed and isn't known to write chapters (unverified).
- **Which formats players read:**
  - **Nero `chpl`** (in `moov/udta`) is read by **VLC** (`LoadChapterGpac` in `modules/demux/mp4/mp4.c` [cited]) and by **ffmpeg/ffprobe, and so mpv**.
  - The **QuickTime `chap` tref + text track** is what Apple players read. It needs sample data, so it is much more work.
- **YouTube doesn't read file chapters officially:** its help page documents chapters only from **description timestamps** (first 00:00, ≥ 3 entries, each ≥ 10 s) or auto-chapters. The page never mentions chapters embedded in the file [cited: support.google.com/youtube/answer/9884579]. One project claims YouTube's auto-chapter detection reads the atoms [cited: splitsmith PR #1004]; unverified. **To get chapters on YouTube, export a copyable "YouTube chapters" text block** alongside the file. Trivial, pure core.
- **Simplest writer, [verified here with a prototype]:**
  - Export already puts **`moov` first in a reserved region** (`reserved-max-duration`, `crates/video-coach-media/src/composite/export.rs:721-729`). On a test file, `mp4mux` produced `ftyp, free, moov(…, udta(meta)), free(2294), free(8), mdat`.
  - The post-process appends a `chpl` box into `moov/udta`, bumps the `udta` and `moov` sizes, and **shrinks the following `free` box by the same amount**. That is **in place, same file size, `mdat` untouched, no `stco` offsets change**.
  - Result: `ffprobe -show_chapters` listed both chapters with correct titles and times, and all 30 frames still decoded.
  - `chpl` v1 layout: FullBox(v=1, flags 0) + 4 reserved bytes + `u8` count + per entry { `u64` start in **100 ns units**, `u8` title length, UTF-8 title } (confirmed against `mp4-atom`'s `chpl.rs`, merged 2026-09-05, v0.16; MIT/Apache).
  - Limits: **≤ 255 chapters, titles ≤ 255 bytes.** Roughly 60 lines of Rust with no crate: a box walker and a byte splice.
  - Run it on the `.part` file before the rename; it works on bytes, so it could live in core or media.
- **The one real risk:** the reserve margin is `duration/10 + 1 s` of sample tables, so the padding scales with duration. A very short export with many long titles could lack room. Either add a fixed byte allowance to the reservation, or on overflow skip chapters with a logged warning. Don't implement moov relocation plus `stco` rewriting for this.
- **Chapter times:** use output time from the plan (`plan.total_frames()` denominators, `frame_time`), never duration sums (CLAUDE.md, Phase 8).
- **Note:** chapters-per-clip (one chapter per compiled clip, titled with the clip name) is **deterministic and needs no vision at all**. It could ship first and independently.

---

## 6. Recommendation

### Ship first (cheap, reliable, no model)
1. **MP4 chapters from what the project already knows:** one per clip, plus confirmed match events. Use the in-place `chpl` writer (§5) and a "copy YouTube chapters" text. Zero ML, verifiable with `ffprobe` in CI.
2. **Click-to-highlight with manual keyframes, no tracker yet:**
   - The data model (source-space, source-time-keyed boxes with colour and label), the zoom-aware rendering in `overlay.rs`, and per-player colours.
   - The coach sets the box at a few times; the app interpolates. Label typed on click ("#7").
   - This builds the whole storage, preview and export path, and the correction UX the tracker will need anyway.
3. **Whistle detection as suggestions:** pure DSP in core, on Phase 10's 16 kHz Reader. Offered as a chapter/suggestion list to jump between; the coach tags StartStop or goal and picks Home/Away.
   - Before thresholds: **collect labelled footage** from the coach's own matches.

### Second
4. **Auto-tracking to fill in between keyframes:**
   - Detector on a native-res crop (GL crop and scale, then pull, §2) + one-track Kalman/IoU in core. Offline job on the Phase 10 queue, model via the Phase 11 downloader.
   - **Permissive model (D-FINE-N/DEIM-N or RTMDet-tiny), CPU only.**
   - S0 spike: `rten` vs `ort` latency on this laptop, pinned threads and on AC.
5. **Broadcast scoreboard OCR** (rectangle once, 1 fps diff, template digits). The one path that yields attributed goals with high precision.
6. **Goal suggestions from the combined audio pattern** (cheer → gap → stillness → whistle), only once the labelled set shows it beats whistles alone.

### Drop, or defer indefinitely
- **Jersey-number OCR on phone footage:** the data is illegible, the only strong pipeline is non-commercial, and training data is missing.
- **Vision kick-off formation and ball-in-goal from a panning phone:** needs whole-match small-object detection plus pitch calibration. Infeasible on this hardware and camera.
- **iGPU inference, OpenVINO GPU, DL Streamer, GStreamer `onnxinference`:** no speed win on Gen9 GT2, plus unpackaged or legacy dependencies.
- **Live (under-the-pen) tracking:** CPU budget is ~0.4–0.8× realtime for a single crop per frame [estimate].

### Biggest risks
1. **No ground truth.** Every precision and recall number for phone footage above is an estimate. Without a labelled set from the coach's matches, thresholds are guesswork. Retire this first.
2. **Tracker identity switches** in crowds of same-kit players. Mitigated only by correction UX; budget for it as a core part of the feature.
3. **Inference throughput on a 15 W chip** under sustained load. It throttles, and whisper showed multi-minute numbers differ from short ones. Measure with the actual model before committing (Phase 10 S0 discipline).
4. **Licence drift:** default to Apache-2.0 models. Ultralytics AGPL is legal here but forecloses relicensing. Never use YOLO-NAS weights or the CC-BY-NC jersey pipeline. Avoid SoccerNet-trained weights.
5. **Packaging:** `ort` means a bundled `libonnxruntime.so` or a build-time download. `rten` avoids both, if it's fast enough.
6. **Adjacent-pitch whistles** at tournaments: a structural false-positive source that no single-mic DSP can separate.

---

## Sources
- Repo: `CLAUDE.md`; `docs/superpowers/specs/2026-09-20-linux-port-phase-10-design.md` (S1, S3, S5, S8); `…/2026-09-21-linux-port-phase-11-design.md` S3; `docs/superpowers/spikes/2026-09-19-{compositing-throughput,seek-latency,export-graph}.md`, `2026-09-21-whisper-throughput.md`; `crates/video-coach-core/src/{scoreboard,zoom,stroke_replay}.rs`; `crates/video-coach-media/src/{overlay.rs,composite/export.rs}`.
- [Ultralytics YOLO11 metrics](https://docs.ultralytics.com/models/yolo11/) · [Ultralytics OpenVINO benchmarks](https://docs.ultralytics.com/integrations/openvino/) · [Ultralytics licence](https://www.ultralytics.com/license) · [ultralytics/inference (Rust, AGPL)](https://github.com/ultralytics/inference)
- [Frigate hardware / OpenVINO inference table](https://docs.frigate.video/frigate/hardware)
- [OpenVINO GPU device docs](https://docs.openvino.ai/2025/openvino-workflow/running-inference/inference-devices-and-modes/gpu-device.html) · [intel/compute-runtime LEGACY_PLATFORMS](https://github.com/intel/compute-runtime/blob/master/LEGACY_PLATFORMS.md) · [OVMS CometLake GT2 crash #3635](https://github.com/openvinotoolkit/model_server/issues/3635)
- [ort execution providers](https://ort.pyke.io/perf/execution-providers) · [intel/openvino-rs](https://github.com/intel/openvino-rs) · [rten](https://github.com/robertknight/rten) · [rten-examples](https://github.com/robertknight/rten/tree/main/rten-examples) · [ocrs](https://github.com/robertknight/ocrs)
- [RF-DETR (licences per variant)](https://github.com/roboflow/rf-detr)
- Rust trackers: [jamtrack-rs](https://github.com/kadu-v/jamtrack-rs) · [trackforge](https://lib.rs/crates/trackforge) · [similari](https://crates.io/crates/similari-trackers-rs) · [edgefirst-tracker](https://lib.rs/crates/edgefirst-tracker) · [mot-rs](https://github.com/LdDl/mot-rs)
- [GStreamer onnxinference docs](https://gstreamer.freedesktop.org/documentation/onnx/index.html) · [Collabora on GStreamer ONNX revamp](https://www.collabora.com/news-and-blog/news-and-events/effortless-gstreamer-analytics-cross-platform-support-via-onnx.html) · [DL Streamer](https://github.com/open-edge-platform/dlstreamer)
- [Koshkina & Elder, CVPRW 2024](https://openaccess.thecvf.com/content/CVPR2024W/CVsports/html/Koshkina_A_General_Framework_for_Jersey_Number_Recognition_in_Sports_Video_CVPRW_2024_paper.html) · [jersey-number-pipeline (CC BY-NC)](https://github.com/mkoshkina/jersey-number-pipeline) · [SoccerNet 2023 challenges](https://arxiv.org/abs/2309.06006)
- [Referee whistle detection, IJCA](https://www.ijcaonline.org/archives/volume12/number11/1729-2340/) · [Whistle detection with ML, JDSAI](https://publications.isods.org/index.php/jdsai/article/view/33) · [Audio-visual soccer summarization](https://pmc.ncbi.nlm.nih.gov/articles/PMC4485672/)
- [Small-object recall vs pixel size](https://joelhuang.dev/blog/small-object-detection)
- [SoccerNet action-spotting per-class AP (goal 75.6, kick-off 34.3 on broadcast)](https://doi.org/10.3390/app16146838)
- [VLC mp4 demux (chpl)](https://github.com/videolan/vlc/blob/master/modules/demux/mp4/libmp4.h) · [mp4-atom chpl PR #232](https://github.com/kixelated/mp4-atom/pull/232) · [YouTube chapters help](https://support.google.com/youtube/answer/9884579) · [splitsmith PR #1004](https://github.com/mandakan/splitsmith/pull/1004)
