#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md -- the release notes for that
# version, without its heading. Exits non-zero if the section is missing or
# empty, so a forgotten changelog entry is a failure rather than empty notes.
#
#   scripts/release-notes.sh 0.5.0
set -euo pipefail

version="${1:?usage: $0 <version>, e.g. $0 0.5.0}"
changelog="$(dirname "$0")/../CHANGELOG.md"

# The heading match is literal -- index(), never a regex. A version number is
# mostly dots, and `0.1.0` read as a pattern would match `0a1b0` too. The
# section ends at the next `## ` heading, so a section must not contain one
# inside a fenced code block.
notes=$(awk -v v="$version" '
  index($0, "## [" v "]") == 1 { inside = 1; next }
  inside && index($0, "## ") == 1 { exit }
  inside { print }
' "$changelog")

# Empty is a failure, not empty notes: a whitespace-only section means someone
# cut a release without writing down what changed.
if [[ -z ${notes//[[:space:]]/} ]]; then
  echo "CHANGELOG.md has no notes under '## [$version]' (missing or empty section)" >&2
  exit 1
fi

printf '%s\n' "$notes"
