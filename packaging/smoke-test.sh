#!/usr/bin/env bash
# Smoke-test the Coach Cuts .deb in a clean Ubuntu 24.04 container: the one
# definition, run by hand and by CI.
#
#   packaging/smoke-test.sh target/debian/coach-cuts_<version>_amd64.deb
#
# Needs docker. A clean container, not the build machine, because the build
# machine already has every library installed and so can't prove the package's
# dependency list (Phase 11 spec, Done-when #1). The steps:
#
#   1. Depends carries `libc6 (>= 2.39)`: proof that dpkg-shlibdeps ran.
#   2. `apt install` of the package alone (no Recommends) resolves.
#   3. Every software-path GStreamer element the code names exists.
#   4. The app launches under Xvfb, stays up, and creates a project. This is
#      the only proof of the X11 libraries winit dlopens, which no linker or
#      dpkg-shlibdeps sees. It decodes nothing: hardware decode is checked by
#      hand on the laptop.
set -euo pipefail

if [[ "${1:-}" != --inside ]]; then
    deb=$(realpath "${1:?usage: $0 path/to/coach-cuts_<version>_amd64.deb}")
    [[ -f $deb ]] || { echo "no such package: $deb" >&2; exit 1; }
    exec docker run --rm \
        -v "$deb:/smoke/${deb##*/}:ro" \
        -v "$(realpath "$0"):/smoke/smoke-test.sh:ro" \
        ubuntu:24.04 /smoke/smoke-test.sh --inside "/smoke/${deb##*/}"
fi

# From here on: inside the container, as root.
deb=$2
export DEBIAN_FRONTEND=noninteractive
step() { printf '\n== %s\n' "$*"; }
fail() { printf '\nSMOKE TEST FAILED: %s\n' "$*" >&2; exit 1; }
apt_install() {
    apt-get install -y -qq --no-install-recommends "$@" >/dev/null ||
        fail "apt could not install: $*"
}

step "1. Depends carries dpkg-shlibdeps' libc floor"
depends=$(dpkg-deb -f "$deb" Depends)
echo "$depends"
[[ $depends == *"libc6 (>= 2.39)"* ]] ||
    fail "Depends lacks 'libc6 (>= 2.39)': the package was built without dpkg-shlibdeps (install dpkg-dev)"

step "2. apt installs the package and its dependencies"
apt-get update -qq
apt_install "$deb"
dpkg -s coach-cuts | grep -E '^(Package|Version|Status):'

step "3. Every software-path element exists"
# gst-inspect-1.0 is for this check only: nothing in Depends pulls it.
apt_install gstreamer1.0-tools
# The elements the code names, found by grepping crates/*/src and keeping
# what gst-inspect-1.0 knows. Left out: the VA elements (vah264lpenc,
# vajpegdec), absent without /dev/dri and checked by hand; the test sources
# and `fixtures.rs`'s encoders, which only tests use. The last five aren't
# named: decodebin3 autoplugs them for game film (MP4, H.264/H.265) and for
# the recordings (Matroska, Opus) when no VA decoder is present.
elements=(
    playbin3 decodebin3 filesrc filesink fakesink queue capsfilter
    appsrc appsink videoconvert audioconvert audioresample level
    autoaudiosink
    glupload glcolorconvert gltransformation glvideomixer gldownload
    v4l2src pipewiresrc jpegdec x264enc opusenc matroskamux
    h264parse avenc_aac aacparse mp4mux
    souphttpsrc
    qtdemux matroskademux avdec_h264 avdec_h265 opusdec
)
missing=()
for element in "${elements[@]}"; do
    gst-inspect-1.0 --exists "$element" || missing+=("$element")
done
((${#missing[@]} == 0)) || fail "GStreamer elements missing: ${missing[*]}"
echo "all ${#elements[@]} present"

step "4. The app launches, stays up and creates a project"
apt_install xvfb xauth libgl1-mesa-dri libegl-mesa0
project=$(mktemp -d)
config=$(mktemp -d)
log=$(mktemp)
status=0
XDG_CONFIG_HOME=$config timeout 20 xvfb-run -a coach-cuts "$project" >"$log" 2>&1 ||
    status=$?
cat "$log"
((status == 124)) ||
    fail "coach-cuts exited with status $status within 20 s; it should still be running (124 = killed by timeout)"
! grep -q 'panicked' "$log" || fail "coach-cuts panicked (log above)"
[[ -f $project/project.json ]] || fail "coach-cuts did not create $project/project.json"
echo "still running at 20 s, no panic, project.json created"

step "PASSED"
