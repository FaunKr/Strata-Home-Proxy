#!/usr/bin/env bash
# Decides which CI jobs a pull request needs, from the paths it changes.
#
# Usage: <changed paths, one per line> | scripts/ci-select.sh
#        scripts/ci-select.sh --all
# Prints test=, packages=, windows= and image= for $GITHUB_OUTPUT. An empty
# package list means the whole workspace (scripts/check.sh without arguments).
# scripts/ci-select.test.sh checks this mapping against the workspace.
set -euo pipefail

all() { printf '%s\n' test=true packages= windows=true image=true; }
if [[ ${1:-} == --all ]]; then
  all
  exit 0
fi

packages=()
tools=false
windows=false
image=false
while IFS= read -r path; do
  case $path in
    # Shared: whatever changes here can break any job. check.sh also runs the
    # scripts' own tests, which only an unscoped run covers.
    Cargo.toml | Cargo.lock | rust-toolchain* | .cargo/* | .github/workflows/* | scripts/* | deploy/truenas-auto-update.sh)
      all
      exit 0
      ;;
    # The NAS proxy links the web UI.
    crates/admin-ui/*) packages+=(strata-admin-ui strata-nas-proxy) image=true ;;
    crates/nas-proxy/*) packages+=(strata-nas-proxy) image=true ;;
    crates/pc-agent/*) packages+=(strata-pc-agent) windows=true image=true ;;
    # The image builds every crate.
    crates/*) image=true ;;
    # The proxy embeds the install script (include_str!), the image copies it.
    deploy/windows/*) packages+=(strata-nas-proxy) windows=true image=true ;;
    Dockerfile | .dockerignore) image=true ;;
    # The local-agent runner has its own tests in check.sh, which only an
    # unscoped run covers.
    tools/local-agent | tools/local-agent/*) tools=true ;;
  esac
done

packages=$(printf '%s\n' ${packages[@]+"${packages[@]}"} | sort -u | xargs)
echo "test=$([[ -n $packages || $tools == true ]] && echo true || echo false)"
echo "packages=$packages"
echo "windows=$windows"
echo "image=$image"
