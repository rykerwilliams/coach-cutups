# Spike — CPU compositing throughput for the Linux port

**Date:** 2026-09-19
**Question:** The Linux port design proposed composing every output frame in Rust (`appsink` → `tiny-skia` → `appsrc`), replacing the macOS Core Image GPU pipeline. Is CPU compositing fast enough?
**Answer:** Not as proposed — but the cost is concentrated in one operation, and moving that one operation into GStreamer makes the Rust side ~9x faster than realtime.

## Method

`tiny-skia` 0.12 + `cosmic-text` 0.19, release build, single-threaded.
Machine: Intel Xeon @ 2.80 GHz, 4 cores (shared CI container — treat absolute numbers as a **floor**; a desktop CPU will be faster).
Workload models one 1080p output frame of `CompilationCompositor.startRequest`.

## Result 1 — compose everything in Rust (as the spec proposed)

```
base image blit w/ zoom transform (bilinear, 1920x1080 src) : 37.56 ms
webcam PiP blit (640x480 → 0.22·W, bilinear)                :  2.58 ms
text-bar background fill_rect                               :  0.51 ms
40-segment freehand stroke, round caps/joins, antialiased    :  0.83 ms
two text runs (cosmic-text + swash)                         :  0.22 ms
------------------------------------------------------------------
total                                                        : 41.71 ms
                                            = 24.0 fps = 0.80x realtime
```

**Fails.** A 3-minute clip would take over 3.7 minutes to composite, before decode or encode.

## Result 2 — GStreamer video + Rust overlay layer only

Same rasterizer, but Rust draws *only* the vector overlay into a transparent RGBA layer;
GStreamer owns base scale/crop/zoom and PiP mixing. Deliberately a **busier** frame than
Result 1: three strokes instead of one, plus a scoreboard plate.

```
clear transparent layer                                     :  0.61 ms
vector (text bar + scoreboard plate + 3 strokes)            :  2.59 ms
text (2 runs)                                               :  0.42 ms
------------------------------------------------------------------
total                                                        :  3.62 ms
                                           = 276.4 fps = 9.2x realtime
```

**Passes with an order of magnitude to spare.**

## Conclusion

Full-frame image resampling is ~90% of the cost of Result 1 (37.56 of 41.71 ms). Vector
overlay rasterization is nearly free. The architecture should therefore be:

- **GStreamer** owns every full-frame pixel operation: decode, scale, crop, the zoom
  transform, and PiP mixing.
- **Rust + tiny-skia** owns only the vector overlay layer: strokes, text bar, scoreboard.

This is both faster *and* structurally simpler than the spec's original proposal — it
removes the per-frame full-resolution CPU staging copy that `MPVSourcePlayer.swift`
(lines 8–22) documents this project already spent eight phases migrating away from on
macOS.

## Caveats

- Single-threaded. Export compositing parallelizes trivially across output frames, so
  Result 1 is not as hopeless as it looks for *export* — but it is fatal for *preview*,
  which is frame-clocked and cannot buy throughput with latency.
- Absolute numbers come from a shared CI container. The ratio between the two results is
  the durable finding; the absolute floor is not.
- `tiny-skia` has no SIMD-accelerated pattern shader path for the arbitrary-transform
  bilinear case exercised in Result 1. A different rasterizer might narrow the gap, but
  not by 10x, and not without giving up Result 2's simplicity.
- Text cost is measured with a warm `SwashCache`. First-frame glyph rasterization is
  slower; irrelevant for sustained throughput.

## Reproducing

```rust
// Cargo.toml: tiny-skia = "0.12", cosmic-text = "0.19"
// Result 1: draw base pixmap with Transform::from_translate(W/2,H/2)
//             .pre_scale(1.6,1.6).pre_translate(-(0.5+pan)*W, -0.5*H)
//           then bar rect, PiP pixmap, stroke_path, two cosmic-text runs.
// Result 2: layer.fill(Color::TRANSPARENT) then bar rect, scoreboard rect,
//           3x stroke_path, two cosmic-text runs. No pixmap blits.
// Time each stage separately over 300-600 frames; divide.
```
