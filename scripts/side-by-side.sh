#!/usr/bin/env bash
# Runs a crate's integration test binary several times at once, to bring
# out timing failures that a single run at 32 threads does not show. Waits
# for all runs and names the failed tests of each.
# Usage: scripts/side-by-side.sh <package> [runs] [threads]
# Example: scripts/side-by-side.sh strata-nas-proxy 3 32
set -euo pipefail
cd "$(dirname "$0")/.."
if (($# < 1)); then
  echo "Usage: scripts/side-by-side.sh <package> [runs] [threads]" >&2
  exit 2
fi
pkg=$1 runs=${2:-3} threads=${3:-32}
cargo test -q -p "$pkg" --test integration --no-run
logs=$(mktemp -d)
pids=()
for i in $(seq "$runs"); do
  cargo test -q -p "$pkg" --test integration -- --test-threads "$threads" >"$logs/$i.log" 2>&1 &
  pids+=($!)
done
failed=0
for i in $(seq "$runs"); do
  if ! wait "${pids[$((i - 1))]}"; then
    failed=$((failed + 1))
    echo "Run $i failed: $(sed -n 's/^\(.*\) --- FAILED$/\1/p' "$logs/$i.log" | sort -u | tr '\n' ' ')"
  fi
done
echo "$runs runs side by side: $failed failed (logs in $logs)"
[ "$failed" -eq 0 ]
