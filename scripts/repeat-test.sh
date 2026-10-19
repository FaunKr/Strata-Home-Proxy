#!/usr/bin/env bash
# Lässt eine Testdatei viele Male laufen, um wackelnde oder hängende Tests
# zu fassen. Ein Lauf über dem Zeitlimit zählt als hängend und nennt die
# Tests, die nicht fertig wurden.
# Aufruf: scripts/repeat-test.sh <paket> <testdatei> <anzahl> [filter…]
# Beispiel: scripts/repeat-test.sh strata-nas-proxy settings_page 100
# Zeitlimit pro Lauf der ganzen Datei in Sekunden: LIMIT (Vorgabe 60).
set -euo pipefail
cd "$(dirname "$0")/.."
if (($# < 3)); then
  echo "Aufruf: scripts/repeat-test.sh <paket> <testdatei> <anzahl> [filter…]" >&2
  echo "Beispiel: scripts/repeat-test.sh strata-nas-proxy settings_page 100" >&2
  exit 2
fi
pkg=$1 test=$2 count=$3
shift 3
# Wie cargo test: aus dem Verzeichnis des Pakets.
read -r bin dir < <(cargo test -q -p "$pkg" --test "$test" --no-run --message-format=json \
  | jq -r 'select(.executable != null) | "\(.executable) \(.manifest_path | rtrimstr("/Cargo.toml"))"' | tail -n1)
cd "$dir"
all=$("$bin" --list "$@" 2>/dev/null | sed -n 's/: test$//p' | sort)
failed=0 hung=0
for i in $(seq "$count"); do
  rc=0
  out=$(timeout "${LIMIT:-60}" "$bin" "$@" 2>&1) || rc=$?
  if [ "$rc" -eq 124 ]; then
    hung=$((hung + 1))
    done_=$(sed -n 's/^test \(.*\) \.\.\. .*/\1/p' <<<"$out" | sort)
    echo "Lauf $i hängt in: $(comm -23 <(echo "$all") <(echo "$done_") | tr '\n' ' ')"
  elif [ "$rc" -ne 0 ]; then
    failed=$((failed + 1))
    echo "Lauf $i rot: $(sed -n '/^failures:$/,/^test result/s/^    \([^ ]*\)$/\1/p' <<<"$out" | sort -u | tr '\n' ' ')"
  fi
done
echo "$count Läufe: $failed rot, $hung hängend"
[ "$failed" -eq 0 ] && [ "$hung" -eq 0 ]
