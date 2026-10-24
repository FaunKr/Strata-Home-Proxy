#!/usr/bin/env bash
# Tests .claude/hooks/push-guard.sh: which folder's HEAD a push is checked
# against (the shell's folder, a leading `cd <dir> &&`, `git -C <dir>`).
set -euo pipefail
cd "$(dirname "$0")/.."
guard=$PWD/.claude/hooks/push-guard.sh

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# A repo with two commits: the first passed check.sh, the second did not.
main=$tmp/main checked=$tmp/checked
git init -q "$main"
git -C "$main" -c user.name=t -c user.email=t@t commit -q --allow-empty -m checked
git -C "$main" worktree add -q --detach "$checked" HEAD
mkdir -p "$main/target/check-ok"
touch "$main/target/check-ok/$(git -C "$main" rev-parse 'HEAD^{tree}')"
echo change >"$main/file" && git -C "$main" add file
git -C "$main" -c user.name=t -c user.email=t@t commit -q -m unchecked

failures=0
# $1: the command, $2: the shell's folder, $3: the expected exit code.
expect() {
  local code=0
  jq -n --arg cmd "$1" --arg cwd "$2" '{tool_input: {command: $cmd}, cwd: $cwd}' \
    | env -u CARGO_TARGET_DIR CLAUDE_PROJECT_DIR="$main" "$guard" 2>/dev/null || code=$?
  if [[ $code != "$3" ]]; then
    echo "push-guard: \"$1\" in $2 exits $code, not $3" >&2
    failures=$((failures + 1))
  fi
}
expect "git push origin x" "$checked" 0
expect "git push origin x" "$main" 2
expect "cd $main && git push origin x" "$checked" 2
expect "cd $checked && git push origin x" "$main" 0
expect "git -C $main push origin x" "$checked" 2
expect "git -C $checked push origin x" "$main" 0
expect 'git commit -m "git push later"' "$main" 0
expect "git push origin --delete x" "$main" 0
((failures == 0)) || exit 1
echo "push-guard: all cases passed"
