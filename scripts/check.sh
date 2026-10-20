#!/usr/bin/env bash
# The checks of the CI job `test`. CI runs this script, so local and CI agree.
#
# Usage: scripts/check.sh [package...]
# Without arguments it checks the whole workspace. With package names it checks
# only those; on pull requests CI passes the crates the PR affects. A folder
# name under crates/ works too: pc-protocol stands for strata-pc-protocol.
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

if (($#)); then
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

# A full disk fails the build late and cryptically ("linking with cc failed").
# Locally stop early instead; CI runners start empty, so there it only warns.
free_gb=$(($(df -Pk . | awk 'NR == 2 {print $4}') / 1024 / 1024))
if ((free_gb < 10)); then
  target=$(du -sh "$target_dir" 2>/dev/null | cut -f1 || true)
  message="Hardly any space left: ${free_gb} GB, $target_dir is ${target:-0}; rm -rf $target_dir/debug, and remove old worktrees (git worktree list)"
  if [[ -n ${CI:-} ]]; then
    echo "WARNING: $message" >&2
  else
    echo "$message" >&2
    exit 2
  fi
fi

cargo fmt --check
# Strata starts and stops only in strata_process.rs (#184): the pause after a
# stop, gaming mode and running jobs are checked there.
if grep -rnE '\.(start|stop)_strata\(' crates/pc-agent/src --exclude=strata_process.rs --exclude=platform.rs; then
  echo "Start and stop Strata only through agent.strata() (crates/pc-agent/src/strata_process.rs)" >&2
  exit 1
fi
# The Node scripts of the browser check (eslint.config.mjs). Cloud sessions
# have ESLint under /opt/node-tools; elsewhere: npm install -g eslint@10.1.0 (the version CI installs).
eslint=$(command -v eslint || echo /opt/node-tools/node_modules/.bin/eslint)
if [[ ! -x $eslint ]]; then
  echo "ESLint missing: npm install -g eslint@10.1.0" >&2
  exit 2
fi
"$eslint" scripts
# Which jobs CI runs for a pull request, checked against the workspace.
scripts/ci-select.test.sh
# The repo is English: no umlauts in tracked files (scripts/language-allowlist).
scripts/check-language.test.sh
scripts/check-language.sh
cargo clippy "${package_args[@]}" --all-targets -- -D warnings
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
# The Docker build's version check, on a sample file: a full commit hash
# matches the shortened ID in the .exe, a different one does not.
sample="$(mktemp)"
# shellcheck disable=SC2016 # the backticks are part of the sample.
printf 'MZ strata-pc-agent-version:called `unwrap()`\nstrata-pc-agent-version:0.1.0+5e8b9f1\n' > "$sample"
scripts/check-exe-version.sh "$sample" 5e8b9f13827f6f68bb56ea7e3e91c7654e0023b4
if scripts/check-exe-version.sh "$sample" aaaaaaa3827f6f68bb56ea7e3e91c7654e0023b4 2>/dev/null; then
  echo "check-exe-version.sh accepts a wrong build ID" >&2
  exit 1
fi
# The update script for TrueNAS, against faked docker, midclt and curl.
scripts/test-truenas-auto-update.sh
# The runner of the local-agent skill, against a faked proxy and claude.
# And the repo's own scripts.
if command -v shellcheck >/dev/null; then
  shellcheck tools/local-agent/local-agent tools/local-agent/install.sh tools/local-agent/test.sh tools/local-agent/testdata/fake-claude \
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
tools/local-agent/test.sh
# The code benchmark's runner, against a faked opencode.
python3 tools/code-bench/test_runner.py
# Build first, then test with a time limit: a hanging test aborts instead of
# blocking the run forever. Which one hangs is in the output
# ("has been running for over 60 seconds").
cargo test "${package_args[@]}" --no-run
timeout 10m cargo test "${package_args[@]}"
