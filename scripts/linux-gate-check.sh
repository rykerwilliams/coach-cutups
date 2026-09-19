#!/usr/bin/env bash
# Phase 2 gate check for the Coach Cuts Linux port.
#
# Answers the questions that decide a LOCKED architectural decision and that
# cannot be answered in a headless container with no GPU:
#
#   1. Is a hardware video decoder available, and does it outrank software?
#   2. Can decoded frames reach the display without a system-memory round-trip?
#   3. How slow is an accurate seek on real 4K HEVC match film?
#   4. Does the GL compositing chain the export design assumes actually link?
#   5. Which H.264 encoder will exports get?
#
# Question 3 carries the kill criterion: if accurate seek exceeds ~250 ms WITH
# a hardware decoder confirmed, the scan player switches from GStreamer to
# libmpv (spec risk 1b).
#
# Usage:  scripts/linux-gate-check.sh [path/to/4k-match-film.mp4]
#
# Install first if needed (Debian/Ubuntu):
#   sudo apt install gstreamer1.0-tools gstreamer1.0-plugins-{base,good,bad,ugly} \
#                    gstreamer1.0-libav gstreamer1.0-vaapi gstreamer1.0-pipewire \
#                    vainfo
# Fedora:
#   sudo dnf install gstreamer1-plugins-{base,good,bad-free,ugly} gstreamer1-libav \
#                    gstreamer1-vaapi libva-utils

set -uo pipefail
FILM="${1:-}"
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
hdr()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

hdr "0. Environment"
if ! command -v gst-inspect-1.0 >/dev/null; then
  bad "gst-inspect-1.0 not found — install GStreamer (see header of this script)"
  exit 1
fi
ok "GStreamer $(gst-inspect-1.0 --version | awk '/^GStreamer/{print $2}')"
printf '  session type: %s\n' "${XDG_SESSION_TYPE:-unknown}"
command -v vainfo >/dev/null && vainfo 2>/dev/null | grep -m1 'Driver version' | sed 's/^/  /'
command -v nvidia-smi >/dev/null && nvidia-smi --query-gpu=name --format=csv,noheader | sed 's/^/  NVIDIA: /'

hdr "1. Hardware decoders (and their RANK vs software)"
# Rank is the whole game: decodebin picks by rank, and on most distro builds
# avdec_* is PRIMARY while the hardware decoders are NONE or MARGINAL. The
# default outcome is therefore SOFTWARE decode of 4K HEVC.
rank_of() { gst-inspect-1.0 "$1" 2>/dev/null | sed -n 's/^ *Rank *//p' | head -1; }
FOUND_HW=0
for e in vah265dec vah264dec vaapih265dec vaapih264dec nvh265dec nvh264dec v4l2slh265dec; do
  if gst-inspect-1.0 --exists "$e" 2>/dev/null; then
    ok "$e present — rank: $(rank_of "$e")"
    FOUND_HW=1
  fi
done
[ "$FOUND_HW" = 0 ] && bad "NO hardware decoder found — 4K HEVC will decode on the CPU"
for e in avdec_h265 avdec_h264; do
  gst-inspect-1.0 --exists "$e" 2>/dev/null && printf '    (software %s rank: %s)\n' "$e" "$(rank_of "$e")"
done

hdr "2. H.264 encoders (export path)"
for e in vah264enc vaapih264enc nvh264enc x264enc; do
  gst-inspect-1.0 --exists "$e" 2>/dev/null && ok "$e — rank: $(rank_of "$e")"
done

hdr "3. GL compositing chain (export design)"
for e in glupload glcolorconvert gltransformation glvideomixer gloverlaycompositor; do
  gst-inspect-1.0 --exists "$e" 2>/dev/null && ok "$e" || bad "$e MISSING"
done
printf '  linking a 3-pad GL mixer (base + PiP + overlay)...\n'
if timeout 25 gst-launch-1.0 -q \
     videotestsrc num-buffers=20 ! video/x-raw,width=640,height=360 ! glupload ! glcolorconvert ! gltransformation scale-x=1.5 ! m.sink_0 \
     videotestsrc num-buffers=20 pattern=ball ! video/x-raw,width=320,height=180 ! glupload ! glcolorconvert ! m.sink_1 \
     videotestsrc num-buffers=20 pattern=checkers-8 ! video/x-raw,width=640,height=360 ! glupload ! glcolorconvert ! m.sink_2 \
     glvideomixer name=m ! fakesink >/dev/null 2>&1; then
  ok "GL chain links and runs"
else
  bad "GL chain FAILED — export would fall back to software compositor"
fi

hdr "4. PipeWire capture (recording path)"
gst-inspect-1.0 --exists pipewiresrc 2>/dev/null && ok "pipewiresrc present" \
  || warn "pipewiresrc missing — install gstreamer1.0-pipewire (v4l2src fallback exists)"
ls /dev/video* >/dev/null 2>&1 && ok "camera nodes: $(ls /dev/video* | tr '\n' ' ')" \
  || warn "no /dev/video* nodes visible"

if [ -z "$FILM" ]; then
  hdr "5. SKIPPED — seek latency"
  warn "Re-run with a real 4K HEVC match file to measure the kill criterion:"
  printf '      scripts/linux-gate-check.sh ~/film/match.mp4\n'
  exit 0
fi

hdr "5. Decode path + seek latency on $FILM"
[ -f "$FILM" ] || { bad "no such file"; exit 1; }

# The app's GL context is EGL (Slint's Skia renderer). On X11 GStreamer defaults
# to GLX, where 1.24's DMABuf importer does not exist and every frame is copied
# through the CPU. Report what the default would do, then measure as the app.
GLOUT='glupload name=u ! glcolorconvert ! video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D'
up_default=$(GST_DEBUG=glupload:6 timeout 30 gst-launch-1.0 filesrc location="$FILM" ! decodebin3 ! 'video/x-raw(ANY)' ! glupload ! glcolorconvert ! 'video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D' ! fakesink num-buffers=5 2>&1 | grep -oE 'uploader [A-Za-z ]+ returned 1' | head -1 | sed -E 's/uploader (.*) returned 1/\1/')
if [ "$up_default" = "DirectDmabufExternal" ] || [ "$up_default" = "Dmabuf" ]; then
  ok "default GL platform imports DMABuf zero-copy ($up_default)"
else
  warn "default GL platform uploads via '${up_default:-?}' (a CPU copy) -- the app must use an EGL context"
fi
export GST_GL_PLATFORM=egl

# Everything below is verified rather than inferred. The first version of this
# section reported a pass it could not prove: it never checked that a seek was
# accepted (a rejected seek returns instantly and looks like a fast one), its
# decoder detection grepped debug output that did not match, and it checked
# zero-copy against fakesink, which accepts system memory and so proves nothing.
python3 - "$FILM" <<'PY'
import sys, time, statistics
import gi
gi.require_version("Gst", "1.0")
from gi.repository import Gst
Gst.init(None)
path = sys.argv[1]

# --- GOP length: the variable that actually decides accurate-seek cost.
# Accurate seek decodes forward from the previous keyframe, so the worst case
# is one full GOP of frames regardless of which player or decoder is used.
kf = []
def kprobe(pad, info):
    b = info.get_buffer()
    if not b.has_flags(Gst.BufferFlags.DELTA_UNIT):
        kf.append(b.pts / Gst.SECOND)
    return Gst.PadProbeReturn.OK
p = Gst.parse_launch(
    f'filesrc location="{path}" ! parsebin ! video/x-h264;video/x-h265 '
    '! fakesink name=k sync=false num-buffers=1800')
p.get_by_name("k").get_static_pad("sink").add_probe(Gst.PadProbeType.BUFFER, kprobe)
p.set_state(Gst.State.PLAYING)
p.get_bus().timed_pop_filtered(120 * Gst.SECOND, Gst.MessageType.EOS | Gst.MessageType.ERROR)
p.set_state(Gst.State.NULL)
gaps = [b - a for a, b in zip(kf, kf[1:])]
gop = max(gaps) if gaps else float("nan")
print(f"  keyframe interval (first ~1800 frames): {gop:.2f}s")

# --- Which decoder gets picked, and whether frames are imported into GL
# without a system-memory round trip. The downstream GLMemory/RGBA caps are
# essential: with a plain fakesink after glupload, glupload passes DMABuf
# THROUGH without importing, which looks zero-copy and proves nothing.
#
# decodebin3, not decodebin: measured on Intel VA-API, decodebin negotiates
# SYSTEM memory into glupload and runs ~6x slower (117 vs 739 fps at 1440p).
# The `video/x-raw(ANY)` filter is required -- without it the first pad to
# appear (often audio) gets linked and the measurement silently times audio.
p = Gst.parse_launch(
    f'filesrc location="{path}" ! decodebin3 ! video/x-raw(ANY) ! glupload name=u '
    '! glcolorconvert ! video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D '
    '! fakesink num-buffers=3')
p.set_state(Gst.State.PAUSED)
p.get_state(30 * Gst.SECOND)
dec, feature = "NONE FOUND", "?"
it = p.iterate_recurse()
while True:
    r, e = it.next()
    if r != Gst.IteratorResult.OK:
        break
    f = e.get_factory()
    if f and "Decoder/Video" in (f.get_metadata("klass") or ""):
        dec = f.get_name()
caps = p.get_by_name("u").get_static_pad("sink").get_current_caps()
if caps:
    feature = caps.get_features(0).to_string()
p.set_state(Gst.State.NULL)
hw = not dec.startswith("avdec_")
print(f"  decoder selected: {dec} ({'HARDWARE' if hw else 'software'})")
zc = "zero-copy" if "SystemMemory" not in feature else "frames pass through system memory"
print(f"  caps into GL (EGL, as the app): {feature} -> {zc}")

# --- Seek latency, measured in PAUSED: scrubbing a paused frame is the real
# interaction, and a flushing seek in PAUSED must re-preroll, so get_state()
# genuinely blocks until the target frame is at the sink. Every seek's
# acceptance is checked and a landed frame is required.
#
# Measured THROUGH GL, because that is the app's display path and it changes
# the answer: into a system-memory sink every frame decoded forward from the
# keyframe is also copied off the GPU, which made a 2s-GOP 1080p60 file look
# like 436 ms per accurate seek when the real zero-copy path takes 91 ms.
p = Gst.parse_launch(
    f'filesrc location="{path}" ! decodebin3 ! video/x-raw(ANY) ! glupload '
    '! glcolorconvert ! video/x-raw(memory:GLMemory),format=RGBA,texture-target=2D '
    '! queue ! fakesink name=s sync=false')
landed = {"pts": None}
def sprobe(pad, info):
    landed["pts"] = info.get_buffer().pts
    return Gst.PadProbeReturn.OK
p.get_by_name("s").get_static_pad("sink").add_probe(Gst.PadProbeType.BUFFER, sprobe)
p.set_state(Gst.State.PAUSED)
p.get_state(30 * Gst.SECOND)
_, dur = p.query_duration(Gst.Format.TIME)

def bench(label, flags, n=10):
    times = []
    for i in range(1, n + 1):
        # Offset so targets rarely coincide with a keyframe.
        target = int(dur * i / (n + 1)) + 370_000_000
        landed["pts"] = None
        t0 = time.perf_counter()
        if not p.seek_simple(Gst.Format.TIME, flags, target):
            sys.exit(f"  SEEK REJECTED at {target / Gst.SECOND:.2f}s -- measurement invalid")
        p.get_state(30 * Gst.SECOND)
        if landed["pts"] is None:
            sys.exit("  no frame prerolled after seek -- measurement invalid")
        times.append((time.perf_counter() - t0) * 1000)
    times.sort()
    print(f"  {label:<10} median {statistics.median(times):7.1f} ms   worst {times[-1]:7.1f} ms")
    return times[-1]

print()
kworst = bench("KEY_UNIT", Gst.SeekFlags.FLUSH | Gst.SeekFlags.KEY_UNIT)
aworst = bench("ACCURATE", Gst.SeekFlags.FLUSH | Gst.SeekFlags.ACCURATE)
p.set_state(Gst.State.NULL)

print()
if aworst <= 250:
    print("  \033[32mPASS\033[0m -- accurate seek within 250 ms on this source.")
elif not hw:
    print("  \033[31mSLOW\033[0m -- software decode selected; fix decoder selection first.")
elif "SystemMemory" in feature:
    print("  \033[31mSLOW\033[0m -- frames are not zero-copy into GL; every decode-forward frame")
    print("  is being copied off the GPU. Fix the decode path before judging seek latency.")
else:
    print("  \033[31mSLOW\033[0m -- hardware decode is selected, so this is decode-forward cost:")
    print(f"  up to one {gop:.1f}s GOP of frames per seek. Switching players (e.g. libmpv) would")
    print("  NOT help -- it would use the same hardware decoder. Mitigations: KEY_UNIT during")
    print(f"  drag (worst {kworst:.0f} ms here) and short-GOP proxies at import.")
PY
