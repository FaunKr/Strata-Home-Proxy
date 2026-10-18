#!/usr/bin/env bash
# The checks of the CI job `test`. CI runs this script, so local and CI agree.
#
# Usage: scripts/check.sh [package...]
# Without arguments it checks the whole workspace. With package names it checks
# only those; on pull requests CI passes the crates the PR affects.
set -euo pipefail
cd "$(dirname "$0")/.."

if (($#)); then
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

cargo fmt --check
cargo clippy "${package_args[@]}" --all-targets -- -D warnings
# The browser half of the web UI, which the Docker image builds with cargo-leptos.
if selected strata-admin-ui; then
  cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate -- -D warnings
fi
# The PC service's Windows code, as the CI job `windows` checks it; only
# where the target is installed (rustup target add x86_64-pc-windows-gnu).
if selected strata-pc-agent; then
  if rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-gnu; then
    cargo clippy -p strata-pc-agent --all-targets --target x86_64-pc-windows-gnu -- -D warnings
  else
    echo "WARNUNG: Windows-Prüfung des PC-Dienstes übersprungen (rustup target add x86_64-pc-windows-gnu)" >&2
  fi
fi
# install.ps1 parses, as the CI job `windows` checks it; only where PowerShell
# is installed. The NAS proxy serves the script, the PC service runs it.
if (selected strata-pc-agent || selected strata-nas-proxy) && command -v pwsh >/dev/null; then
  pwsh -NoProfile -Command '$e = $null; [System.Management.Automation.Language.Parser]::ParseFile("deploy/windows/install.ps1", [ref]$null, [ref]$e) | Out-Null; if ($e) { $e; exit 1 }'
fi
# Erst bauen, dann mit Zeitlimit testen: ein hängender Test bricht ab, statt
# den Lauf endlos zu blockieren. Welcher hängt, steht in der Ausgabe
# ("has been running for over 60 seconds").
cargo test "${package_args[@]}" --no-run
timeout 10m cargo test "${package_args[@]}"
