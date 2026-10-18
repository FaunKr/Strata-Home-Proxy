#!/usr/bin/env bash
# The checks of the CI job `test`. CI runs this script, so local and CI agree.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --all-targets -- -D warnings
# The browser half of the web UI, which the Docker image builds with cargo-leptos.
cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate -- -D warnings
# The PC service's Windows code, as the CI job `windows` checks it; only
# where the target is installed (rustup target add x86_64-pc-windows-gnu).
if rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-gnu; then
  cargo clippy -p strata-pc-agent --all-targets --target x86_64-pc-windows-gnu -- -D warnings
else
  echo "WARNUNG: Windows-Prüfung des PC-Dienstes übersprungen (rustup target add x86_64-pc-windows-gnu)" >&2
fi
cargo test
