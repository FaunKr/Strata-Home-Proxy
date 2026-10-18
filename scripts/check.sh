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
# Which jobs CI runs for a pull request, checked against the workspace.
scripts/ci-select.test.sh
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
# Die Versionsprüfung des Docker-Baus, an einer Beispieldatei: ein voller
# Commit-Hash passt zur gekürzten ID in der .exe, ein anderer nicht.
sample="$(mktemp)"
trap 'rm -f "$sample"' EXIT
printf 'MZ strata-pc-agent-version:called `unwrap()`\nstrata-pc-agent-version:0.1.0+5e8b9f1\n' > "$sample"
scripts/check-exe-version.sh "$sample" 5e8b9f13827f6f68bb56ea7e3e91c7654e0023b4
if scripts/check-exe-version.sh "$sample" aaaaaaa3827f6f68bb56ea7e3e91c7654e0023b4 2>/dev/null; then
  echo "check-exe-version.sh nimmt eine falsche Build-ID an" >&2
  exit 1
fi
# Das Update-Skript für TrueNAS, gegen nachgebildete docker, midclt und curl.
scripts/test-truenas-auto-update.sh
# Erst bauen, dann mit Zeitlimit testen: ein hängender Test bricht ab, statt
# den Lauf endlos zu blockieren. Welcher hängt, steht in der Ausgabe
# ("has been running for over 60 seconds").
cargo test "${package_args[@]}" --no-run
timeout 10m cargo test "${package_args[@]}"
