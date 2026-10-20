#!/usr/bin/env bash
# Tests scripts/check-language.sh on small repositories of its own.
set -euo pipefail
cd "$(dirname "$0")/.."
check=$PWD/scripts/check-language.sh

failures=0
fail() {
  echo "check-language: $1" >&2
  failures=$((failures + 1))
}
repo() {
  local dir
  dir=$(mktemp -d)
  git -C "$dir" init -q
  mkdir -p "$dir/scripts"
  echo "$dir"
}
commit() {
  git -C "$1" add -A
  git -C "$1" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qm t
}

dir=$(repo)
trap 'rm -rf "$dir"' EXIT
echo "plain English" >"$dir/a.md"
commit "$dir"
"$check" "$dir" 2>/dev/null || fail "a clean repo fails"

printf 'one\nder Verlauf ist schön\n' >"$dir/b.rs"
commit "$dir"
if out=$("$check" "$dir" 2>&1); then
  fail "an umlaut passes"
fi
[[ $out == *"b.rs:2:"* ]] || fail "the hit does not name file and line: $out"

printf '# a comment\n\nb.rs\n' >"$dir/scripts/language-allowlist"
commit "$dir"
"$check" "$dir" 2>/dev/null || fail "an allowlisted file fails"

printf 'let raw = "dreißig"; // language-check: allow\n' >"$dir/c.rs"
commit "$dir"
"$check" "$dir" 2>/dev/null || fail "a marked line fails"

printf 'Größe\n' >"$dir/untracked.md"
"$check" "$dir" 2>/dev/null || fail "an untracked file counts"
rm "$dir/untracked.md"

printf '\x00\xc3\xa4' >"$dir/blob.bin"
commit "$dir"
"$check" "$dir" 2>/dev/null || fail "a binary file counts"
plain=$(mktemp -d)
if "$check" "$plain" 2>/dev/null; then
  fail "a folder that is no git repository passes"
fi
rmdir "$plain"

if ((failures)); then
  exit 1
fi
echo "check-language: all cases passed"
