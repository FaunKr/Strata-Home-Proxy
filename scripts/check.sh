#!/usr/bin/env bash
# The checks of the CI job `test`. CI runs this script, so local and CI agree.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --all-targets -- -D warnings
# The browser half of the web UI, which the Docker image builds with cargo-leptos.
cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate -- -D warnings
cargo test
