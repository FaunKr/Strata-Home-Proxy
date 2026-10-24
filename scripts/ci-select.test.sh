#!/usr/bin/env bash
# Tests scripts/ci-select.sh: a few fixed cases, then every input the repo
# itself declares: each crate, each path dependency, each file a crate embeds
# from outside its own directory, each file the Dockerfile copies, and each
# file check.sh (or a script it runs) uses.
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
  [[ $got == "$want " ]] || fail "$* -> $got, expected $want"
}
# Passes when the selection for one changed path has value among key's words.
# An empty package list with cargo=true means everything, so it passes too.
expect_selects() {
  local path=$1 key=$2 value=$3 out got
  out=$(printf '%s\n' "$path" | scripts/ci-select.sh)
  got=$(sed -n "s/^$key=//p" <<<"$out")
  [[ " $got " == *" $value "* ]] && return
  [[ $key == packages && -z $got && $out == *cargo=true* ]] && return
  fail "$path -> $key=$got, expected $value"
}
package_of() { sed -n 's/^name *= *"\(.*\)"/\1/p' "$1/Cargo.toml" | head -1; }

# Fixed cases.
shared="test=true packages= cargo=true image=true bench=true ui=true"
expect "test=false packages= cargo=false image=false bench=false ui=false publish=false publish-bench=false" README.md docs/strata-api.md
expect "$shared publish=true publish-bench=false" Cargo.lock
expect "$shared publish=true publish-bench=true" .github/workflows/ci.yml
expect "$shared publish=false publish-bench=false" .config/nextest.toml
expect "$shared publish=true publish-bench=false" scripts/check-exe-version.sh
# Code alone runs the tests of its crates; the images build only when their
# build definition changes.
expect "test=true packages=strata-pc-agent cargo=true image=false bench=false ui=false publish=true publish-bench=false" crates/pc-agent/src/agent.rs
expect "test=true packages=strata-admin-ui strata-nas-proxy cargo=true image=false bench=false ui=true publish=true publish-bench=false" crates/admin-ui/src/app.rs
expect "test=true packages=strata-nas-proxy cargo=true image=false bench=false ui=false publish=true publish-bench=false" deploy/windows/install.ps1
expect "test=true packages=strata-nas-proxy cargo=true image=false bench=false ui=true publish=true publish-bench=false" crates/nas-proxy/src/admin.rs
expect "test=false packages= cargo=false image=true bench=false ui=true publish=true publish-bench=false" Dockerfile
expect "test=false packages= cargo=false image=true bench=false ui=false publish=true publish-bench=false" .dockerignore
expect "test=false packages= cargo=false image=false bench=false ui=false publish=false publish-bench=false" deploy/docker-compose.yml
expect "test=true packages= cargo=false image=false bench=false ui=false publish=false publish-bench=false" tools/local-agent/local-agent
expect "test=true packages= cargo=false image=false bench=true ui=false publish=false publish-bench=true" tools/code-bench/runner.py
expect "test=true packages= cargo=false image=false bench=true ui=false publish=false publish-bench=true" tools/code-bench/Dockerfile
expect "test=true packages= cargo=false image=false bench=false ui=false publish=false publish-bench=false" tools/code-bench/test_runner.py
expect "test=true packages= cargo=false image=false bench=true ui=false publish=false publish-bench=true" tools/code-bench/prepare_repo_task.py
expect "test=false packages= cargo=false image=false bench=true ui=false publish=true publish-bench=true" tools/code-bench/tasks/pc-force-endpoints/repo.json
expect "test=true packages=strata-nas-proxy cargo=true image=false bench=false ui=false publish=true publish-bench=false" tools/code-bench/tasks/roman/TASK.md
# A runner change next to a crate change: check.sh gets the crate and runs the
# runners' tests anyway.
expect "test=true packages=strata-pc-agent cargo=true image=false bench=true ui=false publish=true publish-bench=true" tools/code-bench/runner.py crates/pc-agent/src/agent.rs
[[ $(scripts/ci-select.sh --all </dev/null | tr '\n' ' ') == "$shared publish=true publish-bench=true " ]] || fail "--all -> not everything"
# On main: test-only files publish nothing, other workflows and scripts neither.
expect "test=true packages=strata-nas-proxy cargo=true image=false bench=false ui=true publish=false publish-bench=false" crates/nas-proxy/tests/proxy.rs
expect "test=true packages=strata-pc-agent cargo=true image=false bench=false ui=false publish=false publish-bench=false" crates/pc-agent/fixtures/strata-tune.json
expect "test=true packages=strata-pc-agent cargo=true image=false bench=false ui=false publish=false publish-bench=false" crates/pc-agent/strata-pc-agent.example.toml
expect "test=true packages=strata-nas-proxy cargo=true image=false bench=false ui=true publish=true publish-bench=false" crates/nas-proxy/src/tests/x.rs
expect "$shared publish=false publish-bench=false" .github/workflows/windows.yml
expect "$shared publish=false publish-bench=false" scripts/check.sh

# Every crate selects itself.
for dir in crates/*/; do
  dir=${dir%/}
  package=$(package_of "$dir")
  expect_selects "$dir/src/lib.rs" packages "$package"
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
  target=$(cd "$(dirname "$source")/$(dirname "$embedded")" && pwd -P)/$(basename "$embedded")
  target=${target#"$PWD"/}
  [[ $target == "$crate"/* ]] && continue
  expect_selects "$target" packages "$(package_of "$crate")"
done < <(grep -roE --include='*.rs' 'include_(str|bytes)!\(\s*"[^"]+"' crates |
  sed -E 's/:include_(str|bytes)!\(\s*"/:/; s/"$//')

# Every local file an image's Dockerfile copies is checked by a pull request
# that changes it: it builds that image or runs the tests that cover the file.
expect_checked() {
  local path=$1 key=$2 out
  out=$(printf '%s\n' "$path" | scripts/ci-select.sh)
  [[ $out == *"$key=true"* || $out == *test=true* ]] || fail "$path -> neither $key nor test"
}
dockerfile_sources() {
  local words word sources src
  while read -r -a words; do
    sources=()
    for word in "${words[@]:1}"; do
      [[ $word == --from=* || $word == *://* ]] && continue 2
      [[ $word == --* ]] || sources+=("$word")
    done
    unset "sources[$((${#sources[@]} - 1))]"
    for src in "${sources[@]}"; do
      [[ -d $1/$src ]] && src=$src/x
      echo "${1#./}/$src" | sed 's|^\./||; s|^/||'
    done
  done < <(sed -e ':a' -e '/\\$/N' -e 's/\\\n//' -e 'ta' "$1/Dockerfile" | grep -E '^(COPY|ADD) ')
}
while read -r src; do expect_checked "$src" image; done < <(dockerfile_sources .)
# The runner's image holds nothing a test job builds, so only bench counts.
while read -r src; do expect_selects "$src" bench true; done < <(dockerfile_sources tools/code-bench)
# Every file that goes into an image publishes it, unless .dockerignore keeps
# it out as test-only; each test-only entry publishes nothing.
dockerignored() {
  local pattern
  while read -r pattern; do
    [[ -z $pattern || $pattern == "#"* ]] && continue
    # As in Docker, * does not match across a /.
    [[ $1 =~ ^${pattern//\*/[^/]*}(/|$) ]] && return 0
  done <.dockerignore
  return 1
}
while read -r src; do
  for file in $(git ls-files "${src%/x}"); do
    dockerignored "$file" && continue
    expect_selects "$file" publish true
  done
done < <(dockerfile_sources .)
while read -r pattern; do
  [[ $pattern == crates/* ]] || continue
  for file in $(git ls-files "$pattern"); do expect_selects "$file" publish false; done
done <.dockerignore
while read -r src; do expect_selects "$src" publish-bench true; done < <(dockerfile_sources tools/code-bench)
# The image definitions themselves select their image.
expect_selects Dockerfile image true
expect_selects tools/code-bench/Dockerfile bench true

# Every file check.sh uses, and every file the scripts it runs use, runs the
# test job.
used_by() { grep -oE '(scripts|deploy|tools)/[A-Za-z0-9._/-]+' "$1" | sort -u; }
for script in scripts/check.sh $(used_by scripts/check.sh | grep '^scripts/.*\.sh$' | grep -v ci-select); do
  for path in $(used_by "$script"); do
    [[ -e $path ]] && expect_selects "$path" test true
  done
done

# Every doc, comment or script that names a CI job or workflow names one that
# exists, so removing a job can't leave stale mentions behind.
jobs=" $(awk '/^jobs:/ { in_jobs = 1; next } /^[^ #]/ { in_jobs = 0 } in_jobs && /^  [A-Za-z0-9_-]+:/ { sub(/:.*/, ""); printf "%s ", $1 }' .github/workflows/*.yml)"
# The backticks are literal (Markdown code spans), not substitutions.
# shellcheck disable=SC2016
while IFS= read -r mention; do
  file=${mention%%:*}
  name=$(sed -E 's/.*`([^`]+)`.*/\1/' <<<"$mention")
  case $mention in
    *orkflow*) [[ -e .github/workflows/$name ]] || fail "$file names the workflow $name, which does not exist" ;;
    *) [[ $jobs == *" $name "* ]] || fail "$file names the CI job $name, which does not exist" ;;
  esac
done < <(git ls-files -z | xargs -0 grep -oIE '(CI[- ][Jj]ob|[Jj]ob|[Ww]orkflow) `[A-Za-z0-9_.-]+`' 2>/dev/null || true)

if ((failures)); then
  echo "ci-select: $failures failures; the mapping is in scripts/ci-select.sh" >&2
  exit 1
fi
echo "ci-select: ok"
