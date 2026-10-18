#!/bin/bash
# Cloud sessions: builds what scripts/check.sh needs ahead, so the first full
# test run does not start cold and run into the command timeout.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR"

# Less debug info and no incremental cache keep target/ at a few GB instead
# of 30; the session's own builds use the same settings, so nothing rebuilds.
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
  echo 'export CARGO_INCREMENTAL=0' >> "$CLAUDE_ENV_FILE"
  echo 'export CARGO_PROFILE_DEV_DEBUG=line-tables-only' >> "$CLAUDE_ENV_FILE"
fi

rustup target add wasm32-unknown-unknown
cargo test --no-run --quiet
# The same arguments as scripts/check.sh, or clippy checks again; a warning
# here is for the session to fix, not a reason to fail the start.
cargo clippy --all-targets --quiet -- -D warnings || true
cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate --quiet -- -D warnings || true
