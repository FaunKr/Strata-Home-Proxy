# shellcheck shell=bash
# Sourced by scripts/check.sh and scripts/repeat-test.sh: GNU timeout, which
# macOS has from coreutils as gtimeout. Sets timeout, empty when neither exists.
# shellcheck disable=SC2034 # used by the scripts that source it.
timeout=$(command -v timeout || command -v gtimeout || true)
# shellcheck disable=SC2034
timeout_hint="timeout: brew install coreutils (on Linux it is part of coreutils)"
