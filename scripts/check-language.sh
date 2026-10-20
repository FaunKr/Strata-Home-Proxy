#!/usr/bin/env bash
# Fails when a tracked text file contains ä, ö, ü, Ä, Ö, Ü or ß, or a common
# German word (below): the repo is English (GLOSSARY.md). Prints file:line for
# each hit.
#
# Usage: scripts/check-language.sh [repo]
# Files that need German are listed in scripts/language-allowlist, one path
# per line; a single line elsewhere is allowed with the marker
# "language-check: allow" on it, e.g. a German parser input in a test. In
# Rust, prefer an escape ("drei\u{df}ig"): rustfmt can move a trailing marker
# comment off its line.
# CI runs this in the changes job, so docs-only pull requests are checked too.
set -euo pipefail
cd "${1:-$(dirname "$0")/..}"

# Function words that German without umlauts still carries ("nicht lesbar").
# Only words that are not also English: "die", "an" and "mit" (MIT) are not
# here.
words='nicht|wird|wurde|werden|kein|keine|oder|und|beim|noch|schon|bitte|sind|auch|nur|wenn|dann|eine|einen|ist|auf|von|bei|aber|fuer|ueber|gibt|kann|muss|soll|hat|wir|ich|dass|weil|nach|wie|der|das|dem|den|des'

excludes=()
if [[ -f scripts/language-allowlist ]]; then
  while IFS= read -r path; do
    [[ -z $path || $path == \#* ]] && continue
    excludes+=(":(exclude)$path")
  done <scripts/language-allowlist
fi

# git grep exits 1 when nothing matches; anything above that is an error.
grep_tracked() {
  local status=0
  git grep -I -n "$@" -- . ${excludes[@]+"${excludes[@]}"} || status=$?
  if ((status > 1)); then
    exit "$status"
  fi
}

found=$(
  grep_tracked -F -e ä -e ö -e ü -e ß -e Ä -e Ö -e Ü
  grep_tracked -i -w -E "$words"
)
hits=$(grep -v 'language-check: allow' <<<"$found" | sort -u || true)
if [[ -n $hits ]]; then
  echo "German in tracked files (the repo is English; see GLOSSARY.md):" >&2
  echo "$hits" >&2
  exit 1
fi
