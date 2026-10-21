#!/usr/bin/env bash
# Installs cargo-nextest at the version .config/nextest.toml pins, as a
# prebuilt binary into ~/.cargo/bin: seconds instead of a cargo install.
# get.nexte.st is blocked in cloud sessions, GitHub is not. CI uses
# taiki-e/install-action with the same version (scripts/nextest-version.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(scripts/nextest-version.sh)
if [[ $(cargo nextest --version 2>/dev/null | head -n1) == "cargo-nextest $version "* ]]; then
  exit 0
fi
curl -fsSL "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$version/cargo-nextest-$version-$(uname -m)-unknown-linux-gnu.tar.gz" \
  | tar xz -C "${CARGO_HOME:-$HOME/.cargo}/bin"
