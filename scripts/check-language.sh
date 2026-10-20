#!/usr/bin/env bash
# Fails when a tracked text file contains ä, ö, ü, Ä, Ö, Ü or ß: the repo is
# English (GLOSSARY.md). Prints file:line for each hit.
#
# Usage: scripts/check-language.sh [repo]
# Files that need those characters are listed in scripts/language-allowlist,
# one path per line; a single line elsewhere is allowed with the marker
# "language-check: allow" on it, e.g. a German parser input in a test.
# CI runs this in the changes job, so docs-only pull requests are checked too.
set -euo pipefail
cd "${1:-$(dirname "$0")/..}"

excludes=()
if [[ -f scripts/language-allowlist ]]; then
  while IFS= read -r path; do
    [[ -z $path || $path == \#* ]] && continue
    excludes+=(":(exclude)$path")
  done <scripts/language-allowlist
fi

# -I skips binary files, -F matches the characters as plain text. git grep
# exits 1 without a match; anything above is an error, not a clean repo.
status=0
found=$(git grep -I -n -F -e ä -e ö -e ü -e ß -e Ä -e Ö -e Ü -- . ${excludes[@]+"${excludes[@]}"}) ||
  status=$?
if ((status > 1)); then
  exit "$status"
fi
hits=$(grep -v 'language-check: allow' <<<"$found" || true)
if [[ -n $hits ]]; then
  echo "German characters in tracked files (the repo is English; see GLOSSARY.md):" >&2
  echo "$hits" >&2
  exit 1
fi
