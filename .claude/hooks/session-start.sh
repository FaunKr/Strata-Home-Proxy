#!/bin/bash
# Cloud sessions: builds what scripts/check.sh needs ahead, so the first full
# test run does not start cold and run into the command timeout.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR"

rustup target add wasm32-unknown-unknown
cargo test --no-run --quiet
# The same arguments as scripts/check.sh, or clippy checks again; a warning
# here is for the session to fix, not a reason to fail the start.
cargo clippy --all-targets --quiet -- -D warnings || true
cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate --quiet -- -D warnings || true
