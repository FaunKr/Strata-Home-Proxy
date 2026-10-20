# shellcheck shell=bash
# Sourced by scripts/check.sh and scripts/ui-check.sh: a full disk fails the
# build late and cryptically ("linking with cc failed", "No space left on
# device"). Locally stop early instead; CI runners start empty, so there it
# only warns. Expects target_dir to be set.
: "${target_dir:?}"
free_gb=$(($(df -Pk . | awk 'NR == 2 {print $4}') / 1024 / 1024))
if ((free_gb < 10)); then
  target=$(du -sh "$target_dir" 2>/dev/null | cut -f1 || true)
  # Test binaries take most of it; deleting only them keeps the dependencies built.
  message="Little space left: ${free_gb} GB, $target_dir is ${target:-0}. Delete the test binaries: find $target_dir/debug/deps -maxdepth 1 -type f -executable ! -name '*.so' -delete; if that is not enough, rm -rf $target_dir/debug and remove old worktrees (git worktree list)"
  if [[ -n ${CI:-} ]]; then
    echo "WARNING: $message" >&2
  else
    echo "$message" >&2
    exit 2
  fi
fi
