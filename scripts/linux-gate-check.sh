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
rank_of() { gst-inspect-1.0 "$1" 2>/dev/null | awk '/Rank/{print $2, $3; exit}'; }
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

hdr "5. Decode path + accurate-seek latency on $FILM"
[ -f "$FILM" ] || { bad "no such file"; exit 1; }
gst-discoverer-1.0 "$FILM" 2>/dev/null | grep -E 'Duration|width|height|Codec|video codec' | head -6 | sed 's/^/  /'

# Which decoder actually gets chosen, and does the frame stay off the CPU?
printf '\n  decoder actually selected:\n'
GST_DEBUG=GST_ELEMENT_FACTORY:4 timeout 25 gst-launch-1.0 -q \
  filesrc location="$FILM" ! decodebin ! fakesink num-buffers=5 2>&1 \
  | grep -oE 'chosen.*(vah26[45]dec|nvh26[45]dec|vaapi[a-z0-9]*dec|avdec_h26[45]|v4l2[a-z0-9]*dec)' \
  | tail -3 | sed 's/^/    /'

printf '\n  negotiated caps feature (zero-copy check):\n'
CAPS=$(timeout 25 gst-launch-1.0 -v filesrc location="$FILM" ! decodebin ! fakesink num-buffers=3 2>&1 \
       | grep -oE 'memory:(DMABuf|VAMemory|GLMemory|NVMM)' | sort -u | tr '\n' ' ')
if [ -n "$CAPS" ]; then
  ok "frames stay on the GPU: $CAPS"
else
  warn "no GPU memory feature negotiated — frames are landing in system memory"
fi

printf '\n  accurate-seek latency (5 seeks, ACCURATE|FLUSH):\n'
python3 - "$FILM" <<'PY'
import sys, time
import gi
gi.require_version("Gst", "1.0")
from gi.repository import Gst
Gst.init(None)

path = sys.argv[1]
p = Gst.parse_launch(f'filesrc location="{path}" ! decodebin ! videoconvert ! fakesink name=s sync=false')
p.set_state(Gst.State.PLAYING)
p.get_state(Gst.CLOCK_TIME_NONE)

ok, dur = p.query_duration(Gst.Format.TIME)
if not ok:
    print("    could not query duration"); sys.exit(0)

flags = Gst.SeekFlags.FLUSH | Gst.SeekFlags.ACCURATE
times = []
for i in range(1, 6):
    target = int(dur * i / 7)
    t0 = time.perf_counter()
    p.seek_simple(Gst.Format.TIME, flags, target)
    p.get_state(Gst.CLOCK_TIME_NONE)      # blocks until the seek completes
    times.append((time.perf_counter() - t0) * 1000)

p.set_state(Gst.State.NULL)
worst, avg = max(times), sum(times) / len(times)
for i, t in enumerate(times, 1):
    print(f"    seek {i}: {t:7.1f} ms")
print(f"    avg {avg:.1f} ms   worst {worst:.1f} ms")
print()
if worst <= 250:
    print("    \033[32mPASS\033[0m — GStreamer stays the scan player.")
else:
    print("    \033[31mFAIL\033[0m — exceeds the 250 ms kill criterion.")
    print("    If a hardware decoder was selected above, this triggers the")
    print("    libmpv fallback for the scan player (spec risk 1b).")
PY
