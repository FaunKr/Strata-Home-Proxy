#!/bin/bash
# Cloud sessions: builds what scripts/check.sh needs ahead, so the first full
# test run does not start cold and run into the command timeout.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR"

rustup target add wasm32-unknown-unknown
# cargo-leptos for scripts/ui-check.sh.
scripts/install-cargo-leptos.sh || true
# cargo-nextest, the test runner of scripts/check.sh, at the version
# .config/nextest.toml pins; get.nexte.st is blocked here, GitHub is not.
nextest_version=$(scripts/nextest-version.sh)
if [[ $(cargo nextest --version 2>/dev/null | head -n1) != "cargo-nextest $nextest_version "* ]]; then
  curl -fsSL "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$nextest_version/cargo-nextest-$nextest_version-$(uname -m)-unknown-linux-gnu.tar.gz" \
    | tar xz -C "$HOME/.cargo/bin" || echo "cargo-nextest $nextest_version not installed" >&2
fi
cargo test --no-run --quiet
# The same arguments as scripts/check.sh, or clippy checks again; a warning
# here is for the session to fix, not a reason to fail the start.
cargo clippy --all-targets --quiet -- -D warnings || true
cargo clippy -p strata-admin-ui --target wasm32-unknown-unknown --no-default-features --features hydrate --quiet -- -D warnings || true
# The Windows check in scripts/check.sh runs only with this target; mingw links
# the PC service for Windows (also what the Docker image uses).
rustup target add x86_64-pc-windows-gnu
if ! command -v x86_64-w64-mingw32-gcc >/dev/null; then
  (apt-get install -y -q gcc-mingw-w64-x86-64 || (apt-get update -q && apt-get install -y -q gcc-mingw-w64-x86-64)) >/dev/null 2>&1 || true
fi
# PowerShell for the parse check of install.ps1 in scripts/check.sh.
if ! command -v pwsh >/dev/null; then
  pwsh_dir="$HOME/.local/share/pwsh"
  mkdir -p "$pwsh_dir" "$HOME/.local/bin"
  if curl -fsSL https://github.com/PowerShell/PowerShell/releases/download/v7.4.6/powershell-7.4.6-linux-x64.tar.gz \
    | tar xz -C "$pwsh_dir"; then
    chmod +x "$pwsh_dir/pwsh" && ln -sf "$pwsh_dir/pwsh" "$HOME/.local/bin/pwsh"
  fi
fi
# The linters shellcheck and actionlint for the lint steps in scripts/check.sh.
if ! command -v shellcheck >/dev/null; then
  (apt-get install -y -q shellcheck || (apt-get update -q && apt-get install -y -q shellcheck)) >/dev/null 2>&1 || true
fi
if ! command -v actionlint >/dev/null; then
  scripts/install-actionlint.sh || true
fi
cargo clippy -p strata-pc-agent --all-targets --target x86_64-pc-windows-gnu --quiet -- -D warnings || true
