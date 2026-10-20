#!/bin/bash
# PreToolUse hook for Bash: a `git push` goes through only when a full
# scripts/check.sh run passed on the tree of HEAD. check.sh leaves that mark
# in target/check-ok/<tree>; a merge or any later edit needs a new run.
set -euo pipefail

command -v jq >/dev/null || exit 0
cmd=$(jq -r '.tool_input.command // empty')
# Only where a command starts, so text such as a commit message passes.
[[ $cmd =~ (^|[\;\&\|])[[:space:]]*git[[:space:]]+push([[:space:]]|$) ]] || exit 0
# Deleting a remote branch pushes no code.
[[ $cmd == *--delete* ]] && exit 0

cd "$CLAUDE_PROJECT_DIR"
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
target_dir=${CARGO_TARGET_DIR:-${common_dir%/.git}/target}
tree=$(git rev-parse 'HEAD^{tree}')
if [[ ! -e $target_dir/check-ok/$tree ]]; then
  echo "Push blocked: no full scripts/check.sh run passed on this commit's tree. Run scripts/check.sh without crate names, commit nothing new after it, then push." >&2
  exit 2
fi
