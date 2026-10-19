#!/usr/bin/env bash
# Decides which CI jobs a pull request needs, from the paths it changes.
#
# Usage: <changed paths, one per line> | scripts/ci-select.sh
#        scripts/ci-select.sh --all
# Prints test=, packages=, image= (the proxy's image) and bench= (the code
# benchmark runner's image) for $GITHUB_OUTPUT. The images build only when
# their build definition changes: the test job already compiles the code. An empty
# package list means the whole workspace (scripts/check.sh without arguments).
# scripts/ci-select.test.sh checks this mapping against the workspace.
set -euo pipefail

all() { printf '%s\n' test=true packages= image=true bench=true; }
if [[ ${1:-} == --all ]]; then
  all
  exit 0
fi

packages=()
tools=false
image=false
bench=false
while IFS= read -r path; do
  case $path in
    # Shared: whatever changes here can break any job. check.sh also runs the
    # scripts' own tests, which only an unscoped run covers.
    Cargo.toml | Cargo.lock | rust-toolchain* | .cargo/* | .github/workflows/* | scripts/* | deploy/truenas-auto-update.sh)
      all
      exit 0
      ;;
    # The NAS proxy links the web UI.
    crates/admin-ui/*) packages+=(strata-admin-ui strata-nas-proxy) ;;
    crates/nas-proxy/*) packages+=(strata-nas-proxy) ;;
    crates/pc-agent/*) packages+=(strata-pc-agent) ;;
    # A crate without its own rule: the whole workspace.
    crates/*) tools=true ;;
    # The proxy embeds the install script (include_str!); check.sh also checks
    # that it parses.
    deploy/windows/*) packages+=(strata-nas-proxy) ;;
    Dockerfile | .dockerignore) image=true ;;
    # The local-agent runner: check.sh runs its tests and shellcheck. It is no
    # crate, so the test job runs without a package list, i.e. unscoped.
    tools/local-agent | tools/local-agent/*) tools=true ;;
    # The code benchmark: the proxy embeds its tasks (include_str!), check.sh
    # runs the runner's tests; its image holds only the runner.
    tools/code-bench/tasks/*) packages+=(strata-nas-proxy) tools=true ;;
    tools/code-bench/Dockerfile | tools/code-bench/runner.py) tools=true bench=true ;;
    tools/code-bench | tools/code-bench/*) tools=true ;;
  esac
done

packages=$(printf '%s\n' ${packages[@]+"${packages[@]}"} | sort -u | xargs)
echo "test=$([[ -n $packages || $tools == true ]] && echo true || echo false)"
echo "packages=$packages"
echo "image=$image"
echo "bench=$bench"
