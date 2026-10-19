#!/usr/bin/env bash
# Tests tools/local-agent/local-agent as a black box: each test runs the real
# script in a throwaway git repo, against a fake Proxy (testdata/fake-proxy.py)
# and a fake `claude` on PATH (testdata/fake-claude), and checks only what a
# user sees: exit code, output, worktrees, branches and the original checkout.
#
# Usage: tools/local-agent/test.sh [test_name...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
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
has_line() { grep -qxF -- "$2" "$1" || fail "$1 hat keine Zeile '$2'"; }
lacks_line() { ! grep -qxF -- "$2" "$1" || fail "$1 hat die Zeile '$2'"; }
contains() { [[ $1 == *"$2"* ]] || fail "'$2' fehlt in: $1"; }

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
  health '{"status": 200, "max_context": 65536}'
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
  [[ -n $id ]] || fail "keine Lauf-ID in: $out"
  # The worker ran in a worktree outside the repo, on a new branch.
  local cwd
  cwd=$(cat "$FAKE_CLAUDE_RECORD/cwd")
  [[ $cwd == "$XDG_STATE_HOME/local-agent/"* ]] || fail "Worker lief in $cwd"
  [[ $(git -C "$cwd" branch --show-current) == "local-agent/$id" ]] || fail "falscher Branch in $cwd"
  has_line "$FAKE_CLAUDE_RECORD/stdin" "Add a line to README"
  # The edit is committed on the branch; the checkout is untouched.
  git diff --quiet main "local-agent/$id" && fail "Branch ohne Änderung"
  [[ $(cat README) == hello ]] || fail "Checkout geändert"
  [[ -z $(git status --porcelain) ]] || fail "Checkout nicht sauber"
  # How the worker was set up.
  local env=$FAKE_CLAUDE_RECORD/env
  has_line "$env" "ANTHROPIC_BASE_URL=$url"
  has_line "$env" "ANTHROPIC_AUTH_TOKEN=client-key"
  has_line "$env" "ANTHROPIC_API_KEY=<unset>"
  grep -qx 'ANTHROPIC_MODEL=claude-.*' "$env" || fail "kein Claude-Modellname"
  has_line "$env" "CLAUDE_CODE_DISABLE_1M_CONTEXT=1"
  has_line "$env" "CLAUDE_CODE_MAX_OUTPUT_TOKENS=8192"
  has_line "$env" "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=24"
  # The result shows the summary and the diff.
  la result "$id"
  [[ $code == 0 ]] || fail "result exit $code"
  contains "$out" "Added a line to README."
  contains "$out" "+edited by the worker"
}

test_autocompact_follows_context() {
  health '{"status": 200, "max_context": 128000}'
  la run "task"
  [[ $code == 0 ]] || fail "exit $code: $out"
  has_line "$FAKE_CLAUDE_RECORD/env" "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=48"
}

test_config_required() {
  rm "$config"
  la run "task"
  [[ $code == 2 ]] || fail "exit $code"
  contains "$out" "$config"
  [[ -e $FAKE_CLAUDE_RECORD/args ]] && fail "Worker gestartet"
  # A config others can read is refused.
  write_config
  chmod 644 "$config"
  la run "task"
  [[ $code == 2 ]] || fail "exit $code bei lesbarer Datei"
  contains "$out" "chmod 600"
}

test_install() {
  rm "$config"
  code=0
  out=$(PREFIX=$tmp/inst "$here/install.sh" 2>&1) || code=$?
  [[ $code == 0 ]] || fail "install exit $code: $out"
  [[ -x $tmp/inst/local-agent ]] || fail "local-agent nicht installiert"
  # A config template, readable only by the user, that run still refuses.
  [[ -f $config ]] || fail "keine Vorlage für $config"
  # shellcheck disable=SC2012
  [[ $(ls -l "$config" | cut -c5-10) == ------ ]] || fail "Vorlage für andere lesbar"
  contains "$out" "$config"
  "$tmp/inst/local-agent" run task >/dev/null 2>&1 && fail "run mit leerer Vorlage"
  # A second install keeps the existing config.
  write_config
  PREFIX=$tmp/inst "$here/install.sh" >/dev/null 2>&1
  grep -q client-key "$config" || fail "Konfiguration überschrieben"
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
}

test_read_only() {
  export FAKE_CLAUDE_MODE=no-tool
  la run --read-only "Explain README"
  [[ $code == 0 ]] || fail "exit $code: $out"
  [[ $(cat "$FAKE_CLAUDE_RECORD/cwd") == "$repo" ]] || fail "Worker nicht im Repo"
  [[ $(git branch --format='%(refname:short)') == main ]] || fail "Branch angelegt"
  [[ -z $(git worktree list | sed 1d) ]] || fail "Worktree angelegt"
  lacks_line <(option_values --allowedTools) "Edit"
  has_line <(option_values --disallowedTools) "Edit"
  has_line <(option_values --disallowedTools) "Write"
  la result "$(run_id)"
  contains "$out" "I would rather not."
  [[ $out != *"## Changes"* ]] || fail "Änderungen bei --read-only"
}

# Runs a task that must fall back before the worker starts; $1 is the
# expected reason, $2 a piece of the hint.
expect_fallback() {
  la run "task"
  [[ $code == 3 ]] || fail "exit $code statt 3: $out"
  [[ ! -e $FAKE_CLAUDE_RECORD/args ]] || fail "Worker gestartet"
  [[ $(git branch --format='%(refname:short)') == main ]] || fail "Branch angelegt"
  la result "$(run_id)"
  contains "$out" "fallback"
  contains "$out" "$1"
  contains "$out" "$2"
}

test_fallback_gaming() {
  health '{"status": 503, "tag": "gaming"}'
  expect_fallback "Gaming-Modus" "fall back"
  [[ $(wc -l <"$FAKE_PROXY_SCENARIO.requests") == 1 ]] || fail "Gaming-Modus wiederholt"
}

test_fallback_wake_timeout_retries_once() {
  health '{"status": 504, "tag": "wake_timeout"}' '{"status": 200, "max_context": 65536}'
  la run "task"
  [[ $code == 0 ]] || fail "exit $code nach erfolgreicher Wiederholung: $out"
  health '{"status": 504, "tag": "wake_timeout"}'
  rm -r "$FAKE_PROXY_SCENARIO.requests" "$FAKE_CLAUDE_RECORD"
  new_repo -2
  expect_fallback "did not wake" "fall back"
  [[ $(wc -l <"$FAKE_PROXY_SCENARIO.requests") == 2 ]] || fail "nicht genau einmal wiederholt"
}

test_fallback_setup_errors() {
  health '{"status": 401, "tag": "unauthorized"}'
  expect_fallback "key" "Access"
  health '{"status": 401, "tag": "strata_unauthorized"}'
  expect_fallback "Strata refused" "Strata key"
  health '{"status": 503, "tag": "not_configured"}'
  expect_fallback "not set up" "Strata"
  health '{"status": 503, "tag": "no_mac"}'
  expect_fallback "MAC" "PC-Dienst"
  health '{"status": 200, "max_context": 32768}'
  expect_fallback "32768" "65536"
}

test_fallback_proxy_unreachable() {
  sed -i.bak "s|^LOCAL_AGENT_URL=.*|LOCAL_AGENT_URL=http://127.0.0.1:9|" "$config"
  expect_fallback "unreachable" "LOCAL_AGENT_URL"
}

test_fallback_mid_run_keeps_work() {
  export FAKE_CLAUDE_MODE=crash
  la run "task"
  [[ $code == 3 ]] || fail "exit $code statt 3: $out"
  local id
  id=$(run_id)
  git show "local-agent/$id:README" | grep -qx "half done" || fail "Arbeit des Workers fehlt auf dem Branch"
  [[ -d $(cat "$FAKE_CLAUDE_RECORD/cwd") ]] || fail "Worktree entfernt"
  la result "$id"
  contains "$out" "fallback"
  contains "$out" "mid-run"
  contains "$out" "API Error: 503"
  contains "$out" "## Log summary"
  contains "$out" "Edit README"
  contains "$out" "+half done"
}

test_fallback_mid_run_gaming() {
  export FAKE_CLAUDE_MODE=crash-gaming
  la run "task"
  [[ $code == 3 ]] || fail "exit $code statt 3: $out"
  la result "$(run_id)"
  contains "$out" "mid-run"
  contains "$out" "Gaming-Modus"
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
  ((rc == 0)) || fail "abgebrochen (exit $rc)"
done
cd "$here"
if [[ -s $tmp/failures ]]; then
  echo "local-agent: $(wc -l <"$tmp/failures") Fehler" >&2
  exit 1
fi
echo "local-agent: ok (${#tests[@]} Tests)"
