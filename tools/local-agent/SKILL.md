---
name: local-agent
description: Offload a task to Strata, the local model on the home PC, through the local-agent runner. Use when the user says "offload" or "local agent", or to suggest it for a well-scoped, mechanical task.
---

# local-agent

`local-agent run` starts a headless Claude Code worker whose model is Strata, reached through the Proxy on the NAS. It works on a new branch in its own worktree; the user's checkout stays untouched. Strata is slow and free; a cloud agent is fast and costs tokens.

If `local-agent` isn't on `PATH`, tell the user to run `tools/local-agent/install.sh` from their Strata-Home-Proxy checkout, then `local-agent check`.

Offload only when the user asks. When a task is well scoped and mechanical (a rename, a test for a known function, a doc pass), suggest offloading it in one line and wait for the answer.

## Offload a task

1. Write the task as the worker's whole brief: the goal, the files involved, how to check the work (the test command), and what done looks like. The worker sees the last commit, not uncommitted changes; commit or mention them first.
2. Start the run as a **background** Bash command from inside the repo, so you are woken when it ends:

   ```sh
   local-agent run "<task>"
   ```

   Its first line names the run id. Tell the user in one line that the task is running on Strata. Only one run talks to Strata at a time; a later run waits its turn and says so.

   Options, before the task:
   - `--read-only` for research, review or questions: the worker reads the repo as it is, with no edit tools, no commits and no branch. The repo's allow rules don't apply; `--allow` only read commands here.
   - `--watch` only when the user wants live reports: the run names a progress file; follow it with the Monitor tool (`tail -f <file>`) and relay milestones briefly.
   - `--allow 'Bash(cargo test:*)'` (repeatable) when the task needs a command beyond the base set (read, edit, `git status/diff/add/commit`, `ls`, `grep`) and the repo's own allow rules in `.claude/settings.json`. Network tools and `git push` stay blocked whatever you pass.
3. When the user asks how it's going, answer from `local-agent status <run>`.
4. When the command ends, act on its exit code.

## Offload the build step of /implement or /implement-spec

Only when the user asks for it; otherwise implement as usual.

- Commit first: a run starts from the current branch's HEAD. For `/implement-spec`, check out the integration branch first.
- One run per ticket, one at a time: Strata takes one run at a time, so start the next ticket when the previous run has ended, not in parallel. The brief is the ticket (title, body, link to the spec) plus the test command.
- Pass `--allow Skill` so the worker can call `tdd`. The repo's allow rules already let it run `cargo test`, `cargo clippy`, `cargo fmt` and `scripts/check.sh`.
- Exit codes as below, with one change for exit 0: after your review, merge the branch into the current branch yourself; asking for /implement covers the merge. A fallback agent gets the same brief and also uses `tdd`.
- `/code-review` and the final commit stay with you, after the last ticket.

## Exit codes

- **0, done**: run `local-agent result <run>`. Review the summary and the whole diff as you would a colleague's work: Strata makes more mistakes than you. Fix small problems on the branch yourself or name them. Then propose merging the branch (`git merge local-agent/<run>`) and wait for the user's word; the merge is theirs to approve. When they reject the work, offer `local-agent discard <run>`, which removes the worktree and branch. `local-agent prune` cleans up merged worktrees and runs older than 14 days.
- **3, fall back**: Strata can't do the task. `local-agent result <run>` names the reason, and for setup problems what to fix. Hand the task to a normal agent: the Agent tool with model `sonnet`, the same brief, and the same limits: read-only stays read-only; a writing run works on a branch, not on the user's checkout, with no `git push` and no network tools.
  - When the result shows a **Log summary**, Strata failed mid-run and its work is committed in the worktree it names. Have the agent continue there, on that branch: give it the brief, the log summary, and the instruction to finish the work rather than start over.
  - Otherwise the worker never started; the agent starts fresh on a new branch.

  When the agent is done, review the whole diff as in exit 0. Your report says that the fallback did the work, and why, including any fix the user should make, and suggests `local-agent check` once it's fixed.
- **4, timed out**: the run hit its time limit (2 hours, or `--timeout 30m`). Usually the model went in circles, and a fallback would repeat that at full cost, so don't fall back. Report the timeout with the last steps from `local-agent result <run>`; the branch keeps what was done.
- **2, setup**: the message says what is missing in the configuration. Pass it on to the user.
- **1**: local-agent itself failed; show the user its message.
