---
name: adversarial-review
description: Run this repo's adversarial review pattern (simplify + correctness agents in parallel, then grouping, deliberation, apply/skip/defer) on a spec, plan, or code change. Use at every artifact handoff required by the CLAUDE.md workflow.
argument-hint: "<spec path | plan path | diff range like main..HEAD>"
---

# Adversarial review

The process is defined in `CLAUDE.md` under "Review pattern". This skill is
the checklist for running it well.

## 1. Launch two reviewers in parallel (one message, two Agent calls)

- **Simplify agent** (`general-purpose`): "adversarial simplification review".
- **Correctness agent** (`general-purpose` for specs/plans; for code, the
  strongest code-review agent available).

Each prompt must include:
- the artifact path (or diff range) and "read it in full first";
- the reference paths to verify against — for port work, the specific Swift
  files in the `macos-reference` tag under `apple/VideoCoachCore/Sources/VideoCoachCore/`
  (read with `git show macos-reference:<path>`) and their tests;
- "verify claims against the source; do not trust the artifact";
- the **User values** block from `CLAUDE.md`, copied verbatim;
- the output format: numbered findings, most important first, each with the
  location, what's wrong, the fix, and confidence; "do not edit files".

For correctness reviews, ask the agent to label findings (fact error,
omission, bug, design risk) and to run the code where it can.

## 2. Verify before applying

Reviewers are wrong sometimes too. For every finding you plan to apply,
check the key claim yourself — read the cited lines, or reproduce the bug
with a scratch test (then delete it). Several of this project's best fixes
came from reproducing a finding; one "decodebin3 is buggy" conclusion came
from not doing so.

## 3. Group, deliberate, decide

- Group overlapping findings across the two reports. Findings both reviewers
  reached independently are strong signal.
- For contested or design-level groups, spawn a deliberation agent per group
  (in parallel) with the same user values. Skip deliberation for findings
  that are already verified and unambiguous — say that you skipped it.
- **APPLY** when strictly better, **SKIP** when the fix is worse than the
  problem, **DEFER** when it needs the user's judgment.

## 4. Close out

- Apply fixes, run the `verify` skill for code, commit with a message that
  lists what was applied and what was rejected, and why.
- Add each deferred item to `BACKLOG.md` with "Why deferred" and "When to
  revisit".
- Tell the user what was deferred and why it needs them.
