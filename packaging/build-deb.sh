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
