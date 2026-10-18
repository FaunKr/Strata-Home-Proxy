#!/usr/bin/env bash
# Automatic update with rollback for the Strata Home Proxy on TrueNAS SCALE 24.10+.
# Run as root from a TrueNAS Cron Job every 15 minutes. Step by step: README.md, section
# "Automatische Updates (TrueNAS)". Holds no secrets: an optional Home Assistant webhook URL
# comes from the environment or from the file <base dir>/auto-update.webhook.
#
# Flow: pull the image; same image ID as the running container -> quiet exit. Otherwise back up
# the data directory, redeploy through the TrueNAS middleware, wait for health. On failure: stop,
# restore the backup, retag the old image as :latest, redeploy without pulling, remember the
# failed image ID in the bad-images file and tell Home Assistant.
#
# Rolling back relies on `pull_policy: missing` in the app's YAML (deploy/truenas-app.yaml):
# with `always`, the redeploy would pull the failed image again.
set -euo pipefail
umask 077

APP_NAME="${STRATA_PROXY_APP_NAME:-strata-proxy}"
SERVICE="${STRATA_PROXY_SERVICE:-strata-proxy}"
# Empty: the container of $SERVICE in the TrueNAS app $APP_NAME (compose project ix-$APP_NAME).
CONTAINER="${STRATA_PROXY_CONTAINER:-}"
IMAGE="${STRATA_PROXY_IMAGE:-ghcr.io/faunkr/strata-home-proxy:latest}"
# Empty: the host directory mounted at /data in the container.
DATA_DIR="${STRATA_PROXY_DATA_DIR:-}"
# The proxy runs in the host network, so its web UI answers on the NAS itself. /admin/ is
# served by the proxy alone and never wakes the PC.
HEALTH_URL="${STRATA_PROXY_HEALTH_URL:-http://127.0.0.1:8080/admin/}"
BASE_DIR="${STRATA_PROXY_BASE_DIR:-$(cd "$(dirname "$0")" && pwd)}"
LOG_FILE="${STRATA_PROXY_UPDATE_LOG:-$BASE_DIR/auto-update.log}"
LOG_MAX_BYTES="${STRATA_PROXY_UPDATE_LOG_MAX_BYTES:-200000}"
LOCK_FILE="${STRATA_PROXY_UPDATE_LOCK:-$BASE_DIR/auto-update.lock}"
BAD_IMAGES_FILE="${STRATA_PROXY_BAD_IMAGES_FILE:-$BASE_DIR/auto-update.bad-images}"
BACKUP_DIR="${STRATA_PROXY_BACKUP_DIR:-$BASE_DIR/backups}"
BACKUP_KEEP="${STRATA_PROXY_BACKUP_KEEP:-10}"
HEALTH_TIMEOUT="${STRATA_PROXY_HEALTH_TIMEOUT:-120}"
HEALTH_INTERVAL="${STRATA_PROXY_HEALTH_INTERVAL:-5}"
WEBHOOK_URL="${STRATA_PROXY_UPDATE_WEBHOOK_URL:-}"
WEBHOOK_FILE="${STRATA_PROXY_UPDATE_WEBHOOK_FILE:-$BASE_DIR/auto-update.webhook}"
PRUNE_UNTIL="${STRATA_PROXY_PRUNE_UNTIL:-168h}"
PREVIOUS_TAG="${IMAGE%:*}:previous"

log() {
  local line
  line="$(date '+%Y-%m-%d %H:%M:%S') $*"
  printf '%s\n' "$line"
  printf '%s\n' "$line" >>"$LOG_FILE" 2>/dev/null || true
}

rotate_log() {
  local size
  [ -f "$LOG_FILE" ] || return 0
  size="$(wc -c <"$LOG_FILE" | tr -d ' ')"
  if [ "$size" -gt "$LOG_MAX_BYTES" ]; then
    mv -f "$LOG_FILE" "$LOG_FILE.1"
  fi
}

short_id() {
  local id="${1#sha256:}"
  printf '%s' "${id:0:12}"
}

# --- TrueNAS middleware (24.10 "Electric Eel"): small wrappers, tests fake midclt in PATH -------

tn_stop() {
  midclt call -job app.stop "$APP_NAME" >/dev/null
}

tn_start() {
  midclt call -job app.start "$APP_NAME" >/dev/null
}

# Redeploy recreates the containers from the stored compose config. Falls back to stop + start.
# Last resort (manual): Apps -> strata-proxy -> Edit -> Save.
tn_redeploy() {
  if midclt call -job app.redeploy "$APP_NAME" >/dev/null; then
    return 0
  fi
  log "app.redeploy failed, trying app.stop + app.start"
  tn_stop || true
  tn_start
}

# --- helpers -------------------------------------------------------------------------------------

# The container ID changes with every redeploy, so this is looked up each time.
container() {
  local id
  if [ -n "$CONTAINER" ]; then
    printf '%s' "$CONTAINER"
    return 0
  fi
  id="$(docker ps -aq \
    --filter "label=com.docker.compose.project=ix-$APP_NAME" \
    --filter "label=com.docker.compose.service=$SERVICE" | head -n 1)"
  [ -n "$id" ] || return 1
  printf '%s' "$id"
}

running_image_id() {
  local id
  id="$(container)" || return 1
  docker inspect "$id" --format '{{.Image}}'
}

pulled_image_id() {
  docker image inspect "$IMAGE" --format '{{.Id}}'
}

data_mount() {
  local id
  id="$(container)" || return 1
  docker inspect "$id" --format '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Source}}{{end}}{{end}}'
}

is_bad_image() {
  [ -f "$BAD_IMAGES_FILE" ] && grep -qxF "$1" "$BAD_IMAGES_FILE"
}

mark_bad_image() {
  printf '%s\n' "$1" >>"$BAD_IMAGES_FILE"
}

# Waits until the container runs image $1, is running and the web UI answers.
wait_healthy() {
  local expected="$1" deadline id running state
  deadline=$((SECONDS + HEALTH_TIMEOUT))
  while :; do
    running="$(running_image_id 2>/dev/null || true)"
    if [ "$running" = "$expected" ] && id="$(container 2>/dev/null)"; then
      state="$(docker inspect "$id" --format '{{.State.Status}}' 2>/dev/null || true)"
      if [ "$state" = "running" ] && curl -fsS -m 5 -o /dev/null "$HEALTH_URL" 2>/dev/null; then
        return 0
      fi
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      return 1
    fi
    sleep "$HEALTH_INTERVAL"
  done
}

notify() {
  local from="$1" to="$2" rolled_back="$3" url="$WEBHOOK_URL"
  if [ -z "$url" ] && [ -f "$WEBHOOK_FILE" ]; then
    url="$(head -n 1 "$WEBHOOK_FILE")"
  fi
  [ -n "$url" ] || return 0
  curl -fsS -m 10 -X POST -H 'Content-Type: application/json' \
    -d "{\"event\":\"nas_update_failed\",\"data\":{\"app\":\"$APP_NAME\",\"from\":\"$(short_id "$from")\",\"to\":\"$(short_id "$to")\",\"rolled_back\":$rolled_back}}" \
    "$url" >/dev/null 2>&1 || log "notification failed (webhook unreachable)"
}

# Everything in DATA_DIR (settings, admin password, API keys, remembered MAC, event history) as
# one archive. The proxy writes its files atomically, so a copy while it runs is consistent.
backup_data() {
  local stamp="$1" file
  file="$BACKUP_DIR/data-$stamp.tar.gz"
  mkdir -p "$BACKUP_DIR"
  tar -C "$DATA_DIR" -czf "$file" . || return 1
  # Keep the newest $BACKUP_KEEP archives.
  find "$BACKUP_DIR" -maxdepth 1 -name 'data-*.tar.gz' | sort -r | tail -n +"$((BACKUP_KEEP + 1))" |
    while IFS= read -r old; do rm -f "$old"; done
  printf '%s' "$file"
}

# DATA_DIR is usually a dataset of its own, so its contents move, not the directory.
restore_data() {
  local backup="$1" failed="$2"
  if [ ! -s "$backup" ]; then
    log "backup file $backup is missing"
    return 1
  fi
  mkdir -p "$failed"
  find "$DATA_DIR" -mindepth 1 -maxdepth 1 -exec mv -t "$failed" {} + &&
    tar -C "$DATA_DIR" --numeric-owner -xzpf "$backup"
}

rollback() {
  local old_id="$1" new_id="$2" backup="$3" failed
  failed="$BASE_DIR/failed-data-$(date '+%Y%m%d-%H%M%S')"
  log "rolling back to $(short_id "$old_id")"
  if tn_stop &&
    restore_data "$backup" "$failed" &&
    docker tag "$old_id" "$IMAGE" &&
    tn_redeploy &&
    wait_healthy "$old_id"; then
    log "rollback done; the data of the failed image is kept in $failed"
    notify "$old_id" "$new_id" true
    return 0
  fi
  log "ROLLBACK FAILED. The proxy may be down. Data of the failed image: $failed; backup: $backup. Fix by hand (README.md, section \"Automatische Updates (TrueNAS)\")."
  notify "$old_id" "$new_id" false
  return 1
}

run() {
  local new_id old_id backup revision

  if ! old_id="$(running_image_id 2>/dev/null)"; then
    log "container of app $APP_NAME not found, nothing done"
    return 1
  fi
  if [ -z "$DATA_DIR" ] && ! DATA_DIR="$(data_mount 2>/dev/null)"; then
    DATA_DIR=""
  fi
  # A rollback moves everything out of DATA_DIR, so it must be a real directory of its own.
  if [ "${DATA_DIR#/}" = "$DATA_DIR" ] || [ "$DATA_DIR" = "/" ] || [ ! -d "$DATA_DIR" ]; then
    log "data directory not found (no /data mount?), set STRATA_PROXY_DATA_DIR; nothing done"
    return 1
  fi
  if ! docker pull -q "$IMAGE" >/dev/null 2>&1; then
    log "docker pull failed (registry unreachable or not logged in to ghcr.io?), will retry next run"
    return 0
  fi
  new_id="$(pulled_image_id)"
  if [ "$new_id" = "$old_id" ]; then
    return 0
  fi
  if is_bad_image "$new_id"; then
    log "image $(short_id "$new_id") failed before, skipping until a newer image appears"
    return 0
  fi

  log "new image $(short_id "$new_id") (running $(short_id "$old_id")), backing up $DATA_DIR"
  if ! backup="$(backup_data "$(date '+%Y%m%d-%H%M%S')")" || [ ! -s "$backup" ]; then
    log "backup failed, no update"
    return 1
  fi

  # Keep the old image under a tag so pruning never removes the rollback target.
  docker tag "$old_id" "$PREVIOUS_TAG" || true

  log "redeploying $APP_NAME"
  if ! tn_redeploy; then
    log "redeploy call failed"
    mark_bad_image "$new_id"
    rollback "$old_id" "$new_id" "$backup"
    return $?
  fi
  if wait_healthy "$new_id"; then
    revision="$(docker image inspect "$IMAGE" --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' 2>/dev/null || true)"
    log "update ok: now $(short_id "$new_id") revision ${revision:-unknown}"
    docker image prune -f --filter "until=$PRUNE_UNTIL" >/dev/null 2>&1 || true
    return 0
  fi

  log "new image $(short_id "$new_id") did not become healthy within ${HEALTH_TIMEOUT}s"
  mark_bad_image "$new_id"
  rollback "$old_id" "$new_id" "$backup"
}

main() {
  mkdir -p "$(dirname "$LOG_FILE")"
  rotate_log
  # Re-run this script under flock so two runs never overlap; 75 means "lock is held".
  if [ -z "${STRATA_PROXY_UPDATE_LOCKED:-}" ]; then
    local rc=0
    STRATA_PROXY_UPDATE_LOCKED=1 flock -n -E 75 "$LOCK_FILE" bash "$0" "$@" || rc=$?
    if [ "$rc" -eq 75 ]; then
      log "another update run is active, skipping"
      return 0
    fi
    return "$rc"
  fi
  run
}

main "$@"
