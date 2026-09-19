# Spike — 4K HEVC accurate-seek latency, software decode

**Date:** 2026-09-19
**Question:** The Phase 2 gate says the scan player falls back to libmpv if accurate seek on 4K HEVC exceeds ~250 ms *with a hardware decoder confirmed*. Can that be settled without a GPU?
**Answer:** Partly. Software decode fails the criterion, which does not settle it — but decomposing the cost does change what the gate should measure.

## Method

Generated a synthetic 4K HEVC file (`videotestsrc pattern=smpte` → `x265enc speed-preset=ultrafast bitrate=20000 key-int-max=60` → `matroskamux`): 3840×2160, 30 fps, 30 s, **2-second GOP**, 73 MB.

Measured with `gstreamer-rs`, `filesrc ! decodebin ! videoconvert ! fakesink sync=false`, eight seeks spread across the file, each timed to completion via a blocking state query. Decoder selected: `avdec_h265` (software).

Machine: Intel Xeon @ 2.80 GHz, 4 cores — a shared CI container, so treat absolutes as a **floor**.

## Result

| Seek mode | median | worst |
|---|---|---|
| `FLUSH \| KEY_UNIT` (flush + demux, decodes ~nothing) | **104.6 ms** | 148.9 ms |
| `FLUSH \| ACCURATE` (+ decode forward to the target frame) | **439.8 ms** | 616.6 ms |

Decomposed:

```
fixed pipeline cost   : 104.6 ms   hardware decode does NOT reduce this
decode-forward cost   : 335.2 ms   this is what hardware reduces  (76% of total)
```

## What this settles, and what it doesn't

**It does not settle the gate.** The test is one-sided: hardware decode is strictly faster than software, so a software *pass* would have settled it for every machine. A software *fail* says nothing about hardware — and 76% of the cost is precisely the term hardware eliminates.

**It does produce two facts the gate did not have:**

1. **There is a hardware-independent floor of ~105 ms**, spent on the pipeline flush and demux before a single frame is decoded. That is 42% of the entire 250 ms budget. A hardware decoder has to bring 335 ms down to under ~145 ms to pass — plausible, but the criterion is much tighter than "hardware will obviously fix it."
2. **The hybrid seek policy is independently validated.** KEY_UNIT at ~105 ms median is comfortably usable for live scrubber drag, and it is hardware-independent, so it holds on any machine. The spec already specifies KEY_UNIT during drag and ACCURATE on release; this is evidence for that split rather than an assumption behind it.

## Caveats

- **Synthetic content understates the decode term.** SMPTE bars have far less entropy and motion than real match film, so `avdec_h265` decodes them faster than it would decode the real thing. The 335 ms decode-forward figure is a floor, not an estimate.
- Shared CI container; both terms would improve on a desktop CPU, but the *ratio* is the durable finding.
- A 2-second GOP is a reasonable guess at camera output. A longer GOP increases the decode-forward term proportionally and does not move the fixed cost.

## What still needs real hardware

`scripts/linux-gate-check.sh <file>` on a machine with a GPU, against **real match film**. Two numbers matter: whether a hardware decoder is selected at all (rank, not just presence), and whether ACCURATE lands under 250 ms given the ~105 ms floor measured here.
