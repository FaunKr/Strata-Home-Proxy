#!/usr/bin/env bash
# Prints the release notes of the product version: the section of CHANGELOG.md
# for the version in [workspace.package] of Cargo.toml. Fails when that
# section is missing, empty or not the newest, so a version bump without its
# section fails in check.sh before the workflow Release does.
#
# Usage: scripts/release-notes.sh [--version] [repo]
#   --version  prints only the version
set -euo pipefail
only_version=false
if [[ ${1:-} == --version ]]; then
  only_version=true
  shift
fi
cd "${1:-$(dirname "$0")/..}"

version=$(awk '
  /^\[workspace\.package\]/ { inside = 1; next }
  /^\[/ { inside = 0 }
  inside && /^version *=/ { gsub(/[" ]/, ""); sub(/^version=/, ""); print; exit }
' Cargo.toml)
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "release-notes: no x.y.z version in [workspace.package] of Cargo.toml (${version:-none})" >&2
  exit 1
fi
newest=$(sed -n 's/^## //p' CHANGELOG.md | head -n 1)
if [[ $newest != "$version" ]]; then
  echo "release-notes: Cargo.toml is at $version, but the newest section of CHANGELOG.md is ${newest:-missing}: add a \"## $version\" section" >&2
  exit 1
fi
notes=$(awk -v head="## $version" '
  $0 == head { inside = 1; next }
  /^## / && inside { exit }
  inside
' CHANGELOG.md | sed -e '/./,$!d')
if ! grep -q '^- ' <<<"$notes"; then
  echo "release-notes: the section $version of CHANGELOG.md has no \"- \" line" >&2
  exit 1
fi
if [[ $only_version == true ]]; then
  echo "$version"
else
  printf '%s\n' "$notes"
fi
