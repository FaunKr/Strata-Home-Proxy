#!/bin/sh
# Checks that the NAS can read the version from the PC service's .exe:
# the first marker followed by a version (nas-proxy/src/pc_setup.rs).
# The ID after the + is the start of the build ID (crates/pc-agent/build.rs
# shortens a commit hash to 7 characters).
# Usage: check-exe-version.sh <exe> <build-id>
set -eu
exe=$1
build_id=$2
found="$(LC_ALL=C grep -ao 'strata-pc-agent-version:[!-~]\{1,63\}$' "$exe" | head -n1)"
id="${found##*+}"
case "$found" in *+?*) ;; *) id="" ;; esac
case "$build_id" in
    "$id"*) [ -n "$id" ] && exit 0 ;;
esac
echo "No version with ${build_id} in $exe, found: '$found'" >&2

exit 1
