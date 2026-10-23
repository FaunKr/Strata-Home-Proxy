#!/usr/bin/env bash
# Installs the pinned PSScriptAnalyzer, with its checksum, as a PowerShell
# module into the given directory (default: pwsh's user module folder). The CI
# test job and the cloud session hook both use it, so the version lives in one
# place. The PowerShell Gallery is unreachable from cloud sessions; the GitHub
# release carries the same package.
set -euo pipefail
version=1.23.0
sum=b9969345f78251112fb75bdddc789be8689a343472a2e09762303aa03287afab
dir=${1:-$HOME/.local/share/powershell/Modules}
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL -o "$tmp" "https://github.com/PowerShell/PSScriptAnalyzer/releases/download/$version/PSScriptAnalyzer.$version.nupkg"
echo "$sum  $tmp" | sha256sum -c --quiet
target=$dir/PSScriptAnalyzer/$version
rm -rf "$target"
mkdir -p "$target"
# Only the module, the Windows PowerShell 5.1 profile check.sh targets and the
# rule's default reference profile, without which it fails; all profiles
# together are 300 MB.
unzip -q "$tmp" -d "$target" -x '_rels/*' 'package/*' '\[Content_Types\].xml' '*.nuspec' 'compatibility_profiles/*'
unzip -q "$tmp" -d "$target" \
  'compatibility_profiles/win-48_x64_10.0.17763.0_5.1.17763.316_x64_4.0.30319.42000_framework.json' \
  'compatibility_profiles/win-8_x64_10.0.17763.0_6.2.4_x64_4.0.30319.42000_core.json'
