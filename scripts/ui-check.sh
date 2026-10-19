#!/usr/bin/env bash
# Checks the web UI in a real browser: builds it with cargo-leptos, starts the
# proxy with an empty DATA_DIR, a fake Messsteckdose (scripts/fake-shelly.js)
# and a fake awake PC (scripts/fake-pc.js) and runs scripts/ui-check.js against
# every area,
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
# A finished code benchmark, as the proxy keeps it.
cp scripts/ui-check-code-bench.json "$data/code-bench.json"
cargo leptos build --project admin-ui >"$data/build.log" 2>&1 || { cat "$data/build.log"; exit 1; }

port="${UI_CHECK_PORT:-18099}"
plug_port=$((port - 1))
pc_port=$((port - 2))
node scripts/fake-shelly.js "$plug_port" &
plug=$!
node scripts/fake-pc.js "$pc_port" &
pc=$!
trap 'kill $plug $pc 2>/dev/null; rm -rf "$data"' EXIT
LISTEN_ADDR="127.0.0.1:$port" DATA_DIR="$data" LEPTOS_SITE_ROOT=target/site \
  SHELLY_URL="127.0.0.1:$plug_port" CODE_BENCH_URL="http://127.0.0.1:$pc_port" \
  STRATA_URL="http://127.0.0.1:$pc_port" PC_AGENT_URL="http://127.0.0.1:$pc_port" PC_MAC=10:20:30:40:50:60 \
  target/debug/strata-nas-proxy >"$data/proxy.log" 2>&1 &
proxy=$!
trap 'kill $proxy $plug $pc 2>/dev/null; rm -rf "$data"' EXIT

for _ in $(seq 50); do
  curl -fs -o /dev/null "http://127.0.0.1:$port/admin" && break
  sleep 0.2
done

mkdir -p target/ui-check
BASE="http://127.0.0.1:$port" OUT=target/ui-check node scripts/ui-check.js
