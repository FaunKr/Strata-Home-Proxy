#!/usr/bin/env bash
# Checks a PR for Claude session or thread links (check-session-links.sh):
#   pr-text.sh commits   its commit messages (the changes job, each push)
#   pr-text.sh body      its body (pr-text.yml, on open and each edit)
# Needs GH_TOKEN, PR and GITHUB_REPOSITORY (CI sets them).
set -euo pipefail
cd "$(dirname "$0")/.."
case ${1:-} in
  commits) gh api --paginate "repos/$GITHUB_REPOSITORY/pulls/$PR/commits" --jq '.[].commit.message' ;;
  body) gh api "repos/$GITHUB_REPOSITORY/pulls/$PR" --jq '.body // ""' ;;
  *) echo "usage: pr-text.sh commits|body" >&2; exit 2 ;;
esac | scripts/check-session-links.sh
