#!/usr/bin/env bash
# Checks the tracked Markdown files: no emojis (CODING_STANDARDS.md, Docs),
# and every relative link points to a tracked file, with its #anchor matching
# a heading there. Prints file:line for each hit. The check itself is
# scripts/check-docs.py.
#
# Usage: scripts/check-docs.sh [repo]
# CI runs this in the changes job, so docs-only pull requests are checked too.
set -euo pipefail
script=$(cd "$(dirname "$0")" && pwd)/check-docs.py
cd "${1:-$(dirname "$0")/..}"
python3 -I "$script"
