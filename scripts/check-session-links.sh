#!/usr/bin/env bash
# Fails when the text on stdin links a Claude session or project thread: a PR
# body or commit messages. These links are private and stay out of the repo,
# also when tooling appends one on its own.
# Usage: <text> | scripts/check-session-links.sh
set -euo pipefail
# Any claude.ai/code/... or claude.ai/project/... path; the bare product link
# claude.ai/code (the attribution footer of comments) passes.
if found=$(grep -niE 'claude\.ai/(code|project)/|^[[:space:]>]*Claude-Session:' -); then
  echo "Claude session or thread links, remove them:" >&2
  echo "$found" >&2
  exit 1
fi
