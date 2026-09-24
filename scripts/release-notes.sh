#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md -- the release notes for that
# version, without its heading.
#
#   scripts/release-notes.sh 0.5.0
#
# release.yml runs it twice. The `version` job throws the output away and only
# wants the exit status, so a version bump with no changelog section fails in
# seconds instead of publishing empty notes after the whole pipeline has run.
# The `release` job writes it to the file `gh release create --notes-file`
# reads. The docs site includes the same CHANGELOG.md, so the published notes
# and the site cannot disagree.
set -euo pipefail

version="${1:?usage: $0 <version>, e.g. $0 0.5.0}"
changelog="$(dirname "$0")/../CHANGELOG.md"
[[ -f $changelog ]] || { echo "no changelog at $changelog" >&2; exit 1; }

# The heading match is literal -- index(), never a regex. A version number is
# mostly dots, and `0.1.0` read as a pattern would match `0a1b0` too. The
# section ends at the next `## ` heading, or at the link references below the
# last section.
notes=$(awk -v v="$version" '
  index($0, "## [" v "]") == 1 { inside = 1; next }
  inside && (index($0, "## ") == 1 || /^\[[^]]+\]: /) { exit }
  inside { if (!started && $0 ~ /^[[:space:]]*$/) next; started = 1; print }
' "$changelog")

# Empty is a failure, not empty notes: a whitespace-only section means someone
# cut a release without writing down what changed.
if [[ -z ${notes//[[:space:]]/} ]]; then
  echo "CHANGELOG.md has no notes under '## [$version]' (missing or empty section)" >&2
  exit 1
fi

printf '%s\n' "$notes"
