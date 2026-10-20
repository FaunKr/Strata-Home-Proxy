#!/usr/bin/env bash
# Runs a test file many times to catch flaky or hanging tests. A run over
# the time limit counts as hanging and names the tests that did not
# finish.
# Usage: scripts/repeat-test.sh <package> <test file> <count> [filter…]
# Example: scripts/repeat-test.sh strata-nas-proxy settings_page 100
# Time limit per run of the whole file in seconds: LIMIT (default 60).
set -euo pipefail
cd "$(dirname "$0")/.."
if (($# < 3)); then
  echo "Usage: scripts/repeat-test.sh <package> <test file> <count> [filter…]" >&2
  echo "Example: scripts/repeat-test.sh strata-nas-proxy settings_page 100" >&2
  exit 2
fi
pkg=$1 test=$2 count=$3
shift 3
# As cargo test does: from the package's directory.
read -r bin dir < <(cargo test -q -p "$pkg" --test "$test" --no-run --message-format=json \
  | jq -r 'select(.executable != null and .target.kind == ["test"]) | "\(.executable) \(.manifest_path | rtrimstr("/Cargo.toml"))"' | tail -n1)
cd "$dir"
all=$("$bin" --list "$@" 2>/dev/null | sed -n 's/: test$//p' | sort)
failed=0 hung=0
for i in $(seq "$count"); do
  rc=0
  out=$(timeout "${LIMIT:-60}" "$bin" "$@" 2>&1) || rc=$?
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
