---
name: local-agent
description: Offload a task to Strata, the local model on the home PC, through the local-agent runner. Use when the user says "offload" or "local agent", or to suggest it for a well-scoped, mechanical task.
---

# local-agent

`local-agent run` starts a headless Claude Code worker whose model is Strata, reached through the Proxy on the NAS. It works on a new branch in its own worktree; the user's checkout stays untouched. Strata is slow and free; a cloud agent is fast and costs tokens.

Offload only when the user asks. When a task is well scoped and mechanical (a rename, a test for a known function, a doc pass), suggest offloading it in one line and wait for the answer.

## Offload a task

1. Write the task as the worker's whole brief: the goal, the files involved, how to check the work (the test command), and what done looks like. The worker sees the last commit, not uncommitted changes; commit or mention them first.
2. Start the run as a **background** Bash command from inside the repo, so you are woken when it ends:

   ```sh
   local-agent run "<task>"
   ```

   Its first line names the run id. Tell the user in one line that the task is running on Strata.

   Options, before the task:
   - `--read-only` for research, review or questions: the worker reads the repo as it is, with no edit tools and no branch.
   - `--allow 'Bash(cargo test:*)'` (repeatable) when the task needs a command beyond the base set (read, edit, `git status/diff/add/commit`, `ls`, `grep`) and the repo's own allow rules in `.claude/settings.json`. Network tools and `git push` stay blocked whatever you pass.
3. When the command ends, act on its exit code.

## Exit codes

- **0, done**: run `local-agent result <run>`. Review the summary and the whole diff as you would a colleague's work: Strata makes more mistakes than you. Fix small problems on the branch yourself or name them. Then propose merging the branch (`git merge local-agent/<run>`) and wait for the user's word; the merge is theirs to approve.
- **3, fall back**: Strata can't do the task. `local-agent result <run>` names the reason, and for setup problems what to fix. Hand the task to a normal agent: the Agent tool with model `sonnet`, the same brief, and the same limits: read-only stays read-only; a writing run works on a new branch, not on the user's checkout, with no `git push` and no network tools. When it's done, review its work as in exit 0. Your report says that the fallback did the work, and why, including any fix the user should make.
- **2, setup**: the message says what is missing in the configuration. Pass it on to the user.
- **1**: local-agent itself failed; show the user its message.
