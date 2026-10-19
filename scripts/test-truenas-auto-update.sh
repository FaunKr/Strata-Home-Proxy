#!/usr/bin/env bash
# Runs deploy/truenas-auto-update.sh against a faked docker, midclt and curl: no update, a
# healthy update, a failed update with rollback, a failed rollback, a known bad image, an
# unreachable registry, a new code-bench image.
set -euo pipefail
cd "$(dirname "$0")/.."
script="$PWD/deploy/truenas-auto-update.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- fakes; their state lives in $FAKE: image IDs as files, calls in calls.log ----------------
mkdir -p "$work/bin"
cat >"$work/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -eu
echo "docker $*" >>"$FAKE/calls.log"
case "$1 ${2:-}" in
  "ps -aq")
    case "$*" in
      *service=code-bench*) [ ! -e "$FAKE/bench-running" ] || echo bid ;;
      *) echo cid ;;
    esac ;;
  "inspect bid") cat "$FAKE/bench-running" ;;
  "inspect cid")
    case "$*" in
      *.Image*) cat "$FAKE/running" ;;
      *.Mounts*) cat "$FAKE/mount" ;;
      *.State.Status*) echo running ;;
    esac ;;
  "pull -q")
    case "$3" in
      *code-bench*) cp "$FAKE/bench-registry" "$FAKE/bench-latest" ;;
      *) [ ! -e "$FAKE/registry-down" ] && cp "$FAKE/registry" "$FAKE/latest" ;;
    esac ;;
  "image inspect")
    case "$*" in
      *code-bench*) cat "$FAKE/bench-latest" ;;
      *.Id*) cat "$FAKE/latest" ;;
      *) echo abc123 ;;
    esac ;;
  "image prune") touch "$FAKE/pruned" ;;
  tag*) case "$3" in *:latest) echo "$2" >"$FAKE/latest" ;; esac ;;
  *) echo "unexpected docker call: $*" >&2; exit 1 ;;
esac
EOF
cat >"$work/bin/midclt" <<'EOF'
#!/usr/bin/env bash
set -eu
echo "midclt $*" >>"$FAKE/calls.log"
case "$3" in
  app.redeploy | app.start)
    cp "$FAKE/latest" "$FAKE/running"
    [ ! -e "$FAKE/bench-latest" ] || cp "$FAKE/bench-latest" "$FAKE/bench-running"
    # The bad image changes the data on its first start, as a broken migration would.
    if grep -qxF "$(cat "$FAKE/running")" "$FAKE/unhealthy" 2>/dev/null; then
      echo migrated >"$(cat "$FAKE/mount")/settings.json"
    fi ;;
  app.stop) ;;
esac
EOF
cat >"$work/bin/curl" <<'EOF'
#!/usr/bin/env bash
echo "curl $*" >>"$FAKE/calls.log"
case "$*" in
  *-X\ POST*) exit 0 ;;
esac
! grep -qxF "$(cat "$FAKE/running")" "$FAKE/unhealthy" 2>/dev/null
EOF
chmod +x "$work/bin/"*

fail() {
  echo "FAIL ($case_name): $*" >&2
  echo "--- calls" >&2
  cat "$FAKE/calls.log" >&2 || true
  echo "--- log" >&2
  cat "$base/auto-update.log" >&2 || true
  exit 1
}

# Fresh state for one case: running image $1, registry offers $2.
setup() {
  case_name="$3"
  export FAKE="$work/$case_name"
  base="$FAKE/base"
  mkdir -p "$FAKE/data" "$base"
  echo "$FAKE/data" >"$FAKE/mount"
  echo original >"$FAKE/data/settings.json"
  echo "sha256:$1" >"$FAKE/running"
  echo "sha256:$1" >"$FAKE/latest"
  echo "sha256:$2" >"$FAKE/registry"
  : >"$FAKE/calls.log"
}

update() {
  PATH="$work/bin:$PATH" STRATA_PROXY_BASE_DIR="$base" STRATA_PROXY_HEALTH_TIMEOUT=0 \
    STRATA_PROXY_UPDATE_WEBHOOK_URL=http://ha.local/hook bash "$script" >/dev/null
}

setup old old no-update
update || fail "exit code $?"
grep -q app.redeploy "$FAKE/calls.log" && fail "redeployed without a new image"
[ ! -e "$base/backups" ] || fail "backed up without a new image"

setup old new healthy
update || fail "exit code $?"
[ "$(cat "$FAKE/running")" = sha256:new ] || fail "not running the new image"
ls "$base/backups/"data-*.tar.gz >/dev/null 2>&1 || fail "no backup"
grep -q "tag sha256:old ghcr.io/faunkr/strata-home-proxy:previous" "$FAKE/calls.log" ||
  fail "old image not kept as :previous"
[ -e "$FAKE/pruned" ] || fail "no prune"
grep -q "update ok: now new revision abc123" "$base/auto-update.log" || fail "no success line"

setup old new rollback
echo sha256:new >"$FAKE/unhealthy"
update || fail "exit code $? after a rollback that worked"
[ "$(cat "$FAKE/running")" = sha256:old ] || fail "not back on the old image"
[ "$(cat "$FAKE/latest")" = sha256:old ] || fail ":latest not retagged to the old image"
[ "$(cat "$FAKE/data/settings.json")" = original ] || fail "data not restored"
[ "$(cat "$base"/failed-data-*/settings.json)" = migrated ] || fail "failed data not kept"
grep -qxF sha256:new "$base/auto-update.bad-images" || fail "bad image not recorded"
grep -q '"rolled_back":true' "$FAKE/calls.log" || fail "no webhook"

# The same bad image again: no second attempt.
: >"$FAKE/calls.log"
update || fail "exit code $? for a known bad image"
grep -q app.redeploy "$FAKE/calls.log" && fail "retried a known bad image"
[ "$(cat "$FAKE/latest")" = sha256:old ] || fail ":latest left on the known bad image"

setup old new rollback-fails
printf 'sha256:new\nsha256:old\n' >"$FAKE/unhealthy"
update && fail "exit code 0 after a failed rollback"
grep -q "ROLLBACK FAILED" "$base/auto-update.log" || fail "no ROLLBACK FAILED line"
grep -q '"rolled_back":false' "$FAKE/calls.log" || fail "no webhook"

setup old new base-in-data
base="$FAKE/data/update"
mkdir -p "$base"
update && fail "exit code 0 with the base directory inside the data directory"
grep -q app.redeploy "$FAKE/calls.log" && fail "redeployed with the base directory inside the data"

setup old new registry-down
touch "$FAKE/registry-down"
update || fail "exit code $? with the registry down"
grep -q app.redeploy "$FAKE/calls.log" && fail "redeployed without a pull"

setup old old bench-same
echo sha256:b1 | tee "$FAKE/bench-running" "$FAKE/bench-latest" >"$FAKE/bench-registry"
update || fail "exit code $?"
grep -q app.redeploy "$FAKE/calls.log" && fail "redeployed without a new code-bench image"

setup old old bench-new
echo sha256:b1 | tee "$FAKE/bench-running" >"$FAKE/bench-latest"
echo sha256:b2 >"$FAKE/bench-registry"
update || fail "exit code $?"
[ "$(cat "$FAKE/bench-running")" = sha256:b2 ] || fail "code-bench not on the new image"
[ ! -e "$base/backups" ] || fail "backed up for a code-bench image"

echo "truenas-auto-update: all cases passed"
