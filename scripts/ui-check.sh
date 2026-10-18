#!/usr/bin/env bash
# Checks the web UI in a real browser: builds it with cargo-leptos, starts the
# proxy with an empty DATA_DIR and runs scripts/ui-check.js against every area,
# at 1280 px light and 390 px dark. SHOTS=1 also saves JPEG screenshots to
# target/ui-check/. Needs cargo-leptos, Node and Playwright with Chromium.
set -euo pipefail
cd "$(dirname "$0")/.."

# Cloud sessions have Playwright here; elsewhere: npm install -g playwright.
export NODE_PATH="${NODE_PATH:+$NODE_PATH:}/opt/node-tools/node_modules:$(npm root -g 2>/dev/null || true)"
if ! node -e 'require("playwright")' 2>/dev/null; then
  echo "Playwright fehlt: npm install -g playwright && npx playwright install chromium" >&2
  exit 2
fi

data="$(mktemp -d)"
trap 'rm -rf "$data"' EXIT
cargo leptos build --project admin-ui >"$data/build.log" 2>&1 || { cat "$data/build.log"; exit 1; }

port="${UI_CHECK_PORT:-18099}"
LISTEN_ADDR="127.0.0.1:$port" DATA_DIR="$data" LEPTOS_SITE_ROOT=target/site \
  target/debug/strata-nas-proxy >"$data/proxy.log" 2>&1 &
proxy=$!
trap 'kill $proxy 2>/dev/null; rm -rf "$data"' EXIT

for _ in $(seq 50); do
  curl -fs -o /dev/null "http://127.0.0.1:$port/admin" && break
  sleep 0.2
done

mkdir -p target/ui-check
BASE="http://127.0.0.1:$port" OUT=target/ui-check node scripts/ui-check.js
