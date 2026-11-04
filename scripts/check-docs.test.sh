#!/usr/bin/env bash
# Tests scripts/check-docs.sh on small repositories of its own.
set -euo pipefail
cd "$(dirname "$0")/.."
check=$PWD/scripts/check-docs.sh

failures=0
fail() {
  echo "check-docs: $1" >&2
  failures=$((failures + 1))
}
commit() {
  git -C "$1" add -A
  git -C "$1" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qm t
}

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
git -C "$dir" init -q
mkdir -p "$dir/docs"
printf '# Setup\n\n## The PC does not wake\n' >"$dir/docs/setup.md"
# shellcheck disable=SC2016 # the backticks are Markdown.
printf '[setup](docs/setup.md#the-pc-does-not-wake), [web](https://example.com/x#y)\n\n```\n[not a link](missing.md)\n```\n' >"$dir/README.md"
commit "$dir"
"$check" "$dir" 2>/dev/null || fail "a clean repo fails"

printf 'Done \xe2\x9c\x85\n' >"$dir/a.md"
commit "$dir"
if out=$("$check" "$dir" 2>&1); then
  fail "an emoji passes"
fi
[[ $out == *"a.md:1: emoji"* ]] || fail "the emoji hit does not name file and line: $out"
rm "$dir/a.md"

printf 'one\n[gone](docs/gone.md)\n' >"$dir/b.md"
commit "$dir"
if out=$("$check" "$dir" 2>&1); then
  fail "a link to a missing file passes"
fi
[[ $out == *"b.md:2: link to missing docs/gone.md"* ]] || fail "the missing file is not named: $out"
rm "$dir/b.md"

printf '[old](docs/setup.md#forgot-password)\n' >"$dir/c.md"
commit "$dir"
if out=$("$check" "$dir" 2>&1); then
  fail "a link to a missing heading passes"
fi
[[ $out == *"no heading for #forgot-password"* ]] || fail "the missing heading is not named: $out"

if ((failures > 0)); then
  exit 1
fi
echo "check-docs: all cases passed"
