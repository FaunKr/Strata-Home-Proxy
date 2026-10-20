#!/usr/bin/env bash
# The checks of the CI job `test`. CI runs this script, so local and CI agree.
#
# Usage: scripts/check.sh [package...]
# Without arguments it checks the whole workspace. With package names it checks
# only those; on pull requests CI passes the crates the PR affects. A folder
# name under crates/ works too: pc-protocol stands for strata-pc-protocol.
set -euo pipefail
cd "$(dirname "$0")/.."

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
      echo "Unbekanntes Paket: $package" >&2
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

# A full disk fails the build late and cryptically ("linking with cc failed").
# Locally stop early instead; CI runners start empty, so there it only warns.
free_gb=$(($(df -Pk . | awk 'NR == 2 {print $4}') / 1024 / 1024))
if ((free_gb < 10)); then
  target=$(du -sh target 2>/dev/null | cut -f1 || true)
  message="Kaum Platz frei: ${free_gb} GB, target/ ist ${target:-0} groß; rm -rf target/debug"
  if [[ -n ${CI:-} ]]; then
    echo "WARNUNG: $message" >&2
  else
    echo "$message" >&2
    exit 2
  fi
fi

cargo fmt --check
# Strata starts and stops only in strata_process.rs (#184): the pause after a
# stop, gaming mode and running jobs are checked there.
if grep -rnE '\.(start|stop)_strata\(' crates/pc-agent/src --exclude=strata_process.rs --exclude=platform.rs; then
  echo "Strata nur über agent.strata() starten und beenden (crates/pc-agent/src/strata_process.rs)" >&2
  exit 1
fi
# The Node scripts of the browser check (eslint.config.mjs). Cloud sessions
# have ESLint under /opt/node-tools; elsewhere: npm install -g eslint@10.1.0 (the version CI installs).
eslint=$(command -v eslint || echo /opt/node-tools/node_modules/.bin/eslint)
if [[ ! -x $eslint ]]; then
  echo "ESLint fehlt: npm install -g eslint@10.1.0" >&2
  exit 2
fi
"$eslint" scripts
# Which jobs CI runs for a pull request, checked against the workspace.
scripts/ci-select.test.sh
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
    echo "WARNUNG: Windows-Prüfung des PC-Dienstes übersprungen (rustup target add x86_64-pc-windows-gnu)" >&2
  fi
fi
# install.ps1 parses, as the workflow windows.yml checks it; only where
# PowerShell is installed. The NAS proxy serves the script, the PC service runs it.
if (selected strata-pc-agent || selected strata-nas-proxy) && command -v pwsh >/dev/null; then
  # shellcheck disable=SC2016 # PowerShell variables, for pwsh to expand.
  pwsh -NoProfile -Command '$e = $null; [System.Management.Automation.Language.Parser]::ParseFile("deploy/windows/install.ps1", [ref]$null, [ref]$e) | Out-Null; if ($e) { $e; exit 1 }'
fi
# Die Versionsprüfung des Docker-Baus, an einer Beispieldatei: ein voller
# Commit-Hash passt zur gekürzten ID in der .exe, ein anderer nicht.
sample="$(mktemp)"
trap 'rm -f "$sample"' EXIT
# shellcheck disable=SC2016 # the backticks are part of the sample.
printf 'MZ strata-pc-agent-version:called `unwrap()`\nstrata-pc-agent-version:0.1.0+5e8b9f1\n' > "$sample"
scripts/check-exe-version.sh "$sample" 5e8b9f13827f6f68bb56ea7e3e91c7654e0023b4
if scripts/check-exe-version.sh "$sample" aaaaaaa3827f6f68bb56ea7e3e91c7654e0023b4 2>/dev/null; then
  echo "check-exe-version.sh nimmt eine falsche Build-ID an" >&2
  exit 1
fi
# Das Update-Skript für TrueNAS, gegen nachgebildete docker, midclt und curl.
scripts/test-truenas-auto-update.sh
# Der Runner des Skills local-agent, gegen einen nachgebildeten Proxy und claude.
# Und die Skripte des Repos selbst.
if command -v shellcheck >/dev/null; then
  shellcheck tools/local-agent/local-agent tools/local-agent/install.sh tools/local-agent/test.sh tools/local-agent/testdata/fake-claude \
    scripts/*.sh .claude/hooks/*.sh
else
  echo "WARNUNG: shellcheck fehlt, tools/local-agent und scripts ungeprüft" >&2
fi
# Die Workflows: jeder kaputte Push daran kostet einen vollen CI-Lauf.
if command -v actionlint >/dev/null; then
  actionlint
else
  echo "WARNUNG: actionlint fehlt, .github/workflows ungeprüft (scripts/install-actionlint.sh)" >&2
fi
tools/local-agent/test.sh
# Der Runner des Code-Benchmarks, gegen ein nachgebildetes opencode.
python3 tools/code-bench/test_runner.py
# Erst bauen, dann mit Zeitlimit testen: ein hängender Test bricht ab, statt
# den Lauf endlos zu blockieren. Welcher hängt, steht in der Ausgabe
# ("has been running for over 60 seconds").
cargo test "${package_args[@]}" --no-run
timeout 10m cargo test "${package_args[@]}"
