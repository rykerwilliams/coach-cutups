# Docs and Releases — Plan

**Date:** 2026-09-22
**Spec:** `docs/superpowers/specs/2026-09-22-docs-and-releases-design.md`
**Branch:** `claude/docs` (worktree `.claude/worktrees/docs`), off `origin/main` = `1213305`.
**Status:** Draft, before review.

**Execution.** A fresh subagent runs each task, given this plan, the spec and `CLAUDE.md`. The orchestrator commits each task, and does every outward step itself (pushes to `main`, tags, Pages enablement), because each is public.

## Known facts

**Repo and CI**
- **`main` moves only by fast-forward from `claude/docs`,** and only from this track. The Linux session works on `claude/intelligent-lamport-m2indd` and rebases onto `main` itself. Before any push to `main`, check that `origin/main` is still an ancestor of `claude/docs`. If it isn't, rebase `claude/docs` and re-run checks.
- **Files not to touch:** `docs/hands-on-checklist.md`, `docs/superpowers/{specs,plans}/2026-09-22-match-vision*`, `BACKLOG.md`. In CLAUDE.md, edit only the Packaging → Releasing bullet (lines ~208-220).
- **Tags:** tag only `v0.1.0`. The Linux session owns `0.1.1` and `0.2.0`.
- **The shape of `release.yml` today:**
  - Triggers: a `v*` tag, or `workflow_dispatch`.
  - Jobs:
    - `test` calls `rust.yml`;
    - `version` checks the tag against `cargo metadata`'s `video-coach-app` version;
    - `package` builds the `.deb` and smoke-tests it;
    - `release` runs `gh release create … --generate-notes`, on a tag push only. It has no checkout step.
  - A dispatch runs everything except `release`. That is how the pipeline is checked without releasing, and it takes about 15 minutes.
- **Machine rule (shared with other sessions):** wrap every cargo command as `flock /tmp/claude-1000/cargo.lock nice -n 19 cargo … -j 4`. Prefer CI for heavy builds.

**Tools**
- **mdBook:** pin the current 0.4.x or 0.5.x release. Check which is current and whether `create-missing` and `site-url` are unchanged in it.
- **lychee:** `lycheeverse/lychee-action` or `taiki-e/install-action`, pinned.
- **Pages:** `actions/upload-pages-artifact` and `actions/deploy-pages` at their current major versions.
- Install nothing system-wide locally without sudo. Local verification of the book may use a downloaded `mdbook` binary in the scratchpad.

---

## R1 — Release

### Task 1: `CHANGELOG.md`, with v0.1.0 curated

- **Format: Keep a Changelog 1.1.0.** The header paragraph names Keep a Changelog and Semantic Versioning.
- **`## [Unreleased]`** is empty.
- **`## [0.1.0] - <release date>`:** use a placeholder date if the tag day isn't known yet; Task 4 fixes it. Group it under `Added`, written for a coach, 10–20 lines. It is the first Linux release.
  - **The source material** is the README's "What it does" and the hands-on checklist's sections: projects, scanning and multiple sources, recording commentary with drawing and zoom, clips, tags, notes, filtering and undo, the scoreboard and match clock, transcripts, export, and the `.deb` install.
  - **Say what a coach can do.** Keep implementation details out: no GStreamer, no VA-API.
  - One line may note the platform: Ubuntu 24.04 / Linux Mint 22, x86-64.
- **Link references:** `[Unreleased]: https://github.com/rykerwilliams/coach-cutups/compare/v0.1.0...HEAD` and `[0.1.0]: https://github.com/rykerwilliams/coach-cutups/releases/tag/v0.1.0`.

**Done when** the file validates against the format by eye, and `scripts/release-notes.sh 0.1.0` (Task 2) prints the section.

### Task 2: Release notes from the changelog

1. **`scripts/release-notes.sh <version>`** prints the body of `## [<version>]` from `CHANGELOG.md`: the lines after its heading, up to the next `## ` heading or the link references. It exits non-zero if the section is missing or empty.
   - Plain bash and awk.
   - Test it on: the real file; a missing version; an empty section; a version that is a prefix of another (`0.1.1` against `0.1.10`); and the last section before the link references.
2. **`release.yml`:**
   - The `version` job adds `actions/checkout` if it lacks one (it has one) and runs `scripts/release-notes.sh "$version" > /dev/null` for tag refs. **It also runs it for dispatches**, so a dispatch proves the changelog is ready for the workspace version.
   - The `release` job adds `actions/checkout@v4`, then `scripts/release-notes.sh "${GITHUB_REF_NAME#v}" > notes.md`, and runs `gh release create … --notes-file notes.md` in place of `--generate-notes`.
   - Update the header comment: the notes come from `CHANGELOG.md`.

**Done when** the script's tests pass locally, `actionlint` passes on `release.yml` if it can be run without sudo (otherwise say so), and the diff is minimal.

### Task 3: README install text, and CLAUDE.md's release steps

- **README "Install":**
  - Remove "there are none published yet…".
  - The text becomes: download `coach-cuts_<version>_amd64.deb` from the [latest release](https://github.com/rykerwilliams/coach-cutups/releases/latest), then `sudo apt install ./coach-cuts_*_amd64.deb`.
  - Keep the rest of the README as it is (D2 reshapes it).
- **CLAUDE.md's Releasing bullet** is rewritten to the spec's four steps:
  1. choosing the number by semver, including that a `formatVersion` bump is at least a minor bump;
  2. in one commit, turning `[Unreleased]` into `## [x.y.z] - YYYY-MM-DD`, adding a fresh `[Unreleased]`, fixing the link references, and bumping `[workspace.package] version`;
  3. merging it to `main`;
  4. `git tag v<version> && git push origin v<version>`.
  - **Add the rule:** any commit that changes what a coach sees adds a plain-language line under `[Unreleased]`.
  - Keep the existing facts about dispatch and the cache that are still true.
  - Keep it tight: it is read by agents on every session.

**Done when** the README and CLAUDE.md diffs are confined to those two places.

### Task 4: Ship v0.1.0 (orchestrator)

1. Set the `[0.1.0]` date to today and commit.
2. **Dispatch `release.yml` on `claude/docs`** (`gh workflow run release.yml --ref claude/docs`), and wait for green. That proves the gate, the changelog check and the package, without releasing.
3. **Show the user the rendered notes** (`scripts/release-notes.sh 0.1.0`) and get their go.
4. Check that `origin/main` is an ancestor of `claude/docs`, then fast-forward `main` with `git push origin claude/docs:main`.
5. `git tag -a v0.1.0 -m "Coach Cuts 0.1.0" origin/main && git push origin v0.1.0`.
6. **Watch the run.** On success, check that the Release page shows the `.deb` and the notes, with no stray headings.

**Done when** `https://github.com/rykerwilliams/coach-cutups/releases/tag/v0.1.0` has the `.deb` and the notes.

---

## D1 — Site and checks

### Task 5: The mdBook skeleton

- **`docs/book/book.toml`:**
  - the title, "Coach Cuts";
  - `[build] create-missing = false`;
  - `[output.html] site-url = "/coach-cutups/"`, `git-repository-url`, and `edit-url-template` pointing at `main`.
- **`docs/book/src/SUMMARY.md`:** Introduction, a "Guide" section (a single placeholder chapter until D2), Changelog, and Developers.
- **`index.md`:** what Coach Cuts is (two paragraphs) and "Download the latest release" (link).
- **`changelog.md`:** only `{{#include ../../../CHANGELOG.md}}`. Check that mdBook renders Keep a Changelog's link references.
- **`developers.md`,** per the spec:
  - orientation: the four crates, one sentence each; the Linux port spec on GitHub;
  - links to `README.md#build-from-source`, `CLAUDE.md` and the `docs/superpowers` tree on GitHub;
  - the rustdoc links `api/video_coach_core/index.html`, `api/video_coach_media/index.html` and `api/video_coach_app/index.html`, relative to the page's depth.
- **`.gitignore`:** `docs/book/book/` (the build output).

**Done when** `mdbook build docs/book` succeeds locally with a scratchpad binary and the pages look right in a browser. Share a screenshot if possible.

### Task 6: `docs.yml`, the build and deploy

`.github/workflows/docs.yml`:

- **Triggers:** `push: branches: [main]`, `pull_request`, `workflow_dispatch`. No path filter.
- **Header comment:**
  - what the workflow does;
  - the one-time Pages enablement command;
  - "a private repo needs a paid plan for Pages";
  - "deploy waits on `build` only; `check` is advisory unless made a required check".
- **Job `build`:**
  - checkout; the pinned mdbook;
  - `sudo apt-get install -y --no-install-recommends libfontconfig1-dev`;
  - `dtolnay/rust-toolchain@1.92`, the workspace MSRV, and `Swatinem/rust-cache@v2`;
  - `mdbook build docs/book`;
  - `DOCS_RS=1 RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps --document-private-items --exclude video-coach-harness`;
  - copy `target/doc` into `docs/book/book/api`;
  - `actions/upload-pages-artifact` with `path: docs/book/book`, on `github.event_name == 'push'` only;
  - `timeout-minutes`, per repo convention.
- **Job `deploy`:**
  - `needs: build`, `if: github.event_name == 'push' && github.ref == 'refs/heads/main'`;
  - `permissions: {pages: write, id-token: write}`;
  - `environment: {name: github-pages, url: ${{ steps.deployment.outputs.page_url }}}`;
  - `concurrency: {group: pages, cancel-in-progress: false}`;
  - `actions/deploy-pages` with `id: deployment`.
- **The workflow's top-level `permissions: contents: read`.**
- **If `DOCS_RS=1` doesn't cover a build script** (CI shows which), try the smallest fix first: one more `-dev` package. The fallback is `-p video-coach-core` only, noting it on `developers.md`.
- **Fix any broken intra-doc links rustdoc reports** in `crates/`. These are doc-comment-only edits, under the lock wrapper when running locally.

**Local proof:** run the `cargo doc` command under the flock wrapper with `DOCS_RS=1`, once. This checks both the build-script claim and the lint before CI.

**Done when** the local `cargo doc` passes and the workflow is written. CI proof is Task 8.

### Task 7: The `check` job

Add job `check` to `docs.yml`, which runs in parallel with `build`, **not** before `deploy`:

1. **lychee, offline,** over `README.md`, `CLAUDE.md`, `CHANGELOG.md` and `docs/book/src/**/*.md`, with `--include-fragments`. Markdown link targets (repo paths and anchors) must exist.
   - Also run lychee over the built HTML, excluding `api/` and `404.html`, which means this job builds the book too (cheap: `mdbook build`), or `build` uploads it as an artifact for `check`. Choose the simpler of the two.
2. **The backtick path check** is a short script, `scripts/check-doc-paths.sh`, which CI runs and people can run locally.
   - It extracts backticked tokens from the same Markdown files and keeps those that start with `crates/`, `docs/`, `packaging/`, `scripts/`, `apple/`, `.github/` or `.claude/skills/`.
   - It skips tokens containing a space, `$`, `~`, `<`, `>` or `*`.
   - It strips a trailing `:line` or `#anchor`.
   - It fails, listing every token that doesn't exist.

**Done when:**
- Both checks pass on the branch. Fix real findings in README/CLAUDE.md, where confined to broken paths. A finding inside the Linux session's areas of CLAUDE.md is reported, not fixed.
- **Each check fails on a planted error:** a bogus `crates/nope.rs` backtick, and a broken `#anchor` link. Show the failing output, then remove the planted errors.

### Task 8: Go live (orchestrator)

1. Enable Pages: `gh api -X POST repos/rykerwilliams/coach-cutups/pages -f build_type=workflow`. This is one-time and public.
2. Push `claude/docs` and open no PR (the repo uses none); dispatch `docs.yml` on the branch to see `build` and `check` go green. `deploy` is skipped off `main`.
3. Fast-forward `main` after the ancestor check, and watch `docs.yml` deploy.
4. Open `https://rykerwilliams.github.io/coach-cutups/`. Check the changelog page, the rustdoc links and the 404 page's styling.

**Done when** the site is live and every link on `developers.md` works.

---

## D2 — User guide

### Task 9: Write the guide

`docs/book/src/guide/` holds `install.md`, `first-project.md`, `recording.md`, `clips.md`, `scoreboard.md`, `transcripts.md`, `export.md`, `keyboard.md` and `troubleshooting.md`, and SUMMARY lists them.

- **Audience:** a coach who has never opened the app. Task-first ("To tag a goal, press Z"), short pages, no internals.
- **Source material, all verified:**
  - the README's user sections;
  - `docs/hands-on-checklist.md`, the most accurate behaviour reference;
  - the app's `.slint` files and key handling in `crates/video-coach-app/`, for exact labels and shortcuts. **Every key and button label is checked against the code, not only the checklist.**
- **`keyboard.md`** is one table built from the code's key handling. Note that fast playback (J/L) is coming in 0.1.1 and is not documented yet.
- **`troubleshooting.md`** carries the README's "When something goes wrong", plus the hardware-decode line check, written for a coach.
- **`install.md`** carries the README's requirements and install steps, plus "Transcription and the network".

**Done when** `mdbook build` succeeds, the `check` job's two checks pass locally, and every shortcut in `keyboard.md` has a code reference in the task report.

### Task 10: Trim the README

- The README keeps: the pitch (3–4 lines); **Install**, meaning Task 3's text plus a short requirements list; a "Docs" line linking to the guide, changelog and developer pages; **Build from source**, unchanged; the macOS original; the licence.
- It loses "What it does" in detail, "Transcription and the network" and "When something goes wrong". Each is now in the guide, and the README links to the page.
- **`packaging/build-deps.txt:2`'s "pointed at by the README" must stay true.**

**Done when** the checks pass and every removed paragraph has a home in the guide. The task report gives the mapping.

### Task 11: Ship D2 (orchestrator)

Ancestor check, fast-forward `main`, and watch the deploy. Spot-check the guide on the live site.

---

## After execution

- **Adversarial review of the shipped changes** (CLAUDE.md step 7): the workflows, the scripts, CLAUDE.md's release text, and the guide's accuracy against the code.
- **Items to backlog:** a generated keyboard table (spec non-goal); making `docs/check` a required status check; versioned docs. The BACKLOG belongs to the Linux session, so send them the entries, or append in a separate commit after telling them.
