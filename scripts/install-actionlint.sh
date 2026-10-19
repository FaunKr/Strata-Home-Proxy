#!/usr/bin/env bash
# Installs the pinned actionlint into the given directory (default ~/.local/bin),
# with its checksum. The CI test job and the cloud session hook both use it,
# so the version lives in one place.
set -euo pipefail
version=1.7.7
sum=023070a287cd8cccd71515fedc843f1985bf96c436b7effaecce67290e7e0757
dir=${1:-$HOME/.local/bin}
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL -o "$tmp" "https://github.com/rhysd/actionlint/releases/download/v$version/actionlint_${version}_linux_amd64.tar.gz"
echo "$sum  $tmp" | sha256sum -c --quiet
mkdir -p "$dir"
tar xzf "$tmp" -C "$dir" actionlint
