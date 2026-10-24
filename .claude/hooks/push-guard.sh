#!/bin/bash
# PreToolUse hook for Bash: a `git push` goes through only when a full
# scripts/check.sh run passed on the tree of HEAD. check.sh leaves that mark
# in target/check-ok/<tree>; a merge or any later edit needs a new run.
set -euo pipefail

command -v jq >/dev/null || exit 0
input=$(cat)
cmd=$(jq -r '.tool_input.command // empty' <<<"$input")
# Only where a command starts, so text such as a commit message passes.
# `git -C <dir> push` too.
[[ $cmd =~ (^|[\;\&\|])[[:space:]]*git[[:space:]]+(-C[[:space:]]+([^[:space:]\;\&\|]+)[[:space:]]+)?push([[:space:]]|$) ]] || exit 0
git_dir_arg=${BASH_REMATCH[3]}
# Deleting a remote branch pushes no code.
[[ $cmd == *--delete* ]] && exit 0

# Where the push runs: a worktree (a local-agent run's, say) pushes its own
# HEAD, which check.sh marked in the main checkout's target/. That is the
# shell's folder, then a leading `cd <dir> &&`, then `git -C <dir>`.
cwd=$(jq -r '.cwd // empty' <<<"$input")
cd "${cwd:-$CLAUDE_PROJECT_DIR}"
if [[ $cmd =~ ^[[:space:]]*cd[[:space:]]+([^[:space:]\;\&\|]+)[[:space:]]*\&\& ]]; then
  cd "${BASH_REMATCH[1]//[\"\']/}"
fi
if [[ -n $git_dir_arg ]]; then
  cd "${git_dir_arg//[\"\']/}"
fi
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
target_dir=${CARGO_TARGET_DIR:-${common_dir%/.git}/target}
tree=$(git rev-parse 'HEAD^{tree}')
if [[ ! -e $target_dir/check-ok/$tree ]]; then
  echo "Push blocked: no full scripts/check.sh run passed on this commit's tree. Run scripts/check.sh without crate names, commit nothing new after it, then push." >&2
  exit 2
fi
