#!/usr/bin/env bash
# Tests scripts/ci-select.sh: a few fixed cases, then every input the repo
# itself declares: each crate, each path dependency, each file a crate embeds
# from outside its own directory, and each file the Dockerfile copies.
set -euo pipefail
cd "$(dirname "$0")/.."

failures=0
select_for() { printf '%s\n' "$@" | scripts/ci-select.sh | tr '\n' ' '; }
fail() {
  echo "ci-select: $1" >&2
  failures=$((failures + 1))
}
expect() {
  local want=$1
  shift
  local got
  got=$(select_for "$@")
  [[ $got == "$want " ]] || fail "$* -> $got, erwartet $want"
}
# Passes when the selection for one changed path has value among key's words.
# An empty package list with test=true means everything, so it passes too.
expect_selects() {
  local path=$1 key=$2 value=$3 out got
  out=$(printf '%s\n' "$path" | scripts/ci-select.sh)
  got=$(sed -n "s/^$key=//p" <<<"$out")
  [[ " $got " == *" $value "* ]] && return
  [[ $key == packages && -z $got && $out == *test=true* ]] && return
  fail "$path -> $key=$got, erwartet $value"
}
package_of() { sed -n 's/^name *= *"\(.*\)"/\1/p' "$1/Cargo.toml" | head -1; }

# Fixed cases.
all="test=true packages= windows=true image=true"
expect "test=false packages= windows=false image=false" README.md docs/strata-api.md
expect "$all" Cargo.lock
expect "$all" .github/workflows/ci.yml
expect "test=true packages=strata-pc-agent windows=true image=true" crates/pc-agent/src/agent.rs
expect "test=true packages=strata-admin-ui strata-nas-proxy windows=false image=true" crates/admin-ui/src/app.rs
expect "test=false packages= windows=false image=false" deploy/docker-compose.yml
[[ $(scripts/ci-select.sh --all </dev/null | tr '\n' ' ') == "$all " ]] || fail "--all -> nicht alles"

# Every crate selects itself and the image.
for dir in crates/*/; do
  dir=${dir%/}
  package=$(package_of "$dir")
  expect_selects "$dir/src/lib.rs" packages "$package"
  expect_selects "$dir/src/lib.rs" image true
  # Every crate that depends on it by path is selected too.
  for dependent in crates/*/; do
    dependent=${dependent%/}
    if grep -qE "path *= *\"\.\./${dir#crates/}\"" "$dependent/Cargo.toml"; then
      expect_selects "$dir/src/lib.rs" packages "$(package_of "$dependent")"
    fi
  done
done

# Every file a crate embeds from outside its directory selects that crate.
while IFS=: read -r source embedded; do
  crate=$(cut -d/ -f1-2 <<<"$source")
  target=$(realpath -m --relative-to=. "$(dirname "$source")/$embedded")
  [[ $target == "$crate"/* ]] && continue
  expect_selects "$target" packages "$(package_of "$crate")"
done < <(grep -roE --include='*.rs' 'include_(str|bytes)!\(\s*"[^"]+"' crates |
  sed -E 's/:include_(str|bytes)!\(\s*"/:/; s/"$//')

# Every local file the Dockerfile copies selects the image.
while read -r -a words; do
  sources=()
  for word in "${words[@]:1}"; do
    [[ $word == --* || $word == *://* ]] && continue 2
    sources+=("$word")
  done
  unset 'sources[-1]'
  for src in "${sources[@]}"; do
    [[ -d $src ]] && src=$src/x
    expect_selects "$src" image true
  done
done < <(grep -E '^(COPY|ADD) ' Dockerfile)

if ((failures)); then
  echo "ci-select: $failures Fehler; die Zuordnung steht in scripts/ci-select.sh" >&2
  exit 1
fi
echo "ci-select: ok"
