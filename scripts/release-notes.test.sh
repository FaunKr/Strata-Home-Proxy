#!/usr/bin/env bash
# Tests scripts/release-notes.sh on made-up repos, then on this one.
set -euo pipefail
cd "$(dirname "$0")/.."
notes=$PWD/scripts/release-notes.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

failures=0
fail() {
  echo "release-notes: $1" >&2
  failures=$((failures + 1))
}
# A repo at version $1 with the changelog on stdin.
repo() {
  rm -rf "$tmp/repo"
  mkdir "$tmp/repo"
  printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\nlicense = "MIT"\n\n[profile.dev]\nversion = "9.9.9"\n' "$1" >"$tmp/repo/Cargo.toml"
  cat >"$tmp/repo/CHANGELOG.md"
}

printf '# Changelog\n\n## 1.1.0\n\n- New thing.\n- Other thing.\n\n## 1.0.0\n\n- First.\n' | repo 1.1.0
[[ $("$notes" --version "$tmp/repo") == 1.1.0 ]] || fail "--version does not print the workspace version"
[[ $("$notes" "$tmp/repo") == $'- New thing.\n- Other thing.' ]] || fail "the notes are not the newest section"

printf '# Changelog\n\n## 1.0.0\n\n- First.\n' | repo 1.1.0
"$notes" "$tmp/repo" >/dev/null 2>&1 && fail "a bump without its section passes"

printf '# Changelog\n\n## 1.1.0\n\n## 1.0.0\n\n- First.\n' | repo 1.1.0
"$notes" "$tmp/repo" >/dev/null 2>&1 && fail "an empty section passes"

printf '# Changelog\n\n## 1.0.0\n\n- First.\n' | repo 1.0
"$notes" "$tmp/repo" >/dev/null 2>&1 && fail "a version that is no x.y.z passes"

"$notes" >/dev/null || fail "this repo's CHANGELOG.md has no section for its version"
if ((failures)); then
  exit 1
fi
