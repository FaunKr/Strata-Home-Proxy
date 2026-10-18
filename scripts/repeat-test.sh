#!/usr/bin/env bash
# Lässt eine Testdatei viele Male laufen, um wackelnde oder hängende Tests
# zu fassen. Ein Lauf über dem Zeitlimit zählt als hängend und nennt die
# Tests, die nicht fertig wurden.
# Aufruf: scripts/repeat-test.sh <paket> <testdatei> <anzahl> [filter…]
# Beispiel: scripts/repeat-test.sh strata-nas-proxy settings_page 100
# Zeitlimit pro Lauf in Sekunden: LIMIT (Vorgabe 20).
set -euo pipefail
cd "$(dirname "$0")/.."
pkg=$1 test=$2 count=$3
shift 3
bin=$(cargo test -p "$pkg" --test "$test" --no-run 2>&1 | sed -n 's/.*Executable .* (\(.*\))$/\1/p' | tail -n1)
all=$("$bin" --list "$@" 2>/dev/null | sed -n 's/: test$//p' | sort)
failed=0 hung=0
for i in $(seq "$count"); do
  rc=0
  out=$(timeout "${LIMIT:-20}" "$bin" "$@" 2>&1) || rc=$?
  if [ "$rc" -eq 124 ]; then
    hung=$((hung + 1))
    done_=$(sed -n 's/^test \(.*\) \.\.\. .*/\1/p' <<<"$out" | sort)
    echo "Lauf $i hängt in: $(comm -23 <(echo "$all") <(echo "$done_") | tr '\n' ' ')"
  elif [ "$rc" -ne 0 ]; then
    failed=$((failed + 1))
    echo "Lauf $i rot: $(grep -o '^    [a-z_0-9:]*$' <<<"$out" | tr -d ' ' | tr '\n' ' ')"
  fi
done
echo "$count Läufe: $failed rot, $hung hängend"
[ "$failed" -eq 0 ] && [ "$hung" -eq 0 ]
