#!/usr/bin/env bash
# Decides which images a push to main publishes: each image whose files changed
# since the commit its :latest was built from (the revision label the publish
# jobs set). A cancelled or failed publish is thereby caught up by the next
# push. Without a label, or with a commit outside HEAD's history, the image
# publishes.
#
# Usage: scripts/ci-publish.sh
# Prints publish= and publish-bench= for $GITHUB_OUTPUT. Needs the history
# down to the published commits and a docker login to ghcr.io.
set -euo pipefail
cd "$(dirname "$0")/.."

# The revision label of an image's :latest, empty if there is none. A
# single-platform image or an index (buildx adds an attestation to it).
published() {
  docker buildx imagetools inspect "ghcr.io/faunkr/$1:latest" --format '{{json .}}' |
    jq -r '.image | if has("config") then . else (.["linux/amd64"] // first(.[])) end
      | .config.Labels["org.opencontainers.image.revision"] // empty'
}

select_since() {
  local image=$1 key=$2 revision
  if ! revision=$(published "$image"); then
    echo "$image: could not read :latest, publishing" >&2
    echo "$key=true"
    return
  fi
  if [[ -z $revision ]] || ! git merge-base --is-ancestor "$revision" HEAD 2>/dev/null; then
    echo "$image: no published commit in HEAD's history (${revision:-no label}), publishing" >&2
    echo "$key=true"
    return
  fi
  echo "$image: :latest is $revision" >&2
  # Renames count with their old path too.
  git diff --no-renames --name-only "$revision" HEAD | scripts/ci-select.sh | sed -n "/^$key=/p"
}

select_since strata-home-proxy publish
select_since strata-code-bench publish-bench
