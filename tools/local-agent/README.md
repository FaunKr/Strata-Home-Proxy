# local-agent

Hands tasks from Claude Code to Strata. `local-agent run` starts a headless
`claude -p` whose API is the proxy on the NAS; Strata thus runs the whole agent
loop, in a worktree of its own on a new branch. The `local-agent` skill
([SKILL.md](SKILL.md)) tells Claude when and how.
Design and decisions: issue #107.

Runs on macOS, Linux and Windows. On macOS and Linux `local-agent` is a Bash
script that needs git, jq and curl; on Windows `local-agent.ps1` runs on
Windows PowerShell 5.1 (or PowerShell 7) and needs only git and claude. Both
runners have the same commands, output and exit codes. Cloud sessions cannot
reach the NAS.

**Changing local-agent:** every change to its behaviour goes into both runners
(`local-agent` and `local-agent.ps1`) and both test scripts (`test.sh` and
`test.ps1`) in the same PR.

## Setup

1. `tools/local-agent/install.sh` copies `local-agent` to `~/.local/bin`
   (or `PREFIX`) and creates `~/.config/local-agent/env`, readable only by you.
2. In the web UI's *Access* area, create a key for this computer and enter it
   together with the proxy's address:

   ```sh
   LOCAL_AGENT_URL=http://<NAS-IP>:8080
   LOCAL_AGENT_KEY=<key from Access>
   ```

   Optional: `LOCAL_AGENT_MODEL` (a Claude model name; Strata ignores it,
   Claude Code demands a known one) and `LOCAL_AGENT_COMPACT_SHARE`
   (the share of Strata's context in percent from which Claude Code compacts;
   default 75).
3. In Strata (model config on the PC):
   - Context at least 128K; below that every run falls back. At 64K
     Claude Code went in circles compacting when working with many files: after
     a compaction 20–29K were already used, and one file read adds up to
     15K.
   - `fit_max_tokens` on, as a safety net: Strata then shortens `max_tokens`
     instead of refusing with 400. That does not keep a run alive, it only turns
     a late 400 into ever shorter answers; compaction protects against the full
     context (see below).
   - `"anthropic_thinking": "on_request"`, so that Claude Code's short
     helper requests do not think first.
4. `local-agent check` checks the configuration, the key, `/health`, the context
   and a tool call through Strata and, on an error, names what to do.
5. Save the skill: create the content of [SKILL.md](SKILL.md) as a personal
   skill `local-agent`.

### On Windows

1. In PowerShell, run
   `powershell -NoProfile -ExecutionPolicy Bypass -File tools\local-agent\install.ps1`.
   It needs no administrator rights. It copies the runner to
   `%LOCALAPPDATA%\Programs\local-agent` (or `PREFIX`) and adds that folder to
   your user PATH. It also creates `%APPDATA%\local-agent\env`, readable only
   by you, and warns when git or claude is missing. Open a new terminal
   afterwards.
2. Two shims call the runner: `local-agent.cmd` for PowerShell and cmd, and an
   extensionless `local-agent` for Git Bash, which Claude Code uses for its
   Bash tool. Both run `lib\local-agent.ps1` with `-ExecutionPolicy Bypass`,
   so the execution policy doesn't matter.
3. Fill in the config as in step 2 above, set up Strata as in step 3, run
   `local-agent check`, and save the skill as in step 5.
4. Runs, worktrees and logs live under `%LOCALAPPDATA%\local-agent\`. Paths
   in a worktree may get longer than Windows' 260 characters, so git runs with
   `core.longpaths` (the repo's config stays as it is). Other programs need
   long paths switched on in Windows: `check` warns when `LongPathsEnabled` is
   off (an administrator sets
   `HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled` to 1).
5. `--watch` names the progress file; follow it in Git Bash with `tail -f`.

## Checking compaction

Claude Code knows nothing of Strata's context. Without a setting of its own it
did not compact at all in `-p` mode (Claude Code 2.1.218, 2026-10-19): run #130
grew to 253K tokens and ended at Strata's 400. Nor did
`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` alone, relative to the assumed 200K, have any
effect. So before each run (and in `local-agent check`) the runner sets the
compaction window `CLAUDE_CODE_AUTO_COMPACT_WINDOW` explicitly, computed from
Strata's `max_context`:

- target = `LOCAL_AGENT_COMPACT_SHARE` (default 75) % of `max_context`;
- window = target + 8192 (output limit) + 13000, clamped to 100000–180000.

As measured, Claude Code compacts at about window − output limit − 13K: with
a window of 100K at 80–89K, with 150K at 130K. At 256K that gives the
window 180000 and compaction at around 159K; at 128000 the window 117192
and around 96K. Below 100K Claude Code does not accept a window; only then does
the runner also set `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` (target divided by
1000, i.e. percent of the 100K window). The upper limit of 180K lies on purpose
below Claude Code's 200K model window, because the range above it is untested.
`local-agent check` shows the window and the approximate point of
compaction. During a run, `local-agent status` (and, on failure, the log
summary in `local-agent result`) shows how full the context is and how often
it has compacted so far. If the context is more than 20K above the point of
compaction without Claude Code having compacted, a warning appears there; with
`--watch` it lands once in the progress file. Measurements:
`docs/research/fit-max-tokens-and-autocompact.md` (section "Hardware test").

Prove it once on real hardware, with the context Strata runs:

```sh
cd <this repo>
local-agent run --read-only --watch "Read every file under crates/nas-proxy/src and crates/admin-ui/src in full, one after the other, and summarise each in one sentence. No subagents: read every file yourself."
```

Passed when the run ends with exit 0 and the progress file (or
`local-agent status`) shows `context compacted` at least once. If instead
`request exceeds the context window` is in the log
(`~/.local/state/local-agent/runs/<run>/log.jsonl`, on Windows
`%LOCALAPPDATA%\local-agent\runs\<run>\log.jsonl`), Claude Code compacts
too late: lower `LOCAL_AGENT_COMPACT_SHARE` and check again.

## Tests

`tools/local-agent/test.sh` (runs in `scripts/check.sh`) starts the real
runner against a faked proxy (`testdata/fake-proxy.py`) and a faked
`claude` (`testdata/fake-claude`). `tools/local-agent/test.ps1` does the same
for `local-agent.ps1` under pwsh on Linux, with a test of the same name for
each test in `test.sh`; `scripts/check.sh` runs it and PSScriptAnalyzer, which
checks the PowerShell against Windows PowerShell 5.1. Nothing runs on real
Windows in CI; the manual `windows.yml` workflow parses the scripts under 5.1.

What pwsh on Linux does not enforce, so `test.ps1` can't catch it; check a
change to `local-agent.ps1` for it by reading:

- File sharing: a file another process holds open for writing (the worker's
  `log.jsonl`, `stderr`) opens only with `FileShare.ReadWrite`; plain
  `File.ReadAllLines` or `Get-Content` fails on Windows while the run goes on.
- Native stderr: with `$ErrorActionPreference = 'Stop'`, Windows PowerShell
  5.1 throws on a native command's stderr line when it's redirected (`git`
  goes through `Invoke-Git`; set `Continue` around others such as `taskkill`).
- The Windows-only branches: `taskkill`, the registry and PATH in
  `install.ps1`, ACLs, `LongPathsEnabled`.
