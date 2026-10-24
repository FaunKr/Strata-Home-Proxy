#!/usr/bin/env bash
# Tests tools/local-agent/local-agent as a black box: each test runs the real
# script in a throwaway git repo, against a fake Proxy (testdata/fake-proxy.py)
# and a fake `claude` on PATH (testdata/fake-claude), and checks only what a
# user sees: exit code, output, worktrees, branches and the original checkout.
#
# Usage: tools/local-agent/test.sh [test_name...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# Without symlinks: macOS's /var is /private/var, and the worker's cwd is
# compared with the repo's path.
tmp=$(cd "$(mktemp -d)" && pwd -P)
proxy_pid=
cleanup() {
  [[ -n $proxy_pid ]] && kill "$proxy_pid" 2>/dev/null
  rm -rf "$tmp"
}
trap cleanup EXIT

export HOME=$tmp/home
export XDG_CONFIG_HOME=$HOME/.config XDG_STATE_HOME=$HOME/.local/state
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
mkdir -p "$HOME" "$tmp/bin"
ln -s "$here/testdata/fake-claude" "$tmp/bin/claude"
export PATH=$tmp/bin:$PATH
export FAKE_CLAUDE_RECORD=$tmp/claude
export FAKE_PROXY_SCENARIO=$tmp/scenario.json

echo '{}' >"$FAKE_PROXY_SCENARIO"
python3 "$here/testdata/fake-proxy.py" "$FAKE_PROXY_SCENARIO" "$tmp/port" &
proxy_pid=$!
for _ in $(seq 50); do [[ -s $tmp/port ]] && break; sleep 0.1; done
url=http://127.0.0.1:$(cat "$tmp/port")
config=$XDG_CONFIG_HOME/local-agent/env

current=
# Tests run in subshells, so failures are counted in a file.
fail() {
  echo "FAIL $current: $*" | tee -a "$tmp/failures" >&2
}
# Asserts that a file or string contains a line, or a substring.
has_line() { grep -qxF -- "$2" "$1" || fail "$1 has no line '$2'"; }
lacks_line() { ! grep -qxF -- "$2" "$1" || fail "$1 has the line '$2'"; }
# A process counts as alive unless it is gone or a zombie (which a container's
# init may never reap).
alive() {
  local stat
  stat=$(ps -o stat= -p "$1" 2>/dev/null || true)
  [[ -n $stat && $stat != Z* ]]
}
contains() { [[ $1 == *"$2"* ]] || fail "'$2' missing in: $1"; }

write_config() {
  mkdir -p "$(dirname "$config")"
  printf 'LOCAL_AGENT_URL=%s\nLOCAL_AGENT_KEY=client-key\n' "$url" >"$config"
  chmod 600 "$config"
}
# Sets the fake Proxy's answers to /health, one JSON object per argument.
health() {
  local IFS=,
  echo "{\"health\": [$*]}" >"$FAKE_PROXY_SCENARIO"
}

# A fresh repo with one commit, as the current directory.
new_repo() {
  repo=$tmp/repos/$current${1:-}
  mkdir -p "$repo"
  cd "$repo"
  git init -q -b main
  echo "hello" >README
  git add README
  git commit -qm init
}

# Runs local-agent; sets $out (stdout and stderr) and $code.
la() {
  code=0
  out=$("$here/local-agent" "$@" 2>&1) || code=$?
}

reset() {
  rm -rf "$FAKE_CLAUDE_RECORD" "$XDG_STATE_HOME" "$FAKE_PROXY_SCENARIO.requests"
  unset FAKE_CLAUDE_MODE
  write_config
  health '{"status": 200, "max_context": 262144}'
  new_repo
}

# The values the fake claude got for an option, one per line.
option_values() {
  awk -v opt="$1" '$0 == opt { on = 1; next } /^--/ { on = 0 } on' "$FAKE_CLAUDE_RECORD/args"
}

run_id() { sed -n 's/^local-agent: run \([^ ]*\).*/\1/p' <<<"$out" | head -1; }

test_happy_path() {
  la run "Add a line to README"
  [[ $code == 0 ]] || fail "exit $code: $out"
  local id
  id=$(run_id)
  [[ -n $id ]] || fail "no run id in: $out"
  # The worker ran in a worktree outside the repo, on a new branch.
  local cwd
  cwd=$(cat "$FAKE_CLAUDE_RECORD/cwd")
  [[ $cwd == "$XDG_STATE_HOME/local-agent/"* ]] || fail "worker ran in $cwd"
  [[ $(git -C "$cwd" branch --show-current) == "local-agent/$id" ]] || fail "wrong branch in $cwd"
  has_line "$FAKE_CLAUDE_RECORD/stdin" "Add a line to README"
  # The edit is committed on the branch; the checkout is untouched.
  git diff --quiet main "local-agent/$id" && fail "branch without a change"
  [[ $(cat README) == hello ]] || fail "checkout changed"
  [[ -z $(git status --porcelain) ]] || fail "checkout not clean"
  # How the worker was set up.
  local env=$FAKE_CLAUDE_RECORD/env
  has_line "$env" "ANTHROPIC_BASE_URL=$url"
  has_line "$env" "ANTHROPIC_AUTH_TOKEN=client-key"
  has_line "$env" "ANTHROPIC_API_KEY=<unset>"
  grep -qx 'ANTHROPIC_MODEL=claude-.*' "$env" || fail "no Claude model name"
  has_line "$env" "CLAUDE_CODE_DISABLE_1M_CONTEXT=1"
  has_line "$env" "CLAUDE_CODE_MAX_OUTPUT_TOKENS=8192"
  # At 256K the window is capped at 180K, without a percentage override.
  has_line "$env" "CLAUDE_CODE_AUTO_COMPACT_WINDOW=180000"
  has_line "$env" "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>"
  # The result shows the summary and the diff.
  la result "$id"
  [[ $code == 0 ]] || fail "result exit $code"
  contains "$out" "Added a line to README."
  contains "$out" "+edited by the worker"
}

test_autocompact_follows_context() {
  # 75 % of 128000, plus the output cap and Claude Code's reserve.
  health '{"status": 200, "max_context": 128000}'
  la run "task"
  [[ $code == 0 ]] || fail "exit $code: $out"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_CODE_AUTO_COMPACT_WINDOW=117192"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>"
}

test_compact_share_changes_window() {
  echo "LOCAL_AGENT_COMPACT_SHARE=70" >>"$config"
  health '{"status": 200, "max_context": 128000}'
  la run "task"
  [[ $code == 0 ]] || fail "exit $code: $out"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_CODE_AUTO_COMPACT_WINDOW=110792"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>"
  # Below Claude Code's 100K minimum the window stays at 100K, and the
  # percentage override brings compaction down to the share.
  echo "LOCAL_AGENT_COMPACT_SHARE=50" >>"$config"
  la run "task"
  [[ $code == 0 ]] || fail "exit $code: $out"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=64"
}

test_config_required() {
  rm "$config"
  la run "task"
  [[ $code == 2 ]] || fail "exit $code"
  contains "$out" "$config"
  [[ -e $FAKE_CLAUDE_RECORD/args ]] && fail "worker started"
  # A config others can read is refused.
  write_config
  chmod 644 "$config"
  la run "task"
  [[ $code == 2 ]] || fail "exit $code for a readable file"
  contains "$out" "chmod 600"
}

test_install() {
  rm "$config"
  code=0
  out=$(PREFIX=$tmp/inst "$here/install.sh" 2>&1) || code=$?
  [[ $code == 0 ]] || fail "install exit $code: $out"
  [[ -x $tmp/inst/local-agent ]] || fail "local-agent not installed"
  # A config template, readable only by the user, that run still refuses.
  [[ -f $config ]] || fail "no template for $config"
  # shellcheck disable=SC2012
  [[ $(ls -l "$config" | cut -c5-10) == ------ ]] || fail "template readable by others"
  contains "$out" "$config"
  "$tmp/inst/local-agent" run task >/dev/null 2>&1 && fail "run with an empty template"
  # A second install keeps the existing config.
  write_config
  PREFIX=$tmp/inst "$here/install.sh" >/dev/null 2>&1
  grep -q client-key "$config" || fail "config overwritten"
}

test_worker_permissions() {
  mkdir -p .claude
  echo '{"permissions": {"allow": ["Bash(cargo test:*)", "Bash(git push:*)"]}}' >.claude/settings.json
  git add .claude && git commit -qm settings
  la run --allow "Bash(make check:*)" "task"
  [[ $code == 0 ]] || fail "exit $code: $out"
  has_line <(option_values --permission-mode) acceptEdits
  local allowed=$tmp/allowed denied=$tmp/denied
  option_values --allowedTools >"$allowed"
  option_values --disallowedTools >"$denied"
  has_line "$allowed" "Edit"
  has_line "$allowed" "Bash(git commit:*)"
  has_line "$allowed" "Bash(cargo test:*)"
  has_line "$allowed" "Bash(make check:*)"
  lacks_line "$allowed" "Bash(git push:*)"
  has_line "$denied" "Bash(git push:*)"
  has_line "$denied" "WebFetch"
  has_line "$denied" "Bash(curl:*)"
  # --allow counts for its run only.
  la run "task"
  lacks_line <(option_values --allowedTools) "Bash(make check:*)"
  # A denied tool doesn't block tools that merely share its first letters.
  la run --allow "Bash(ncdu:*)" "task"
  has_line <(option_values --allowedTools) "Bash(ncdu:*)"
  # Read-only runs work in the checkout: the repo's rules don't apply there.
  export FAKE_CLAUDE_MODE=no-tool
  la run --read-only "task"
  lacks_line <(option_values --allowedTools) "Bash(cargo test:*)"
}

test_read_only() {
  export FAKE_CLAUDE_MODE=no-tool
  la run --read-only "Explain README"
  [[ $code == 0 ]] || fail "exit $code: $out"
  [[ $(cat "$FAKE_CLAUDE_RECORD/cwd") == "$repo" ]] || fail "worker not in the repo"
  [[ $(git branch --format='%(refname:short)') == main ]] || fail "branch created"
  [[ -z $(git worktree list | sed 1d) ]] || fail "worktree created"
  lacks_line <(option_values --allowedTools) "Edit"
  has_line <(option_values --disallowedTools) "Edit"
  has_line <(option_values --disallowedTools) "Write"
  has_line <(option_values --disallowedTools) "Bash(git commit:*)"
  la result "$(run_id)"
  contains "$out" "I would rather not."
  [[ $out != *"## Changes"* ]] || fail "changes with --read-only"
}

# Runs a task that must fall back before the worker starts; $1 is the
# expected reason, $2 a piece of the hint.
expect_fallback() {
  la run "task"
  [[ $code == 3 ]] || fail "exit $code instead of 3: $out"
  [[ ! -e $FAKE_CLAUDE_RECORD/args ]] || fail "worker started"
  [[ $(git branch --format='%(refname:short)') == main ]] || fail "branch created"
  la result "$(run_id)"
  contains "$out" "fallback"
  contains "$out" "$1"
  contains "$out" "$2"
}

test_fallback_gaming() {
  health '{"status": 503, "tag": "gaming"}'
  expect_fallback "gaming mode" "fall back"
  [[ $(wc -l <"$FAKE_PROXY_SCENARIO.requests") == 1 ]] || fail "gaming mode retried"
}

test_fallback_wake_timeout_retries_once() {
  health '{"status": 504, "tag": "wake_timeout"}' '{"status": 200, "max_context": 262144}'
  la run "task"
  [[ $code == 0 ]] || fail "exit $code after a successful retry: $out"
  health '{"status": 504, "tag": "wake_timeout"}'
  rm -r "$FAKE_PROXY_SCENARIO.requests" "$FAKE_CLAUDE_RECORD"
  new_repo -2
  expect_fallback "did not wake" "fall back"
  [[ $(wc -l <"$FAKE_PROXY_SCENARIO.requests") == 2 ]] || fail "not retried exactly once"
}

test_fallback_setup_errors() {
  health '{"status": 401, "tag": "unauthorized"}'
  expect_fallback "key" "Access"
  health '{"status": 401, "tag": "strata_unauthorized"}'
  expect_fallback "Strata refused" "Strata key"
  health '{"status": 503, "tag": "not_configured"}'
  expect_fallback "not set up" "Strata"
  health '{"status": 503, "tag": "no_mac"}'
  expect_fallback "MAC" "PC service"
  health '{"status": 200, "max_context": 32768}'
  expect_fallback "32768" "128K"
  health '{"status": 200, "max_context": 127999}'
  expect_fallback "127999" "128K"
}

test_fallback_proxy_unreachable() {
  sed -i.bak "s|^LOCAL_AGENT_URL=.*|LOCAL_AGENT_URL=http://127.0.0.1:9|" "$config"
  expect_fallback "unreachable" "LOCAL_AGENT_URL"
}

test_fallback_mid_run_keeps_work() {
  export FAKE_CLAUDE_MODE=crash
  la run "task"
  [[ $code == 3 ]] || fail "exit $code instead of 3: $out"
  local id
  id=$(run_id)
  git show "local-agent/$id:README" | grep -qx "half done" || fail "the worker's work is missing on the branch"
  [[ -d $(cat "$FAKE_CLAUDE_RECORD/cwd") ]] || fail "worktree removed"
  la result "$id"
  contains "$out" "fallback"
  contains "$out" "mid-run"
  contains "$out" "API Error: 503"
  contains "$out" "## Log summary"
  contains "$out" "Edit README"
  contains "$out" "+half done"
}

test_warns_without_compaction() {
  export FAKE_CLAUDE_MODE=no-compact
  la run --watch "task"
  [[ $code == 3 ]] || fail "exit $code instead of 3: $out"
  local progress
  progress=$(sed -n 's/^local-agent: progress in //p' <<<"$out")
  [[ $(grep -c '^warning: context past the compaction point' "$progress") == 1 ]] ||
    fail "not a single warning in: $(cat "$progress")"
  la status "$(run_id)"
  contains "$out" "context: 190K now (peak 190K, compacts near 159K), compactions: 0"
  contains "$out" "warning: context past the compaction point"
  # The fallback's log summary names the cause.
  la result "$(run_id)"
  contains "$out" "API Error: 400"
  contains "$out" "compactions: 0"
}

test_fallback_mid_run_gaming() {
  export FAKE_CLAUDE_MODE=crash-gaming
  la run "task"
  [[ $code == 3 ]] || fail "exit $code instead of 3: $out"
  la result "$(run_id)"
  contains "$out" "mid-run"
  contains "$out" "the PC switched to gaming mode"
}

test_time_limit() {
  export FAKE_CLAUDE_MODE=hang
  local started=$SECONDS
  la run --timeout 2s "task"
  [[ $code == 4 ]] || fail "exit $code instead of 4: $out"
  ((SECONDS - started < 15)) || fail "time limit not kept"
  sleep 0.5
  local pid
  for pid in "$(cat "$FAKE_CLAUDE_RECORD/pid")" "$(cat "$FAKE_CLAUDE_RECORD/child")"; do
    ! alive "$pid" || fail "process $pid still running"
  done
  local id
  id=$(run_id)
  git rev-parse -q --verify "local-agent/$id" >/dev/null || fail "branch removed"
  [[ -d $(cat "$FAKE_CLAUDE_RECORD/cwd") ]] || fail "worktree removed"
  la result "$id"
  contains "$out" "timeout"
  contains "$out" "Bash sleep 1000"
  # Durations: seconds, minutes, hours; anything else is refused.
  la run --timeout soon "task"
  [[ $code == 2 ]] || fail "--timeout soon: exit $code"
}

test_status() {
  # While running.
  export FAKE_CLAUDE_MODE=slow FAKE_CLAUDE_SLEEP=3
  "$here/local-agent" run "First line of the task
second line" >"$tmp/run.out" 2>&1 &
  local runner=$!
  for _ in $(seq 50); do
    la status
    [[ $out == *running* ]] && break
    sleep 0.1
  done
  contains "$out" "running"
  contains "$out" "First line of the task"
  wait "$runner" || fail "run failed: $(cat "$tmp/run.out")"
  # Done; without a run id it shows the latest run of this repo.
  la status
  [[ $code == 0 ]] || fail "status exit $code"
  contains "$out" "done"
  contains "$out" "Edit README"
  contains "$out" "context compacted"
  contains "$out" "context: 20K now (peak 50K, compacts near 159K), compactions: 1"
  [[ $out != *warning:* ]] || fail "warning despite compaction: $out"
  # Fallen back.
  health '{"status": 503, "tag": "gaming"}'
  la run "task"
  la status "$(run_id)"
  contains "$out" "fallback"
  contains "$out" "the PC is in gaming mode"
}

test_watch() {
  la run --watch "task"
  [[ $code == 0 ]] || fail "exit $code: $out"
  local progress
  progress=$(sed -n 's/^local-agent: progress in //p' <<<"$out")
  [[ -f $progress ]] || fail "no progress file in: $out"
  has_line "$progress" "started"
  has_line "$progress" "Edit README"
  has_line "$progress" "finished"
  [[ $(grep -c '^warning:' "$progress") == 0 ]] || fail "warning despite compaction"
  # Without --watch the run says only when it starts and ends.
  la run "task"
  [[ $(wc -l <<<"$out") == 2 ]] || fail "output without --watch: $out"
}

test_one_run_at_a_time() {
  export FAKE_CLAUDE_MODE=slow FAKE_CLAUDE_SLEEP=2
  local task pids=()
  for task in A B C; do
    "$here/local-agent" run "$task" >"$tmp/run-$task.out" 2>&1 &
    pids+=($!)
    sleep 0.5
  done
  la status
  contains "$out" "queued"
  for task in 0 1 2; do wait "${pids[$task]}" || fail "run $task failed"; done
  # The workers ran one after another, in the order the runs started.
  [[ $(sort -n "$FAKE_CLAUDE_RECORD/spans" | awk '{print $3}' | tr -d '\n') == ABC ]] ||
    fail "order: $(cat "$FAKE_CLAUDE_RECORD/spans")"
  sort -n "$FAKE_CLAUDE_RECORD/spans" | awk 'NR > 1 && $1 < end { bad = 1 } { end = $2 } END { exit bad }' ||
    fail "workers ran at the same time: $(cat "$FAKE_CLAUDE_RECORD/spans")"
}

test_stale_queue_entry() {
  # A runner killed while waiting leaves its entry behind; it must not block.
  mkdir -p "$XDG_STATE_HOME/local-agent/queue"
  echo 999999 >"$XDG_STATE_HOME/local-agent/queue/0000000000-dead"
  local started=$SECONDS
  la run "task"
  [[ $code == 0 ]] || fail "exit $code: $out"
  ((SECONDS - started < 10)) || fail "a dead entry held up the run"
}

test_discard() {
  la run "task"
  local id worktree
  id=$(run_id)
  worktree=$(cat "$FAKE_CLAUDE_RECORD/cwd")
  la discard "$id"
  [[ $code == 0 ]] || fail "discard exit $code: $out"
  [[ ! -e $worktree ]] || fail "worktree stays"
  ! git rev-parse -q --verify "local-agent/$id" >/dev/null || fail "branch stays"
  [[ $(cat README) == hello && -z $(git status --porcelain) ]] || fail "checkout changed"
  la status "$id"
  contains "$out" "discarded"
  la discard nosuchrun
  [[ $code == 2 ]] || fail "unknown run: exit $code"
  contains "$out" "nosuchrun"
}

test_prune() {
  local merged unmerged new
  la run "old and merged"
  merged=$(run_id)
  git merge -q "local-agent/$merged"
  la run "old, not merged"
  unmerged=$(run_id)
  la run "new"
  new=$(run_id)
  local runs=$XDG_STATE_HOME/local-agent/runs
  touch -t 202601010000 "$runs/$merged" "$runs/$merged/"* "$runs/$unmerged" "$runs/$unmerged/"*
  la prune
  [[ $code == 0 ]] || fail "prune exit $code: $out"
  # Old and merged: run folder, worktree and branch go.
  [[ ! -e $runs/$merged ]] || fail "old run stays"
  ! git rev-parse -q --verify "local-agent/$merged" >/dev/null || fail "merged branch stays"
  ! git worktree list | grep -qF "$merged" || fail "merged worktree stays"
  # Old but unmerged: everything stays, so discard still works.
  [[ -d $runs/$unmerged ]] || fail "unmerged run removed"
  git worktree list | grep -qF "$unmerged" || fail "unmerged worktree removed"
  [[ -d $runs/$new ]] || fail "new run removed"
}

test_check() {
  export FAKE_CLAUDE_MODE=check
  la check
  [[ $code == 0 ]] || fail "check exit $code: $out"
  contains "$out" "ok   tool call"
  contains "$out" "ok   compaction: window 180000, compacts near 159K"
  # The worker check runs with the same settings as a run.
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_CODE_AUTO_COMPACT_WINDOW=180000"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=<unset>"
  # Each failure names its fix.
  export FAKE_CLAUDE_MODE=no-tool
  la check
  [[ $code == 2 ]] || fail "without a tool call: exit $code"
  contains "$out" "FAIL tool call"
  health '{"status": 200, "max_context": 32768}'
  la check
  [[ $code == 2 ]] || fail "32K: exit $code"
  contains "$out" "128K"
  health '{"status": 401, "tag": "unauthorized"}'
  la check
  contains "$out" "Access"
  sed -i.bak "s|^LOCAL_AGENT_URL=.*|LOCAL_AGENT_URL=http://127.0.0.1:9|" "$config"
  la check
  [[ $code == 2 ]] || fail "unreachable: exit $code"
  contains "$out" "LOCAL_AGENT_URL"
  rm "$config"
  la check
  [[ $code == 2 ]] || fail "without config: exit $code"
  contains "$out" "$config"
}

test_interrupted_run() {
  export FAKE_CLAUDE_MODE=hang
  "$here/local-agent" run "task" >"$tmp/run.out" 2>&1 &
  local runner=$!
  for _ in $(seq 50); do
    [[ -s $FAKE_CLAUDE_RECORD/child ]] && break
    sleep 0.1
  done
  kill -TERM "$runner"
  wait "$runner" 2>/dev/null || true
  sleep 0.5
  local pid
  for pid in "$(cat "$FAKE_CLAUDE_RECORD/pid")" "$(cat "$FAKE_CLAUDE_RECORD/child")"; do
    ! alive "$pid" || fail "process $pid still running after cancelling"
  done
  la status
  contains "$out" "stopped"
  # A runner that died without cleaning up counts as stopped, too.
  local id
  id=$(sed -n 's/^local-agent: run \([^ ]*\).*/\1/p' "$tmp/run.out")
  sed -i.bak 's/^state=.*/state=running/; s/^pid=.*/pid=999999/' "$XDG_STATE_HOME/local-agent/runs/$id/meta"
  la status "$id"
  contains "$out" "stopped"
  la discard "$id"
  [[ $code == 0 ]] || fail "discard of a dead run: exit $code: $out"
}

test_fallback_gaming_during_run() {
  export FAKE_CLAUDE_MODE=hang-gaming LOCAL_AGENT_PROBE_INTERVAL=1
  local started=$SECONDS
  la run "task"
  [[ $code == 3 ]] || fail "exit $code instead of 3: $out"
  ((SECONDS - started < 15)) || fail "gaming mode noticed too late"
  ! alive "$(cat "$FAKE_CLAUDE_RECORD/child")" || fail "worker still running"
  la result "$(run_id)"
  contains "$out" "switched to gaming mode"
  contains "$out" "## Log summary"
}

test_prune_keeps_runs_without_commits() {
  # A run that changed nothing isn't "merged": prune keeps its worktree.
  export FAKE_CLAUDE_MODE=no-tool
  la run "task"
  local worktree
  worktree=$(cat "$FAKE_CLAUDE_RECORD/cwd")
  la prune
  [[ -d $worktree ]] || fail "worktree without commits removed"
}

tests=("$@")
if ((${#tests[@]} == 0)); then
  # shellcheck disable=SC2207 # test names have no spaces
  tests=($(declare -F | awk '{print $3}' | grep "^test_"))
fi
for current in "${tests[@]}"; do
  set +e
  (
    set -e
    reset
    "$current"
  )
  rc=$?
  set -e
  ((rc == 0)) || fail "aborted (exit $rc)"
done
cd "$here"
if [[ -s $tmp/failures ]]; then
  echo "local-agent: $(wc -l <"$tmp/failures") failures" >&2
  exit 1
fi
echo "local-agent: ok (${#tests[@]} tests)"
