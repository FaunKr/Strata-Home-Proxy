#!/bin/sh
# Checks that the NAS can read the version from the PC service's .exe:
# the first marker followed by a version (nas-proxy/src/pc_setup.rs), and
# that it is the crate version, without a build suffix.
# Usage: check-exe-version.sh <exe> <version>
set -eu
exe=$1
version=$2
found="$(LC_ALL=C grep -ao 'strata-pc-agent-version:[!-~]\{1,63\}$' "$exe" | head -n1)"
[ "$found" = "strata-pc-agent-version:$version" ] && exit 0
echo "No version ${version} in $exe, found: '$found'" >&2
exit 1
