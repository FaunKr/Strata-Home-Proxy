#!/usr/bin/env bash
# Decides which CI jobs a pull request needs, from the paths it changes.
#
# Usage: <changed paths, one per line> | scripts/ci-select.sh
#        scripts/ci-select.sh --all
# Prints test=, packages=, image= (the proxy's image), bench= (the code
# benchmark runner's image) and ui= (the browser check) for $GITHUB_OUTPUT. The images build only when
# their build definition changes: the test job already compiles the code. An empty
# package list means the whole workspace (scripts/check.sh without arguments).
# scripts/ci-select.test.sh checks this mapping against the workspace.
set -euo pipefail

all() { printf '%s\n' test=true packages= image=true bench=true ui=true; }
if [[ ${1:-} == --all ]]; then
  all
  exit 0
fi
packages=()
unscoped=false
image=false
bench=false
# The browser check (scripts/ui-check.sh): the web UI and the proxy that serves it.
ui=false
while IFS= read -r path; do
  case $path in
    # Shared: whatever changes here can break any job. check.sh also runs the
    # scripts' own tests, which only an unscoped run covers.
    Cargo.toml | Cargo.lock | rust-toolchain* | .cargo/* | .config/* | .github/workflows/* | scripts/* | deploy/truenas-auto-update.sh)
      all
      exit 0
      ;;
    # The NAS proxy links the web UI.
    crates/admin-ui/*) packages+=(strata-admin-ui strata-nas-proxy) ui=true ;;
    crates/nas-proxy/*) packages+=(strata-nas-proxy) ui=true ;;
    crates/pc-agent/*) packages+=(strata-pc-agent) ;;
    # The protocol between them: the PC service writes it, the proxy and the web UI read it.
    crates/pc-protocol/*) packages+=(strata-pc-protocol strata-pc-agent strata-admin-ui strata-nas-proxy) ui=true ;;
    # A crate without its own rule: the whole workspace.
    crates/*) unscoped=true ;;
    # The proxy embeds the install script (include_str!); check.sh also checks
    # that it parses.
    deploy/windows/*) packages+=(strata-nas-proxy) ;;
    # The ui job takes cargo-leptos's pin from the Dockerfile.
    Dockerfile) image=true ui=true ;;
    .dockerignore) image=true ;;
    # The local-agent runner: check.sh runs its tests and shellcheck. It is no
    # crate, so the test job runs without a package list, i.e. unscoped.
    tools/local-agent | tools/local-agent/*) unscoped=true ;;
    # The code benchmark: the proxy embeds its tasks (include_str!), check.sh
    # runs the runner's tests; its image holds only the runner.
    tools/code-bench/tasks/*) packages+=(strata-nas-proxy) unscoped=true ;;
    tools/code-bench/Dockerfile | tools/code-bench/runner.py) unscoped=true bench=true ;;
    tools/code-bench | tools/code-bench/*) unscoped=true ;;
  esac
done

package_list=$(printf '%s\n' ${packages[@]+"${packages[@]}"} | sort -u | xargs)
echo "test=$([[ -n $package_list || $unscoped == true ]] && echo true || echo false)"
echo "packages=$package_list"
echo "image=$image"
echo "bench=$bench"
echo "ui=$ui"
