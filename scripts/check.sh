#!/usr/bin/env bash
# The checks of the CI job `test`. CI runs this script, so local and CI agree.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --all-targets -- -D warnings
# The browser half of the web UI, which the Docker image builds with cargo-leptos.
cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate -- -D warnings
# The PC service's Windows code, as the CI job `windows` checks it; only
# where the target is installed (rustup target add x86_64-pc-windows-gnu).
if rustup target list --installed 2>/dev/null | grep -qx x86_64-pc-windows-gnu; then
  cargo clippy -p strata-pc-agent --all-targets --target x86_64-pc-windows-gnu -- -D warnings
else
  echo "WARNUNG: Windows-Prüfung des PC-Dienstes übersprungen (rustup target add x86_64-pc-windows-gnu)" >&2
fi
# install.ps1 parses, as the CI job `windows` checks it; only where PowerShell
# is installed.
if command -v pwsh >/dev/null; then
  pwsh -NoProfile -Command '$e = $null; [System.Management.Automation.Language.Parser]::ParseFile("deploy/windows/install.ps1", [ref]$null, [ref]$e) | Out-Null; if ($e) { $e; exit 1 }'
fi
# Die Versionsprüfung des Docker-Baus, an einer Beispieldatei: ein voller
# Commit-Hash passt zur gekürzten ID in der .exe, ein anderer nicht.
sample="$(mktemp)"
printf 'MZ strata-pc-agent-version:called `unwrap()`\nstrata-pc-agent-version:0.1.0+5e8b9f1\n' > "$sample"
scripts/check-exe-version.sh "$sample" 5e8b9f13827f6f68bb56ea7e3e91c7654e0023b4
if scripts/check-exe-version.sh "$sample" aaaaaaa3827f6f68bb56ea7e3e91c7654e0023b4 2>/dev/null; then
  echo "check-exe-version.sh nimmt eine falsche Build-ID an" >&2
  exit 1
fi
rm "$sample"
# Erst bauen, dann mit Zeitlimit testen: ein hängender Test bricht ab, statt
# den Lauf endlos zu blockieren. Welcher hängt, steht in der Ausgabe
# ("has been running for over 60 seconds").
cargo test --no-run
timeout 10m cargo test
