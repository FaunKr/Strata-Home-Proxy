#!/usr/bin/env bash
# Installs the cargo-leptos release the Dockerfile pins, checked against its
# sha256, into ~/.cargo/bin, unless cargo-leptos is there already. The version
# lives in the Dockerfile only. Used by the session start and the CI job ui.
set -euo pipefail
cd "$(dirname "$0")/.."

if cargo leptos --version >/dev/null 2>&1; then
  exit 0
fi
url=$(sed -n 's|.*\(https://github.com/leptos-rs/cargo-leptos/[^ ]*\.tar\.gz\).*|\1|p' Dockerfile)
sum=$(grep -B1 "$url" Dockerfile | sed -n 's/.*--checksum=sha256:\([0-9a-f]*\).*/\1/p')
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL -o "$tmp" "$url"
echo "$sum  $tmp" | sha256sum -c --quiet
mkdir -p "$HOME/.cargo/bin"
tar xzf "$tmp" -C "$HOME/.cargo/bin" --strip-components=1
