# Docs and Releases — Design

**Date:** 2026-09-22
**Status:** Reviewed. Simplification and correctness passes applied.
**Branch:** `claude/docs`, a worktree off `origin/main` (`1213305`, fast-forwarded that day to the Linux port).

## Goal

1. **A docs site, published by CI on every merge to `main`.** It holds:
   - a **user guide** for coaches;
   - the **changelog**;
   - **developer docs**, meaning an orientation page plus rustdoc;
   - a **staleness check** that fails CI when the repo's docs point at things that no longer exist.
2. **GitHub Releases actually appear,** each with a `.deb` and notes a coach can read.

## Decisions (the user's, 2026-09-22)

| Question | Answer |
|---|---|
| Which docs | All four: the user guide, the changelog and release notes, the developer docs, and README/CLAUDE.md sync. |
| "Autogenerating" | **CI publishes on merge.** Docs are files in the repo, and a GitHub Action builds and publishes them whenever `main` changes. |
| Releases | **Tag when the user says.** Keep `release.yml`: bump the version, then push `v<version>`. |
| Changelog | **Keep a Changelog format plus semver**, curated by hand. v0.1.0's section is written from scratch. |
| Base | `main` is fast-forwarded to the Linux branch first. **Done:** `main` = `1213305`. |

## Starting state

- **The repo:**
  - `rykerwilliams/coach-cutups` is **public**, on the free plan, and the user is an admin.
  - Pages is not enabled (`GET /pages` returns 404).
  - There are **no releases, no tags and no PRs**. The `(#N)` suffixes on seven commits come from another repo.
  - `main` has no branch protection.
- **`README.md`** (117 lines) is the whole user guide (what it does, install, requirements, transcription, troubleshooting) and the build guide. It says "there are none published yet".
- **`docs/hands-on-checklist.md`** is a try-everything walkthrough for the user, and the most detailed description of the app's behaviour. It belongs to the Linux session.
- **`CLAUDE.md`** (333 lines) holds the agent and developer conventions, including Packaging and Releasing (lines ~181-220). `release.yml:8` points at it.
- **`docs/superpowers/`** holds 51 specs, plans and spikes, linked from nowhere.
- **Rustdoc:** every crate has a `//!` header and the code is heavily doc-commented, but nothing builds or publishes it. Most internals are private or `pub(crate)`.
- **Release notes:** `release.yml` publishes with `gh release create --generate-notes`, which builds notes from merged PRs. With no PRs, the notes would be a single "Full Changelog" link.
- **Commits:** 172 of 179 use Conventional Commit prefixes. Their subjects are written for developers (`fix(media, app): apply Phase 5 code review`) and include the macOS era.

## Design

### One site: mdBook on GitHub Pages

mdBook is the Rust ecosystem's docs tool: a single binary, Markdown in, HTML out, with search, a sidebar and themes. The site's URL is `https://rykerwilliams.github.io/coach-cutups/`. Rustdoc is published under `api/` in the same site.

```text
docs/book/
  book.toml             # site-url = "/coach-cutups/", create-missing = false,
                        # git-repository-url
  src/
    SUMMARY.md
    index.md            # what Coach Cuts is; download from releases/latest
    guide/              # user guide, for coaches
      install.md
      first-project.md
      recording.md      # commentary, drawing, zoom
      clips.md          # naming, tags, notes, filter, undo
      scoreboard.md
      transcripts.md
      export.md
      keyboard.md
      troubleshooting.md
    changelog.md        # {{#include ../../../CHANGELOG.md}}
    developers.md       # orientation plus links (below)
```

- **`create-missing = false`.** mdBook's default quietly creates an empty page for a missing SUMMARY entry. With it off, the build fails instead.
- **`site-url`** is required for a project site. Without it, the 404 page's assets break under `/coach-cutups/`.

### One home per fact

- **The README becomes a front page, with build instructions.** It keeps:
  - the pitch;
  - how to install: *download the `.deb` from [the latest release](…/releases/latest), then `sudo apt install ./coach-cuts_*_amd64.deb`*. That wording needs no edit per release, and the asset name carries the version;
  - the requirements as a short list;
  - **Build from source**, where developers look for it on GitHub, and where `packaging/build-deps.txt:2` points.

  Its user-guide sections (What it does in detail, Transcription and the network, When something goes wrong) move into the guide, and the README links there.
- **The user guide is written fresh for coaches.** The source material is the README's user sections and the hands-on checklist. The checklist itself stays as it is: it is the Linux session's, and it is a test script, not a guide.
- **`developers.md` is one page, and copies no content from CLAUDE.md.** It has:
  - two paragraphs of orientation: the crate map in a sentence each, and a link to the Linux port spec;
  - links to `README.md#build-from-source`, `CLAUDE.md` (the conventions, including the release process), the `docs/superpowers/` tree on GitHub (the design history, which already sorts by date because the filenames do), and each crate's rustdoc (`api/video_coach_core/index.html` and the others). `cargo doc` writes no top-level index, so these links are how rustdoc is reached.
- **The release process stays in CLAUDE.md.** Agents cut releases and they read CLAUDE.md, and `release.yml` points there. It gains the changelog step (below).

### Changelog: Keep a Changelog, curated, published by CI

`CHANGELOG.md` at the root follows **[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/)**, and versions follow **[Semantic Versioning](https://semver.org/)** (the user's standard).

- **Format:**
  - `## [Unreleased]` on top, then one `## [x.y.z] - YYYY-MM-DD` per release, newest first;
  - the standard groups inside each: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`;
  - **no link-reference block** (amended 2026-09-24): the versions in the file predate any tag, so every link would be dead, and the Releases page already lists what can be downloaded.
- **Written for coaches:** what's new, what's fixed, in plain language. It is not a commit log.
- **v0.1.0 is curated by hand:** the first Linux release, "what Coach Cuts does", grouped under `Added`.
- **After that, entries accrue under `[Unreleased]`.** A commit that changes what a user sees adds a line there, as Keep a Changelog intends. CLAUDE.md states the rule, so both agent sessions follow it.
- **A release** renames `[Unreleased]` to `[x.y.z] - <date>`, adds a fresh empty `[Unreleased]` and bumps `[workspace.package] version`, all in one commit.
- **Choosing the number (semver, pre-1.0):**
  - a minor bump (`0.x.0`) for new features, or for a change that breaks an existing project;
  - a patch bump (`0.x.y`) for fixes and small additions.

  A `project.json` format bump (`formatVersion`) is at least a minor bump.
- **No chicken-and-egg problem:** the section is on `main` before the tag exists, so the site already shows it.
- **The site** includes the file (see `changelog.md` above). **The release notes** are the same section, cut out by a few lines of `awk` in `release.yml`. The two can't disagree.
- **`release.yml`'s `version` job also checks that `CHANGELOG.md` has a non-empty `## [<version>]` section,** failing in seconds, like the tag check.

**Why not generate it from commits (git-cliff):**
- The subjects are written for developers, and v0.1.0's notes would be about 80 lines spanning the macOS app.
- A generated page would also list a release under "Unreleased" until the next merge, because the tag push doesn't redeploy Pages.
- The user chose curation.

### Rustdoc, the cheap way

- **The command:** `DOCS_RS=1 WHISPER_DONT_GENERATE_BINDINGS=1 cargo doc --workspace --no-deps --document-private-items --exclude video-coach-harness`. `whisper-rs-sys` runs bindgen, which needs libclang, before it checks `DOCS_RS`; the second variable makes it copy its bundled bindings instead.
- **Why `DOCS_RS=1` makes it cheap:** under it, `whisper-rs-sys` skips its CMake build, the gtk-rs `-sys` crates skip pkg-config, and `skia-bindings` uses its pre-generated bindings. The only system package left is `libfontconfig1-dev`. This is read from the build scripts, and D1's first CI run confirms it. If it doesn't hold, publish core only, which needs no GStreamer, and say so on the page.
- **`--document-private-items`,** because the useful docs are on internals.
- **The harness is excluded:** it is test scaffolding.
- **`RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links"`** makes rustdoc its own check. Whether `main` passes today is unknown, so D1 fixes whatever it finds.
- **Output:** copied to `<book>/api/`.

### CI: `docs.yml`

- **Triggers:** every PR, every push to `main`, and `workflow_dispatch`. No path filter, because the checks exist to catch a code PR that renames a file the docs name.
- **Job `build`:**
  - install pinned `mdbook` and `lychee` (e.g. `taiki-e/install-action`), plus `libfontconfig1-dev` and the Rust toolchain;
  - `mdbook build docs/book`;
  - rustdoc into `book/api/`;
  - upload the Pages artifact (`actions/upload-pages-artifact`) on `main` only.
- **Job `check`,** in parallel with `build`:
  1. **Links.** `lychee --offline --include-fragments` over the source Markdown (`README.md`, `CLAUDE.md`, `CHANGELOG.md`, `docs/book/src/**`), so linked repo paths and anchors must exist. `--remap` points `github.com/…/blob|tree/main/` URLs at local files, so the book's links into the repo are checked too. The `api/` links are excluded, and `build` checks them with `test -f`. There is no pass over the built HTML: it would mostly test mdBook's own rewriting.
  2. **Repo paths in backticks.** A few lines of shell find tokens in those files that **start with a real top-level directory** (`crates/`, `docs/`, `packaging/`, `scripts/`, `apple/`, `.github/`, `.claude/skills/`). Tokens containing a space, `$`, `~`, `<` or `*` are skipped. Every token found must exist. Crate-relative mentions (`bus/transcribe.rs`) and runtime paths (`~/.cache/...`) are deliberately out of scope. Run against today's files, this has zero false positives and still catches a renamed script or spec.
- **Job `deploy`:**
  - `needs: build` (not `check`), on a push to `main` only;
  - `permissions: pages: write, id-token: write`, `environment: github-pages`, `concurrency: {group: pages, cancel-in-progress: false}`;
  - runs `actions/deploy-pages`.
  - **Why deploy doesn't wait on `check`:** a stray path in CLAUDE.md shouldn't hold back a fixed user guide. `check` is a red mark to fix; making it a required status check is the user's call later.
- **A one-time step:** enable Pages with the Actions source, `gh api -X POST repos/rykerwilliams/coach-cutups/pages -f build_type=workflow`, before the first deploy. The workflow's header comment records this, along with the fact that a private repo needs a paid plan for Pages.

### Releases

- **The process, recorded in CLAUDE.md:**
  1. Choose the number by semver (above).
  2. In one commit, turn `[Unreleased]` into the version's section (the user reads it before saying go) and bump `[workspace.package] version`.
  3. Merge that commit to `main`.
  4. `git tag v<version> && git push origin v<version>`.
- **`release.yml` changes:**
  - the `version` job also checks the changelog section;
  - the `release` job checks out the repo (it has no checkout today), cuts the section to a file, and runs `gh release create … --notes-file` in place of `--generate-notes`.
- **The first release is `v0.1.0`,** at `main`'s current app code. The workspace version is already `0.1.0`. The Linux session plans `0.1.1` and `0.2.0`; nobody tags those ahead of their version-bump commits.

## Non-goals

- **Versioned docs.** One live site tracks `main`, and the changelog says what shipped when.
- **Generating guide prose from code.** A keyboard-shortcut table generated from the app's bindings is the one worth considering later.
- **Automatic releases on merge.** The user chose manual tags.
- **Changing the hands-on checklist, the specs, the plans or BACKLOG.**
- **Android docs.** That port is paused.

## Phasing

| Phase | Delivers | Done when |
|---|---|---|
| R1 | `CHANGELOG.md` in Keep a Changelog format, with a hand-curated `[0.1.0]` ("the first Linux release: what it does") and an empty `[Unreleased]`; the `release.yml` notes and section check; README install text against `releases/latest`; CLAUDE.md's release steps and the `[Unreleased]` rule. Then **`v0.1.0` is cut, once the user has seen the notes.** | The Release page shows the `.deb` and the section's notes. |
| D1 | The mdBook skeleton (`index`, `changelog`, `developers`), `docs.yml` (build, rustdoc, checks, deploy), Pages enabled, and any intra-doc links or paths the checks find, fixed. | The site is live at the Pages URL after a merge; each check fails on a planted error and passes on `main`. |
| D2 | The user guide; the README's user sections moved into it. | Every README user section has a home in the guide, and the README is a front page with build instructions. |

## Coordination

The Linux session (`claude/intelligent-lamport-m2indd`) is still committing.

- **Files this track leaves alone:** `docs/hands-on-checklist.md`, `docs/superpowers/{specs,plans}/2026-09-22-match-vision*` and `BACKLOG.md`.
- **CLAUDE.md is edited by both.** That session adds transport rules; this track edits only the Releasing bullet. Whoever lands second rebases.
- **A path it renames** will show up in this track's check as a red mark, which is the point of the check.

## Risks

- **`DOCS_RS=1` may not cover every build script.** Fallback: core-only rustdoc (above).
- **The intra-doc lint may need a round of fixes on `main`.** That is D1's job, and it is mostly mechanical.
- **Curation takes discipline.** The `[Unreleased]` rule spreads the work across commits, and the section check makes a forgotten changelog fail the release in seconds rather than ship empty notes.
