# Spike — whisper throughput on the reference laptop

**Date:** 2026-09-21
**Phase:** 10 (transcription), closeout measurement
**Spec:** `docs/superpowers/specs/2026-09-20-linux-port-phase-10-design.md` (S0)

## Why

S0 gated two defaults on one number: which model ships as the default, and
whether stopping a recording should queue its clip. Nothing about whisper had
ever been measured in this project.

## Rig

| | |
|---|---|
| CPU | Intel i7-10610U, 4 cores / 8 threads, 15 W |
| Power | **on AC** (a 15 W mobile part throttles over a multi-minute run) |
| Model | `ggml-small.en.bin`, 487,614,201 bytes |
| `n_threads` | **8** (`available_parallelism()`; whisper's own default is `min(4, …)` and would have halved this) |
| Sampling | greedy, `best_of = 5` (whisper.cpp's own greedy default) |
| `openmp` | off (`whisper-rs-sys`'s `build.rs` disables it by default) |
| Build | whisper.cpp always `Release` — `build.rs` hardcodes it, so the Rust profile is not a factor |
| Test threads | **1** — see the trap below |

## Result

```
transcribe: 65.0 s of audio in 89.2 s (0.73x), ggml-small.en.bin, 8 threads
```

**`small.en` runs at 0.73× realtime.** Transcribing takes about 37% longer than
the recording did.

## Two traps in measuring this, both of which produced wrong numbers first

1. **A short clip's figure is not a rate.** whisper pads anything under 30 s to
   a full window, so a 3 s clip reported `0.06×` — that is one window's cost
   divided by three seconds, not throughput. Only clips longer than one window
   give an honest ratio. The first number this project recorded was the 3 s one.
2. **`cargo test -- --ignored` runs the ignored tests *in parallel*.** Two
   whisper contexts, ~1 GB resident, 16 threads on 8 cores — while producing the
   throughput line. The same fixture measured **29.2 s** in parallel against
   **20.2 s** serialised, i.e. ~45% pessimistic. `--test-threads=1` is in
   `CLAUDE.md`'s documented command for this reason.

## What it decided

- **The default model stays `small.en`** — the user's informed choice when the
  tradeoff was described ("2–3× slower than base, better on names"), and 0.73×
  is consistent with that description. `base.en` is now a picker away
  (`de89f3e`), so the decision is no longer a constant anyone has to defend.
- **Auto-enqueue on recording stop defaults OFF**, and the throughput number is
  the weaker half of the argument. The stronger half is the **livelock**: a
  preempted job restarts from zero, so while the coach's recording cadence is
  shorter than a job's runtime, *no job ever completes* — front-requeue or back,
  whichever clip you pick. A coach recording six takes back to back would finish
  the session with a full queue and no transcripts, having spent the whole
  session with 8 whisper threads competing against the capture pipeline. It is
  bounded (the queue drains once recording stops) and every fix costs real
  complexity (chunked checkpointing, partial-transcript merge), so the fix is to
  not start the work unasked.

## One more measured fact, recorded here because it surprised everyone

**Cancelling costs ~12 s**, not the ~10 ms the code claimed in three places.
whisper.cpp's encoder and decoder call the `ggml_backend_sched_t` overload of
`ggml_graph_compute_helper`, which never installs the abort callback; the
per-node overload that does is used elsewhere. So the flag is consulted once per
encode and once per decode pass. Probe: flag set 1 ms into `full()`, returned
after **12.36 s**.

This is why `Transcriber::drop` cancels without joining (`38854c4`) — the bus
thread used to block on it, and a harness test that hit record during a real
transcription never saw the recording acknowledged within 15 s. It is 53 ms now.
