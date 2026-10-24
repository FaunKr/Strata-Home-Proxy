#!/usr/bin/env bash
# Tests scripts/ci-publish.sh with a stand-in for docker that answers with the
# :latest it is given in $IMAGE_JSON (or fails without one).
set -euo pipefail
cd "$(dirname "$0")/.."

failures=0
bin=$(mktemp -d)
trap 'rm -rf "$bin"' EXIT
cat >"$bin/docker" <<'STUB'
#!/usr/bin/env bash
[[ -n ${IMAGE_JSON:-} ]] || exit 1
echo "$IMAGE_JSON"
STUB
chmod +x "$bin/docker"

expect() {
  local want=$1 json=$2 got
  got=$(IMAGE_JSON=$json PATH="$bin:$PATH" scripts/ci-publish.sh 2>/dev/null | tr '\n' ' ')
  [[ $got == "$want " ]] || {
    echo "ci-publish: $3 -> $got, expected $want" >&2
    failures=$((failures + 1))
  }
}
head=$(git rev-parse HEAD)
labels() { printf '{"Labels":{"org.opencontainers.image.revision":"%s"}}' "$1"; }

expect "publish=false publish-bench=false" "{\"image\":{\"config\":$(labels "$head")}}" "published at HEAD"
expect "publish=false publish-bench=false" "{\"image\":{\"linux/amd64\":{\"config\":$(labels "$head")}}}" "an index published at HEAD"
expect "publish=true publish-bench=true" '{"image":{"config":{"Labels":{}}}}' "no label"
expect "publish=true publish-bench=true" "{\"image\":{\"config\":$(labels 0123456789abcdef0123456789abcdef01234567)}}" "a commit outside the history"
expect "publish=true publish-bench=true" "" "docker fails"

if ((failures)); then
  echo "ci-publish: $failures failures" >&2
  exit 1
fi
echo "ci-publish: ok"
