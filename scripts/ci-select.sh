#!/usr/bin/env bash
# Decides which CI jobs a pull request needs, from the paths it changes, and
# which images a push to main publishes, from the paths changed since their
# last publish.
#
# Usage: <changed paths, one per line> | scripts/ci-select.sh
#        scripts/ci-select.sh --all
# Prints test=, packages=, cargo=, image= (the proxy's image), bench= (the code
# benchmark runner's image) and ui= (the browser check) for $GITHUB_OUTPUT. In a
# pull request the images build only when their build definition changes: the
# test job already compiles the code. On main, publish= and publish-bench= say
# whether the paths changed since an image's last publish go into that image
# (its build context minus the test-only paths in .dockerignore). An empty
# package list means the whole workspace (scripts/check.sh without arguments);
# with cargo=false no crate at all (scripts/check.sh --no-cargo): the change
# only touches what check.sh checks without Cargo, such as the runners' tests.
# scripts/ci-select.test.sh checks this mapping against the workspace.
set -euo pipefail

# Every pull request job, with the whole workspace.
all_checks() { printf '%s\n' test=true packages= cargo=true image=true bench=true ui=true; }
if [[ ${1:-} == --all ]]; then
  all_checks
  printf '%s\n' publish=true publish-bench=true
  exit 0
fi
everything=false
packages=()
unscoped=false
# The runners' own tests, which check.sh runs without Cargo too.
runners=false
image=false
bench=false
# The browser check (scripts/ui-check.sh): the web UI and the proxy that serves it.
ui=false
publish=false
publish_bench=false
while IFS= read -r path; do
  # The proxy's publish job: what goes into its image. The CI workflow publishes
  # both images, so a broken publish step shows up at its merge; the code
  # benchmark's image publishes with bench below.
  if [[ $path == .github/workflows/ci.yml ]]; then
    publish=true publish_bench=true
  elif [[ $path =~ ^crates/[^/]+/tests/ || $path == crates/pc-agent/fixtures/* || $path == crates/pc-agent/strata-pc-agent.example.toml ]]; then
    # Read by tests only; .dockerignore keeps them out of the image.
    :
  else
    case $path in
      Cargo.toml | Cargo.lock | crates/* | deploy/windows/install.ps1 | tools/code-bench/tasks/* | scripts/check-exe-version.sh | Dockerfile | .dockerignore) publish=true ;;
    esac
  fi
  case $path in
    # Shared: whatever changes here can break any job.
    Cargo.toml | Cargo.lock | rust-toolchain* | .cargo/* | .config/* | .github/workflows/* | scripts/* | deploy/truenas-auto-update.sh)
      everything=true
      ;;
    # The NAS proxy links the web UI.
    crates/admin-ui/*) packages+=(strata-admin-ui strata-nas-proxy) ui=true ;;
    crates/nas-proxy/*) packages+=(strata-nas-proxy) ui=true ;;
    # The proxy embeds the changelog and its unit test reads the crate's version.
    crates/pc-agent/CHANGELOG.md | crates/pc-agent/Cargo.toml) packages+=(strata-pc-agent strata-nas-proxy) ;;
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
    # crate, so no crate needs a build for it.
    tools/local-agent | tools/local-agent/*) runners=true ;;
    # The product changelog: check.sh checks its section for the version.
    CHANGELOG.md) runners=true ;;
    # The code benchmark: the proxy embeds its tasks (include_str!), check.sh
    # runs the runner's tests; its image holds the runner and the repo task,
    # built from repo.json by prepare_repo_task.py.
    tools/code-bench/tasks/*/repo.json) bench=true publish_bench=true ;;
    tools/code-bench/tasks/*) packages+=(strata-nas-proxy) ;;
    tools/code-bench/Dockerfile | tools/code-bench/runner.py | tools/code-bench/prepare_repo_task.py) runners=true bench=true publish_bench=true ;;
    tools/code-bench | tools/code-bench/*) runners=true ;;
  esac
done

if [[ $everything == true ]]; then
  all_checks
  echo "publish=$publish"
  echo "publish-bench=$publish_bench"
  exit 0
fi
package_list=$(printf '%s\n' ${packages[@]+"${packages[@]}"} | sort -u | xargs)
cargo=$([[ -n $package_list || $unscoped == true ]] && echo true || echo false)
echo "test=$([[ $cargo == true || $runners == true ]] && echo true || echo false)"
echo "packages=$package_list"
echo "cargo=$cargo"
echo "image=$image"
echo "bench=$bench"
echo "ui=$ui"
echo "publish=$publish"
echo "publish-bench=$publish_bench"
