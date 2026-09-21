#!/usr/bin/env bash
# Build the Coach Cuts .deb: target/debian/coach-cuts_<version>_amd64.deb.
#
#   packaging/build-deb.sh [extra cargo-deb flags]
#
# Needs cargo-deb and cargo-about (`cargo install cargo-deb` and
# `cargo install cargo-about --features cli`), and dpkg-dev, whose
# dpkg-shlibdeps computes the linked-library half of Depends. Without it
# cargo-deb only warns and ships no libc floor; packaging/smoke-test.sh fails
# such a package.
#
# First the crate licence notices (spec S6), generated from Cargo.lock by
# cargo-about. Its accepted-licence list (packaging/about.toml) is also the
# GPL-2.0-only tripwire, so a crate whose licence isn't on it fails here,
# before the three-minute build. Then cargo-deb, which ships the notices from
# the path the asset list names.
#
# Last, whisper.cpp's instruction set is asserted on the release build that
# was packaged. `.cargo/config.toml` pins it, but cargo does not rerun
# whisper-rs-sys's build script when that [env] changes, so a warm target/
# can still hold a library built before the pin: `-march=native` dies with
# SIGILL on an older CPU, and one without `-mavx2` is scalar and unusably
# slow.
set -euo pipefail
cd "$(dirname "$0")/.."

release_dir="${CARGO_TARGET_DIR:-target}/release"
mkdir -p "$release_dir"
cargo fetch --locked
cargo about generate --fail --frozen \
    --config packaging/about.toml \
    --manifest-path crates/video-coach-app/Cargo.toml \
    --output-file "$release_dir/crate-licenses.txt" \
    packaging/about.hbs
cargo deb --locked -p video-coach-app "$@"

build_outputs=("$release_dir"/build/whisper-rs-sys-*/output)
[[ -f ${build_outputs[0]} ]] || {
    echo "no whisper-rs-sys build output under $release_dir/build" >&2
    exit 1
}
for output in "${build_outputs[@]}"; do
    variant=$(grep -m1 'CPU backend variant' "$output" || true)
    if [[ $variant != *-mavx2* ]] || grep -q -- '-march=native' "$output"; then
        echo "$output: whisper.cpp was not built for x86-64-v3 (${variant:-no CPU backend line})." >&2
        echo "Run 'cargo clean -p whisper-rs-sys --release' and build again." >&2
        exit 1
    fi
done
echo "whisper.cpp: ${variant#*-- }"
