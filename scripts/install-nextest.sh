#!/usr/bin/env bash
# Installs cargo-nextest at the version .config/nextest.toml pins, as a
# prebuilt binary into ~/.cargo/bin: seconds instead of a cargo install.
# get.nexte.st is blocked in cloud sessions, GitHub is not. CI uses
# taiki-e/install-action with the same version (scripts/nextest-version.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(scripts/nextest-version.sh)
installed=$(cargo nextest --version 2>/dev/null | sed -n '1s/^cargo-nextest \([0-9.]*\).*/\1/p' || true)
# At least the pin, as check.sh asks: a newer one stays.
if [[ -n $installed && $(printf '%s\n' "$version" "$installed" | sort -V | head -n1) == "$version" ]]; then
  exit 0
fi
if [[ $(uname -s) != Linux ]]; then
  echo "install-nextest.sh is for Linux; elsewhere: cargo install cargo-nextest@$version --locked" >&2
  exit 1
fi
curl -fsSL "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$version/cargo-nextest-$version-$(uname -m)-unknown-linux-gnu.tar.gz" \
  | tar xz -C "${CARGO_HOME:-$HOME/.cargo}/bin"
