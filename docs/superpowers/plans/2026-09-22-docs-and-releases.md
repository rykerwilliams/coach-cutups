# Docs and Releases — Plan

**Date:** 2026-09-22
**Spec:** `docs/superpowers/specs/2026-09-22-docs-and-releases-design.md`
**Branch:** `claude/docs` (worktree `.claude/worktrees/docs`), rebased 2026-09-24 onto `claude/intelligent-lamport-m2indd` = `8dcf6d0`
**Status:** Reviewed. Simplify and correctness passes applied. Amended 2026-09-24 for the state of the repo (see "What changed since the review").

**Execution.** A fresh subagent runs each task, given this plan, the spec and `CLAUDE.md`. The orchestrator commits each task. It also does every outward step itself, because they are public: branch pushes, pushes to `main`, tags, and enabling Pages.

## What changed since the review

The plan was written against `origin/main` = `1213305`, when the workspace said `0.1.0` and the first release was to be `v0.1.0`. Two days of work on the Linux session's branch changed that:

- **The workspace is at `0.5.0`**, through six bumps: `1e125ba` 0.1.1, `0bd18b3` 0.2.0, `b58bb89` 0.2.1, `5f0d7e7` 0.3.0, `9962dc0` 0.4.0, `d458762` 0.5.0. **No tag exists**, local or on origin, so *nothing has ever been published* — those bumps are history, not releases.
- **The base is the Linux session's branch,** not `main`. `main` is six versions behind and the user installs from the branch. The Linux session will not fast-forward `main` on its own initiative; that is the user's call, and this plan's outward steps wait on it.
- **Cargo.toml belongs to the Linux session.** It keeps bumping (0.6.0 is close). This branch never edits it.
- **CLAUDE.md belongs to the Linux session too** — its agents edit it every session. The Releasing bullet is sent to that session as a diff rather than edited here.
- **The changelog is therefore retrospective:** 0.1.0 through 0.5.0, written from the Linux session's own plain-language bullets (it has the user context), collapsing 0.1.0 and 0.1.1 into one initial-release entry because neither was published.

## Known facts

**Repo and CI**
- **Merging:** `main` moves by fast-forward (`git push origin claude/docs:main`), and git refuses a non-fast-forward without `--force`. If refused, rebase `claude/docs` onto `origin/main` and re-verify.
- **The Linux session also lands on `main`.** Its branch holds the untagged 0.1.1 bump (`1e125ba`) and more after it, including a `project.json` format v8.
- **Files and tags that stay out of scope:**
  - **Files not to touch:** `docs/hands-on-checklist.md`, `docs/superpowers/{specs,plans}/2026-09-22-match-vision*` and `BACKLOG.md`.
  - **CLAUDE.md:** edit only the Packaging → Releasing bullet (lines ~208-220). CLAUDE.md is stale elsewhere; for example, it says speech models are "found and never fetched", but the code downloads them (`transcribe.rs:27,48`). Report such findings to the Linux session; don't fix them.
  - **Tags:** tag only `v0.1.0`, at a known SHA, never at a moving `origin/main`.
- **`release.yml` today:**
  - **Triggers:** a `v*` tag, or `workflow_dispatch`.
  - **Jobs:**
    - `test` calls `rust.yml`;
    - `version` checks out the repo; its version is computed inside the tag-only step (`release.yml:40-51`);
    - `package` builds the package;
    - `release` has only `download-artifact` and then `gh release create --generate-notes`.
  - **The last green run** is 35703153862, at `4ef65bf`. From there to `main` only the temporary branch trigger was removed, so the code being released is proven.
- **Dispatch needs the workflow file on the default branch** (CLAUDE.md). A new `docs.yml` therefore can't be dispatched before it reaches `main`, and there are no PRs, so the first CI run of `docs.yml` is on `main`. That's safe: `deploy` needs `build`, and Pages is empty until then.
- **Machine rule:** wrap every cargo command as `flock /tmp/claude-1000/cargo.lock nice -n 19 cargo … -j 4`.
- **New scripts** are committed executable (`git add --chmod=+x`), like the existing ones.

**Tools**
- **None installed:** `mdbook`, `lychee` and `actionlint` are not on this machine. Download the release binaries to the scratchpad; no sudo. For actionlint, `docker run --rm -v "$PWD":/repo -w /repo rhysd/actionlint` also works.
- **mdBook 0.5.x (current 0.5.4):** pin it in CI to the same version as locally. 0.5 rejects unknown `book.toml` keys, so use documented keys only. `create-missing` still exists (default true). mdBook rewrites only relative `.md` links, and `.html` links pass through.
- **lychee:** `--offline` skips http(s) links rather than failing them. `--include-fragments` checks anchors. `--remap` takes a regex. Verify each flag against the pinned version.

---

## R1 — Release

### Task 1: The changelog and release notes

1. **`CHANGELOG.md`** in Keep a Changelog 1.1.0 format. The header names Keep a Changelog and Semantic Versioning. It is **retrospective**: six sections, newest first, from the Linux session's bullets (quoted below verbatim as the source of truth for what mattered to a coach).
   - **`## [Unreleased]`** holds what is on the branch after `d458762`: a chapter list to paste into a YouTube description, and the scoreboard `.srt` confirmed to upload to YouTube as a subtitle track. Nothing else — the detection work is internal.
   - **`## [0.5.0] - 2026-09-24`** — type or paste match events instead of tagging them live, and fix a wrong one by retyping a single line. A colour picker for team kits. Goal cuts start 20 s before the goal rather than 30. Every exported file is tagged with its title, the final score, both teams and the match's date.
   - **`## [0.4.0] - 2026-09-23`** — the whole match exports as a straight copy of the original footage: minutes instead of an hour, a third of the size, no quality lost, with the scoreboard as a subtitle track beside and inside the file. Under `Fixed`: exports at every quality are much smaller (a match at Medium went from ~10 GB to ~2.7 GB) after the encoder was found to be running with no bitrate discipline, and software-only machines were quietly exporting at ~1.7 Mbit/s whatever quality was picked.
   - **`## [0.3.0] - 2026-09-23`** — record with a picture of yourself instead of the webcam: the camera is never opened, and the picture pulses as you talk, live and in the finished video.
   - **`## [0.2.1] - 2026-09-22`** — export the whole match with the clock and score burned in; a separate reel for each team's goals; chapters named for a viewer ("Kick-off", "Rovers goal 1-0", "Half time").
   - **`## [0.2.0] - 2026-09-22`** — a goals reel: every goal as one video, each cut with the build-up before it, trimmed per goal. Chapters in every exported file. Goal and period marks on the scrubber, with `[` and `]` to jump between them. Player highlights: ring a player and the ring follows the boxes you place, live, in previews and in exports.
   - **`## [0.1.0] - 2026-09-22`** is the initial release, under `Added`, written for a coach in 10–20 lines. It **absorbs 0.1.1**, which was a build made for one person on one laptop: fold in the frame step (`,` / `.`) and fast scanning (`J` / `L`, up to 32×).
     - **Sources:** the README's "What it does" and the hands-on checklist: projects, scanning several sources, recording commentary with drawing and zoom, clips, tags, notes, filtering and undo, the scoreboard and match clock, transcripts, export, and the `.deb`.
     - **What a coach can do, not how:** no GStreamer, no VA-API. One line may name the platform: Ubuntu 24.04 / Linux Mint 22, x86-64.
   - **Every section:** grouped under Keep a Changelog headings (`Added`, `Changed`, `Fixed`), plain language, and **inline links only** — a section is cut out whole for the release notes, and a reference-style link would lose its definition.
   - **No link-reference block, ever.** Every version before 0.6.0 links to a tag that will never exist, and the terminator the script would need for such a block is dead code (a release is always cut from the newest section, which the next `## ` heading ends). Headings therefore render with literal brackets, which is ordinary Keep a Changelog output for an unlinked version, and the preamble says why. The Releases page is the navigation.
   - **Honesty about the dates:** these versions were never published. The changelog records when each was cut, which is what the commit dates say; don't invent release dates.
2. **`scripts/release-notes.sh <version>`** prints the body of `## [<version>]`.
   - **The heading match is literal:** `index($0, "## [" v "]") == 1`, never a regex.
   - The body runs to the next `## ` heading or the first link-reference line (`[…]: `).
   - It reads `CHANGELOG.md` relative to the script's own directory.
   - It exits non-zero on a missing section, or on one that is empty or whitespace-only.
   - **Tests:** the real file; a missing version; an empty section; a whitespace-only section; the last section, which runs to the end of the file.
3. **`release.yml` changes:**
   - **`version` job:** one ungated step computes `version` from `cargo metadata` (moved out of the tag step) and always runs `scripts/release-notes.sh "$version" > /dev/null`. Only the tag comparison stays tag-gated.
   - **`release` job:** the **first** step is `actions/checkout@v4`, before `download-artifact`, because checkout empties a non-git workspace. Then `scripts/release-notes.sh "${GITHUB_REF_NAME#v}" > notes.md`, and `gh release create … --notes-file notes.md` in place of `--generate-notes`.
   - **The header comment** says the notes come from `CHANGELOG.md`.

**Done when** the script's tests pass, actionlint passes on `release.yml`, and the workflow diff is minimal.

### Task 2: README install text and CLAUDE.md's release steps

- **README "Install":**
  - Remove "there are none published yet…".
  - Replace it with: download `coach-cuts_<version>_amd64.deb` from the [latest release](https://github.com/rykerwilliams/coach-cutups/releases/latest), then run `sudo apt install ./coach-cuts_*_amd64.deb`.
  - Leave the rest of the README alone; D2 reshapes it.
- **CLAUDE.md's Releasing bullet** is rewritten tight, since agents read it every session. **It is not edited on this branch** — CLAUDE.md is the Linux session's, whose agents edit it several times a day. Draft the replacement text into `/tmp/claude-1000/.../scratchpad/claude-md-releasing.md`; the orchestrator sends it to that session to apply. It covers:
  - **Choosing the number by semver.** While pre-1.0, a feature or a `formatVersion` bump means a minor bump.
  - **One commit:** `[Unreleased]` becomes `## [x.y.z] - YYYY-MM-DD`; add a fresh `[Unreleased]`; bump `[workspace.package] version`. The user reads the section (`scripts/release-notes.sh x.y.z`) before saying go.
  - **Then:** merge to `main`; `git tag v<version> <sha> && git push origin v<version>`.
  - **The rule:** a commit that changes what a coach sees adds a plain-language line under `[Unreleased]`, with inline links only, **and updates the user-guide page it affects** (`docs/book/src/guide/`, once D2 lands).
  - Keep the existing facts about dispatch and the cache that still hold.

**Done when** the README and CLAUDE.md diffs are confined to those two places.

### Task 3: Ship the first real release (orchestrator)

**Which version.** The workspace says `0.5.0` and 0.6.0 is close on the Linux branch. The first published release is whichever of those the **user** picks; the machinery is identical either way. Don't tag on initiative.

**Two decisions that are the user's, put to them together:**
- whether `main` is fast-forwarded to the Linux branch first (releases should come off `main`, and `main` is six versions behind);
- whether to publish `v0.5.0` now or wait for 0.6.0.

1. **Show the user** the whole changelog and the output of `scripts/release-notes.sh <version>`, and get their go on both decisions above.
2. Check that `rust.yml` is green on the SHA being tagged. The last green `release.yml` run is 35703153862, at `4ef65bf` — **five versions old**, so it proves the pipeline, not this code.
3. **Fast-forward `main`** once the user says so, coordinating with the Linux session (it owns that branch's content): `git push origin claude/docs:main`, which carries its commits and these.
4. **Tag the known SHA**, never a moving branch: `sha=$(git rev-parse claude/docs)`, then `git tag -a v<version> -m "Coach Cuts <version>" $sha && git push origin v<version>`. The SHA's `[workspace.package] version` must equal the tag without its `v`, or the `version` job fails by design.
5. **Watch the run.**
   - If it fails before `release`, nothing is published: delete the tag (locally and on origin), fix it, and tag again.
   - On success, check that the Release page shows the `.deb` and the notes, with no stray headings.
6. **Message the Linux session:**
   - the changelog is in, and the `[Unreleased]` and guide rules are live;
   - `release.yml` now fails a version with no changelog section — a bump without one breaks its release;
   - the earlier versions stay untagged history unless the user asks otherwise.

**Done when** `…/releases/tag/v<version>` has the `.deb` and the notes.

---

## D1 — Site and checks

### Task 4: The mdBook skeleton

- **`docs/book/book.toml`:**
  - the title "Coach Cuts";
  - `[build] create-missing = false`;
  - `[output.html]`: `site-url = "/coach-cutups/"`, `git-repository-url`, and `edit-url-template = "https://github.com/rykerwilliams/coach-cutups/edit/main/docs/book/{path}"`.
- **`src/SUMMARY.md`:** Introduction, a Guide section (one placeholder chapter until D2), Changelog, Developers.
- **`index.md`:** what Coach Cuts is, in two paragraphs, and a link to the latest release.
- **`changelog.md`:** only `{{#include ../../../CHANGELOG.md}}`. Check that the file's own `# Changelog` heading doesn't collide with the chapter title.
- **`developers.md`:**
  - the four crates, one sentence each;
  - links to the Linux port spec, `README.md#build-from-source`, `CLAUDE.md` and the `docs/superpowers` tree, as **`https://github.com/rykerwilliams/coach-cutups/blob|tree/main/…` URLs** (relative repo links would be rewritten to `.html` and 404);
  - rustdoc links: `api/video_coach_core/index.html`, `api/video_coach_media/index.html`, `api/video_coach_app/index.html`.
- **`.gitignore`:** `docs/book/book/`.

**Done when** `mdbook build docs/book` succeeds with the scratchpad binary, and the output's pages and links look right.

### Task 5: `docs.yml`

`.github/workflows/docs.yml`:

- **Triggers:** `push: branches: [main]`, `pull_request`, `workflow_dispatch`. No path filter.
- **Top-level `permissions: contents: read`.**
- **Header comment:**
  - what the workflow does;
  - the one-time Pages enable command;
  - one line: a private repo needs a paid plan for Pages;
  - `deploy` doesn't wait on `check`.
- **Job `build`:**
  - checkout;
  - the pinned mdbook;
  - `sudo apt-get update && sudo apt-get install -y --no-install-recommends libfontconfig1-dev`;
  - `dtolnay/rust-toolchain@1.92` and `Swatinem/rust-cache@v2`;
  - `mdbook build docs/book`;
  - `DOCS_RS=1 WHISPER_DONT_GENERATE_BINDINGS=1 RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps --document-private-items --exclude video-coach-harness`;
  - copy `target/doc` into `docs/book/book/api`;
  - `test -f` on each of the three `api/video_coach_*/index.html` files, which is what proves `developers.md`'s rustdoc links;
  - `actions/upload-pages-artifact` (`path: docs/book/book`) when `github.ref == 'refs/heads/main' && github.event_name != 'pull_request'`;
  - `timeout-minutes`.
- **Job `deploy`:**
  - `needs: build`, under the same condition;
  - `permissions: {pages: write, id-token: write}`;
  - `environment: {name: github-pages, url: ${{ steps.deployment.outputs.page_url }}}`;
  - `concurrency: {group: pages, cancel-in-progress: false}`;
  - `actions/deploy-pages` with `id: deployment`.
- **Fallbacks, if a build script ignores `DOCS_RS` on the runner:** add the one `-dev` package it wants. Otherwise, `-p video-coach-core` only, noted on `developers.md`.
- **Fix every broken intra-doc link** rustdoc reports in `crates/`. These are doc-comment edits only.

**Local run:** once, the same `cargo doc` command under the flock wrapper. This proves the intra-doc lint and its fixes. It can't prove the build-script claim, because this machine has every dev package. CI's first run on `main` proves that.

**Done when** the local `cargo doc` passes and actionlint passes on `docs.yml`.

### Task 6: The checks

1. **The `check` job** in `docs.yml`, alongside `build` rather than before `deploy`: a checkout, then the pinned lychee over `README.md`, `CLAUDE.md`, `CHANGELOG.md` and `docs/book/src/**/*.md`:
   - `--offline --include-fragments`;
   - `--exclude '/api/video_coach_'`, which `build` covers with `test -f`;
   - `--remap 'https://github.com/rykerwilliams/coach-cutups/(blob|tree)/main/(.*) file://<workspace>/$2'`, so links into the repo and their anchors are checked offline;
   - no HTML pass.
2. **`scripts/check-doc-paths.sh`,** run by the `check` job, and **added to `.claude/skills/verify/SKILL.md`** so it runs before every commit, not only after `main` moves.
   - It collects backticked tokens in the same Markdown files that start with `crates/`, `docs/`, `packaging/`, `scripts/`, `apple/`, `.github/` or `.claude/skills/`.
   - It skips tokens containing a space, `$`, `~`, `<`, `>` or `*`.
   - It fails, listing every token that is missing.

**Done when:**
- both pass on the branch. Fix real broken paths in the README and in CLAUDE.md's Releasing bullet. Report any in the Linux session's parts of CLAUDE.md;
- each fails on a planted error: a backticked `crates/nope.rs`, a `#nope` anchor on a repo link, and a `README.md#nope` link from `developers.md` via the remap. Show the failing output, then remove the planted errors.

### Task 7: Go live (orchestrator)

1. `gh api -X POST repos/rykerwilliams/coach-cutups/pages -f build_type=workflow`. This is a one-time public step.
2. Fast-forward `main`, and watch `docs.yml`: `build`, `check` and `deploy`. If the first run fails, fix forward; nothing is published until `build` passes.
3. **Open `https://rykerwilliams.github.io/coach-cutups/` and check:**
   - the changelog page;
   - each rustdoc link;
   - the GitHub links;
   - the 404 page's styling.

**Done when** the site is live and every link on `developers.md` works.

---

## D2 — User guide

### Task 8: Write the guide

`docs/book/src/guide/` holds `install.md`, `first-project.md`, `recording.md`, `clips.md`, `scoreboard.md`, `transcripts.md`, `export.md`, `keyboard.md` and `troubleshooting.md`, all listed in SUMMARY.

- **Audience:** a coach who has never opened the app. Lead with tasks ("To tag a goal, press Z"). Short pages. No internals.
- **Sources:** the README's user sections, `docs/hands-on-checklist.md`, and the app code (`.slint` files and key handling in `crates/video-coach-app/`).
- **Check every key and button label against the code.** CLAUDE.md is not a source for user behaviour; parts are stale. Example: models **are** downloaded, per the README and `transcribe.rs`.
- **`keyboard.md`:** one table built from the code on `main`. **Don't document J/L or the `,`/`.` frame step.** They aren't on `main`, and the Linux session adds them to the guide with its own changes.
- **`troubleshooting.md`:** the README's "When something goes wrong", plus the hardware-decode log check, written for a coach.
- **`install.md`:** the requirements, the install steps, and "Transcription and the network".

**Done when:**
- `mdbook build` succeeds;
- both checks pass locally;
- the task report gives a code reference for every shortcut in `keyboard.md`.

### Task 9: Trim the README

- **It keeps:**
  - the pitch (3–4 lines);
  - Install (Task 2's text plus a short requirements list);
  - a "Docs" line linking to the guide, changelog and developer pages;
  - Build from source, unchanged;
  - the macOS original;
  - the licence.
- **It loses** the detailed "What it does", "Transcription and the network" and "When something goes wrong", each replaced by a link to its guide page.
- **`packaging/build-deps.txt:2`** ("pointed at by the README") must stay true.

**Done when** the checks pass. The task report maps every removed paragraph to its new home.

### Task 10: Ship D2 (orchestrator)

Fast-forward `main`, watch the deploy, and spot-check the guide on the live site.

---

## After execution

- **Adversarial review of the shipped changes** (CLAUDE.md step 7): the workflows, the scripts, CLAUDE.md's release text, and the guide's accuracy against the code.
- **Backlog candidates:** a keyboard table generated from the app's bindings (a spec non-goal), and versioned docs. BACKLOG belongs to the Linux session: send the entries there.
