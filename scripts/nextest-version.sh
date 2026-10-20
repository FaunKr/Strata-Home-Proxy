#!/usr/bin/env bash
# Prints the cargo-nextest version .config/nextest.toml pins. check.sh, the CI
# test job and the cloud session hook read it here, so it lives in one place.
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^nextest-version *= *{ *required *= *"\([^"]*\)" *}/\1/p' .config/nextest.toml)
if [[ -z $version ]]; then
  echo "No nextest-version = { required = \"…\" } in .config/nextest.toml" >&2
  exit 1
fi
echo "$version"
