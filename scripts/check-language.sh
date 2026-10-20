#!/usr/bin/env bash
# Fails when a tracked text file contains ä, ö, ü, Ä, Ö, Ü or ß, or a common
# German word that has none of them (nicht, wird, fehlgeschlagen, ...): the
# repo is English (GLOSSARY.md). Prints file:line for each hit.
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

# German log lines like "Update fehlgeschlagen" carry no umlaut; these words
# are common in German messages and do not occur in English ones.
words='nicht|wird|kein|keine|schon|startet|gestartet|beendet|gespeichert|lesbar|fehlgeschlagen|eingesetzt|aktualisiert'
# -I skips binary files, -F matches the characters as plain text, -w -i the
# words whole and in any case. git grep exits 1 without a match; anything
# above is an error, not a clean repo.
found=""
for pattern in "-F -e ä -e ö -e ü -e ß -e Ä -e Ö -e Ü" "-w -i -E -e ($words)"; do
  status=0
  # shellcheck disable=SC2086 # the pattern holds several options on purpose
  more=$(git grep -I -n $pattern -- . ${excludes[@]+"${excludes[@]}"}) || status=$?
  if ((status > 1)); then
    exit "$status"
  fi
  found+=$more$'\n'
done
hits=$(grep -v -e 'language-check: allow' -e '^$' <<<"$found" || true)
if [[ -n $hits ]]; then
  echo "German in tracked files (the repo is English; see GLOSSARY.md):" >&2
  echo "$hits" >&2
  exit 1
fi
