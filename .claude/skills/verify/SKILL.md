---
name: verify
description: Run the full Rust verification gate for the Linux port — fmt, clippy with -D warnings, core tests, workspace tests, and the core dependency audit — and report results faithfully. Use before every commit to crates/, and whenever asked whether the build is green.
---

# Verify the Rust workspace

Run all of these from the repo root and report each result. Do not stop at the
first failure; collect everything, then report.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p video-coach-core
```

**Workspace tests need GStreamer dev headers.** Check first:

```bash
pkg-config --modversion gstreamer-1.0
```

If present, also run `cargo test --workspace`. If absent, say so explicitly —
"core green; workspace tests not run (no GStreamer dev headers)" — rather than
reporting the workspace as green. Installing the headers needs `sudo`; ask
before doing it.

**Core dependency audit.** `video-coach-core` must have no media dependency
and no date crate:

```bash
cargo tree -p video-coach-core --edges normal --depth 1
```

Expected direct dependencies: `serde`, `serde_json`, `thiserror`, `uuid`.
Anything else — especially `gstreamer*`, an image, font, or date crate — is a
failure, even if everything compiles. CI enforces this by running core tests
on a runner without GStreamer.

## Reporting

- Give the test count per suite and the total.
- If a test fails, show the assertion output, not just the name.
- If clippy flags something you believe is intentional, don't silence it
  quietly: use `#[allow(lint, reason = "...")]` and mention it.
- Never report "green" for a step you didn't run.
