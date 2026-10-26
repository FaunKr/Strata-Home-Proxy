#!/usr/bin/env bash
# The checks of the CI job `test`. CI runs this script, so local and CI agree.
#
# Usage: scripts/check.sh [package... | --no-cargo]
# Without arguments it checks the whole workspace. With package names it checks
# only those; on pull requests CI passes the crates the PR affects. A folder
# name under crates/ works too: pc-protocol stands for strata-pc-protocol.
# --no-cargo builds, lints and tests no crate: CI passes it when a pull request
# only changes what the scripts' own tests cover (scripts/ci-select.sh).
set -euo pipefail
cd "$(dirname "$0")/.."

# The last line always says how the run ended, so a log read with `tail` or
# a grep cannot mistake a failed run for a passed one.
sample=""
finish() {
  local status=$?
  rm -f "$sample"
  if ((status == 0)); then
    echo "check.sh: PASSED"
  else
    echo "check.sh: FAILED (exit $status)"
  fi
}
trap finish EXIT

cargo=true
if [[ ${1:-} == --no-cargo ]]; then
  if (($# > 1)); then
    echo "--no-cargo takes no package names" >&2
    exit 2
  fi
  cargo=false
  packages=" "
  package_args=()
elif (($#)); then
  names=()
  for package in "$@"; do
    if [[ -f crates/$package/Cargo.toml ]]; then
      name=$(sed -n 's/^name = "\(.*\)"$/\1/p' "crates/$package/Cargo.toml" | head -n 1)
      package=${name:-$package}
    fi
    names+=("$package")
  done
  set -- "${names[@]}"
  for package in "$@"; do
    # Workspace crates have a path ID; a dependency's name would match too.
    if [[ $(cargo pkgid "$package" 2>/dev/null) != path+file://"$PWD"/crates/* ]]; then
      echo "Unknown package: $package" >&2
      exit 2
    fi
  done
  packages=" $* "
  package_args=()
  for package in "$@"; do package_args+=(-p "$package"); done
else
  packages=all
  package_args=(--workspace)
fi
selected() { [[ $packages == all || $packages == *" $1 "* ]]; }

# A git worktree builds into the main checkout's target/: dependencies are
# built once, not 5 GB again per worktree. Cargo's lock lets parallel runs
# take turns.
git_dir=$(git rev-parse --path-format=absolute --git-dir 2>/dev/null || true)
common_dir=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)
if [[ -z ${CARGO_TARGET_DIR:-} && $git_dir != "$common_dir" ]]; then
  export CARGO_TARGET_DIR=${common_dir%/.git}/target
fi
target_dir=${CARGO_TARGET_DIR:-target}

# The tree this run checks, untracked files included, read through an index of
# its own. A full run that passes leaves a mark for it; the push guard
# (.claude/hooks/push-guard.sh) lets through only commits with that tree.
tree_index=$(mktemp)
cp "$(git rev-parse --git-path index)" "$tree_index"
tree=$(GIT_INDEX_FILE=$tree_index git add -A && GIT_INDEX_FILE=$tree_index git write-tree)
rm -f "$tree_index"

# shellcheck source=scripts/free-space.sh
source "$(dirname "$0")/free-space.sh"

# Every tool a full run needs, all named at once before anything runs: on a
# machine set up for the first time (a Mac for local-agent, say) a run that
# stops at each missing tool in turn costs one run per tool.
missing=()
# The Node scripts of the browser check (eslint.config.mjs). Cloud sessions
# have ESLint under /opt/node-tools; elsewhere: npm install -g eslint@10.1.0 (the version CI installs).
eslint=$(command -v eslint || echo /opt/node-tools/node_modules/.bin/eslint)
[[ -x $eslint ]] || missing+=("ESLint: npm install -g eslint@10.1.0")
# The update script for TrueNAS locks with flock.
command -v flock >/dev/null || missing+=("flock: brew install flock (on Linux it is part of util-linux)")
# The code benchmark's runner removes folders through shutil.rmtree(dir_fd=…).
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' 2>/dev/null ||
  missing+=("Python 3.11 or newer as python3 (now: $(python3 --version 2>&1)): brew install python (on Linux your distribution's python3.11 or newer)")
if [[ $cargo == true ]]; then
  # The time limit of the test run.
  # shellcheck source=scripts/timeout.sh
  source "$(dirname "$0")/timeout.sh"
  [[ -n $timeout ]] || missing+=("$timeout_hint")
  # The test runner, at least the version .config/nextest.toml pins (CI installs it).
  nextest_version=$(scripts/nextest-version.sh)
  installed=$(cargo nextest --version 2>/dev/null | sed -n '1s/^cargo-nextest \([0-9.]*\).*/\1/p' || true)
  if [[ -z $installed || $(printf '%s\n' "$nextest_version" "$installed" | sort -V | head -n1) != "$nextest_version" ]]; then
    missing+=("cargo-nextest ${installed:-missing}, needs $nextest_version: scripts/install-nextest.sh (or cargo install cargo-nextest@$nextest_version --locked)")
  fi
  # The browser half of the web UI. Homebrew's Rust has no wasm32 standard
  # library; rustup's has, once its bin folder comes first in PATH.
  if selected strata-admin-ui && [[ ! -d $(rustc --print sysroot)/lib/rustlib/wasm32-unknown-unknown ]]; then
    missing+=("the wasm32 target for $(command -v rustc): rustup target add wasm32-unknown-unknown; a Rust without rustup (Homebrew's) has none, so put ~/.cargo/bin before it in PATH")
  fi
fi
# The linters shellcheck and actionlint, and pwsh with PSScriptAnalyzer, stay warnings below: CI
# installs them, and on a Mac they are not needed to build and test.
if ((${#missing[@]})); then
  printf 'Missing for a full run:\n' >&2
  printf '  %s\n' "${missing[@]}" >&2
  exit 2
fi

if [[ $cargo == true ]]; then
  cargo fmt --check
fi
# Strata starts and stops only in strata_process.rs (#184): the pause after a
# stop, gaming mode and running jobs are checked there.
if grep -rnE '\.(start|stop)_strata\(' crates/pc-agent/src --exclude=strata_process.rs --exclude=platform.rs; then
  echo "Start and stop Strata only through agent.strata() (crates/pc-agent/src/strata_process.rs)" >&2
  exit 1
fi
"$eslint" scripts
# The integration tests are one binary per crate (autotests = false): a file
# in tests/ that tests/main.rs does not list would silently never run.
for main in crates/*/tests/main.rs; do
  for file in "${main%/main.rs}"/*.rs "${main%/main.rs}"/*/mod.rs; do
    [[ -e $file ]] || continue
    name=$(basename "${file%/mod.rs}" .rs)
    if [[ $name != main ]] && ! grep -qE "(^|[] ])mod $name;" "$main"; then
      echo "$file does not run: add \"mod $name;\" to $main" >&2
      exit 1
    fi
  done
done
# Which jobs CI runs for a pull request, checked against the workspace.
scripts/ci-select.test.sh
# Which images a push to main publishes, from the published revision labels.
scripts/ci-publish.test.sh
# The repo is English: no umlauts in tracked files (scripts/language-allowlist).
scripts/check-language.test.sh
scripts/check-language.sh
# No Claude session links in commits or PR bodies; CI checks the PR itself.
scripts/check-session-links.test.sh
# CHANGELOG.md has a section for the product version (the workflow Release).
scripts/changelog-section.test.sh
# Which HEAD the push guard checks, in a worktree too.
scripts/push-guard.test.sh
if [[ $cargo == true ]]; then
  cargo clippy "${package_args[@]}" --all-targets -- -D warnings
fi
# The browser half of the web UI, which the Docker image builds with cargo-leptos.
if selected strata-admin-ui; then
  cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate -- -D warnings
fi
# The PC service's Windows code; CI checks it here instead of on a Windows
# runner (the manual workflow windows.yml runs it on real Windows). Only
# where the target is installed (rustup target add x86_64-pc-windows-gnu).
if selected strata-pc-agent; then
  if rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-gnu; then
    cargo clippy -p strata-pc-agent --all-targets --target x86_64-pc-windows-gnu -- -D warnings
  else
    echo "WARNING: Windows check of the PC service skipped (rustup target add x86_64-pc-windows-gnu)" >&2
  fi
fi
# install.ps1 parses, as the workflow windows.yml checks it; only where
# PowerShell is installed. The proxy serves the script, the PC service runs it.
if (selected strata-pc-agent || selected strata-nas-proxy) && command -v pwsh >/dev/null; then
  # shellcheck disable=SC2016 # PowerShell variables, for pwsh to expand.
  pwsh -NoProfile -Command '$e = $null; [System.Management.Automation.Language.Parser]::ParseFile("deploy/windows/install.ps1", [ref]$null, [ref]$e) | Out-Null; if ($e) { $e; exit 1 }'
fi
# The Docker build's version check, on a sample file: the crate version
# matches, another version or one with a build suffix does not.
sample="$(mktemp)"
# shellcheck disable=SC2016 # the backticks are part of the sample.
printf 'MZ strata-pc-agent-version:called `unwrap()`\nstrata-pc-agent-version:0.2.0\n' > "$sample"
scripts/check-exe-version.sh "$sample" 0.2.0
for wrong in 0.2.1 0.2; do
  if scripts/check-exe-version.sh "$sample" "$wrong" 2>/dev/null; then
    echo "check-exe-version.sh accepts the wrong version $wrong" >&2
    exit 1
  fi
done
printf 'strata-pc-agent-version:0.2.0+5e8b9f1\n' > "$sample"
if scripts/check-exe-version.sh "$sample" 0.2.0 2>/dev/null; then
  echo "check-exe-version.sh accepts a build suffix" >&2
  exit 1
fi
# The update script for TrueNAS, against faked docker, midclt and curl.
scripts/test-truenas-auto-update.sh
# The runner of the local-agent skill, against a faked proxy and claude.
# And the repo's own scripts.
if command -v shellcheck >/dev/null; then
  shellcheck tools/local-agent/local-agent tools/local-agent/install.sh tools/local-agent/test.sh tools/local-agent/testdata/fake-claude \
    tools/local-agent/shims/local-agent \
    scripts/*.sh .claude/hooks/*.sh
else
  echo "WARNING: shellcheck missing, tools/local-agent and scripts not checked" >&2
fi
# The workflows: every broken push to them costs a full CI run.
if command -v actionlint >/dev/null; then
  actionlint
else
  echo "WARNING: actionlint missing, .github/workflows not checked (scripts/install-actionlint.sh)" >&2
fi
# In an expression, `cond && x || y` gives y whenever x is falsy (0, false,
# ''), whatever cond is; actionlint does not catch it (#344's fetch-depth).
if grep -nE "\\$\\{\\{[^}]*&& *(0|false|''|\"\") *\\)? *\\|\\|" .github/workflows/*.yml; then
  echo "workflows: a falsy value after && always falls through to ||; quote it ('0') or flip the condition" >&2
  exit 1
fi
tools/local-agent/test.sh
# Its PowerShell port for Windows: the same tests under pwsh, and
# PSScriptAnalyzer, which also checks its syntax and commands against Windows
# PowerShell 5.1.
if command -v pwsh >/dev/null; then
  pwsh -NoProfile -File tools/local-agent/test.ps1
  lint=0
  # shellcheck disable=SC2016 # PowerShell variables, for pwsh to expand.
  pwsh -NoProfile -Command '
    if (-not (Get-Module -ListAvailable PSScriptAnalyzer)) { exit 3 }
    $found = foreach ($file in Get-ChildItem tools/local-agent/*.ps1) {
      Invoke-ScriptAnalyzer -Path $file.FullName -Settings tools/local-agent/PSScriptAnalyzerSettings.psd1
    }
    $found | ForEach-Object { "{0}:{1}: {2}: {3}" -f $_.ScriptPath, $_.Line, $_.RuleName, $_.Message }
    if ($found) { exit 1 }' || lint=$?
  if ((lint == 3)); then
    echo "WARNING: PSScriptAnalyzer missing, tools/local-agent/*.ps1 not linted (scripts/install-psscriptanalyzer.sh)" >&2
  elif ((lint != 0)); then
    exit "$lint"
  fi
else
  echo "WARNING: pwsh missing, the PowerShell port of local-agent not tested" >&2
fi
# The code benchmark's runner, against a faked opencode.
python3 tools/code-bench/test_runner.py
# Build first, then test. nextest kills a test after 60 s and names it
# (.config/nextest.toml); a package without tests passes, as with cargo test.
# nextest runs no doctests; there are none (cargo test --doc: 0 everywhere).
if [[ $cargo == true ]]; then
  cargo nextest run "${package_args[@]}" --no-run
  # The time limit catches a run that hangs outside a single test.
  "$timeout" 15m cargo nextest run "${package_args[@]}" --no-tests=pass
fi

if [[ $packages == all ]]; then
  mkdir -p "$target_dir/check-ok"
  touch "$target_dir/check-ok/$tree"
fi
