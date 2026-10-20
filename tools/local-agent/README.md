# local-agent

Hands tasks from Claude Code to Strata. `local-agent run` starts a headless
`claude -p` whose API is the proxy on the NAS; Strata thus runs the whole agent
loop, in a worktree of its own on a new branch. The `local-agent` skill
([SKILL.md](SKILL.md)) tells Claude when and how.
Design and decisions: issue #107.

Runs on macOS and Linux, with Bash, git, jq and curl. Cloud sessions
cannot reach the NAS.

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
(`~/.local/state/local-agent/runs/<run>/log.jsonl`), Claude Code compacts
too late: lower `LOCAL_AGENT_COMPACT_SHARE` and check again.

## Tests

`tools/local-agent/test.sh` (runs in `scripts/check.sh`) starts the real
runner against a faked proxy (`testdata/fake-proxy.py`) and a faked
`claude` (`testdata/fake-claude`).
