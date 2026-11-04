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
repo() {
  local dir
  dir=$(mktemp -d)
  git -C "$dir" init -q
  mkdir -p "$dir/docs"
  # shellcheck disable=SC2016 # the backticks are Markdown.
  printf '# Setup\n\n## The PC does not wake\n\n## Setup\n\n## `x-strata-proxy-status` and *Releases*\n\nTitle\n=====\n' >"$dir/docs/setup.md"
  printf 'one\n' >"$dir/docs/my file.md"
  echo "$dir"
}
commit() {
  git -C "$1" add -A
  git -C "$1" -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qm t
}
# Passes on a repo with this README.md, fails naming $2 on one with $3.
passes() {
  local dir
  dir=$(repo)
  printf '%s\n' "$1" >"$dir/README.md"
  commit "$dir"
  "$check" "$dir" 2>/dev/null || fail "fails: $1"
  rm -rf "$dir"
}
fails() {
  local dir out
  dir=$(repo)
  printf '%s\n' "$1" >"$dir/README.md"
  commit "$dir"
  if out=$("$check" "$dir" 2>&1); then
    fail "passes: $1"
  elif [[ $out != *"$2"* ]]; then
    fail "does not name \"$2\": $out"
  fi
  rm -rf "$dir"
}

passes '[setup](docs/setup.md#the-pc-does-not-wake) and [web](https://example.com/x#y)'
passes '[second Setup heading](docs/setup.md#setup-1)'
passes '[code and emphasis](docs/setup.md#x-strata-proxy-status-and-releases)'
passes '[setext heading](docs/setup.md#title)'
passes '[with a title](docs/setup.md "Setup")'
passes '[angle brackets](<docs/my file.md>) and [escaped](docs/my%20file.md)'
passes '[from the root](/docs/setup.md) and [a folder](docs/)'
passes '[ref]: docs/setup.md'
passes 'plain symbols: ✓ ✗ ★ ➜'
# shellcheck disable=SC2016 # the backticks are Markdown.
passes '```
[in a fence](missing.md)
~~~
[still in it](missing.md)
```
`[inline code](missing.md)`'
passes '# Here

[same file](#here)'

fails 'Done ✅' 'README.md:1: emoji'
fails 'Waiting ⏳' 'README.md:1: emoji'
fails 'one
[gone](docs/gone.md)' 'README.md:2: link to missing docs/gone.md'
fails '[gone, with a title](docs/gone.md "t")' 'link to missing docs/gone.md'
fails '[ref]: docs/gone.md' 'link to missing docs/gone.md'
fails '[old](docs/setup.md#forgot-password)' 'no heading for #forgot-password'
fails '[same file](#nowhere)' 'no heading for #nowhere in README.md'

dir=$(repo)
printf '[untracked](docs/new.md)\n' >"$dir/README.md"
commit "$dir"
printf 'new\n' >"$dir/docs/new.md"
if "$check" "$dir" 2>/dev/null; then
  fail "a link to an untracked file passes"
fi
printf 'caf\xe9\n' >"$dir/latin1.md"
git -C "$dir" add latin1.md
out=$("$check" "$dir" 2>&1) || true
[[ $out == *"latin1.md: not UTF-8"* ]] || fail "a file that is not UTF-8 is not named: $out"
rm -rf "$dir"

if ((failures > 0)); then
  exit 1
fi
echo "check-docs: all cases passed"
