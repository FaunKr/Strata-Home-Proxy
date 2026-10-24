#!/usr/bin/env bash
# Runs a test file many times to catch flaky or hanging tests. A run over
# the time limit counts as hanging and names the tests that did not
# finish.
# Usage: scripts/repeat-test.sh <package> <test file> <count> [filter…]
# Example: scripts/repeat-test.sh strata-nas-proxy settings_page 100
# Time limit per run of the whole file in seconds: LIMIT (default 60).
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/timeout.sh
source scripts/timeout.sh
if (($# < 3)); then
  echo "Usage: scripts/repeat-test.sh <package> <test file> <count> [filter…]" >&2
  echo "Example: scripts/repeat-test.sh strata-nas-proxy settings_page 100" >&2
  exit 2
fi
if [[ -z $timeout ]]; then
  echo "$timeout_hint" >&2
  exit 2
fi
pkg=$1 test=$2 count=$3
shift 3
dir=$(cargo metadata --no-deps --format-version 1 \
  | jq -r --arg pkg "$pkg" '.packages[] | select(.name == $pkg) | .manifest_path | rtrimstr("/Cargo.toml")')
# A crate with tests/main.rs has one test binary, `integration`, with each
# file as a module; there a test file is the filter "<file>::".
target=$test prefix=
if [[ -f $dir/tests/main.rs ]]; then
  target=integration prefix="$test::"
fi
bin=$(cargo test -q -p "$pkg" --test "$target" --no-run --message-format=json \
  | jq -r 'select(.executable != null and .target.kind == ["test"]) | .executable' | tail -n1)
# As cargo test does: from the package's directory.
cd "$dir"
all=$("$bin" --list 2>/dev/null | sed -n 's/: test$//p' | awk -v p="$prefix" 'index($0, p) == 1' | sort)
# Arguments starting with - go to the test binary (--nocapture,
# --test-threads=1, …); the others pick tests of this file by name, as
# libtest's filters do. Without any, the whole file runs.
flags=() filters=()
for arg in "$@"; do
  if [[ $arg == -* ]]; then flags+=("$arg"); else filters+=("$arg"); fi
done
if ((${#filters[@]})); then
  all=$(awk -v p="$prefix" -v f="$(printf '%s\n' "${filters[@]}")" '
    BEGIN { n = split(f, fs, "\n") }
    { name = substr($0, length(p) + 1); for (i = 1; i <= n; i++) if (index(name, fs[i])) { print; next } }' <<<"$all")
fi
if [[ -z $all ]]; then
  echo "No test matches: $test $*" >&2
  exit 2
fi
# No mapfile: macOS has Bash 3.2.
names=()
while IFS= read -r name; do names+=("$name"); done <<<"$all"
set -- ${flags[@]+"${flags[@]}"} --exact "${names[@]}"
failed=0 hung=0
for i in $(seq "$count"); do
  rc=0
  out=$("$timeout" "${LIMIT:-60}" "$bin" "$@" 2>&1) || rc=$?
  if [ "$rc" -eq 124 ]; then
    hung=$((hung + 1))
    done_=$(sed -n 's/^test \(.*\) \.\.\. .*/\1/p' <<<"$out" | sort)
    echo "Run $i hangs in: $(comm -23 <(echo "$all") <(echo "$done_") | tr '\n' ' ')"
  elif [ "$rc" -ne 0 ]; then
    failed=$((failed + 1))
    echo "Run $i failed: $(sed -n '/^failures:$/,/^test result/s/^    \([^ ]*\)$/\1/p' <<<"$out" | sort -u | tr '\n' ' ')"
  fi
done
echo "$count runs: $failed failed, $hung hanging"

[ "$failed" -eq 0 ] && [ "$hung" -eq 0 ]
