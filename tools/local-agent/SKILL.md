---
name: local-agent
description: Offload a task to the local model through the local-agent runner. Use when the user says "offload" or "local agent", or to suggest it for a well-scoped, mechanical task.
---

# local-agent

`local-agent run` starts a headless Claude Code worker whose model is the local model, reached through the local-agent server configured in `~/.config/local-agent/env` (on Windows `%APPDATA%\local-agent\env`). It runs on macOS, Linux and Windows. It works on a new branch in its own worktree; the user's checkout stays untouched. The local model is slow and free; a cloud agent is fast and costs tokens.

If `local-agent` isn't on `PATH`, tell the user to run local-agent's `install.sh` (on Windows `install.ps1`), then `local-agent check`.

Offload only when the user asks. When a task is well scoped and mechanical (a rename, a test for a known function, a doc pass), suggest offloading it in one line and wait for the answer.

## Before you offload

- **Size it.** Estimate the change. A run has 2 hours by default and a context that compacts when it fills. Above about 500 changed lines, or more than one layer (backend and UI, say), split the work into thin vertical slices, one run each, or pass a longer `--timeout` and say why.
- **Check that the worker can verify its work.** The worker runs on the user's machine, with that machine's toolchain. Run the brief's check command there once yourself, or `local-agent check`. If something it needs is missing (a compiler target, a linter, a browser for UI checks), tell the user what to install before the run, or name in the brief which checks the worker can't run.
- **Commit first.** The worker sees the last commit, not uncommitted changes.

## Write the brief

The brief is the worker's whole context. Include:

1. The goal and what done looks like (acceptance criteria).
2. The files and modules involved, and existing code to follow as prior art.
3. The check loop: the narrow command to run after each step (one package's tests, one test file), and the full check command to run before finishing.
4. These working rules, verbatim:
   - Work test first: one failing test, make it pass, then commit with a message naming the behaviour. Commit after every green step, never only at the end.
   - Run the narrow check after each step, and the full check before you finish.
   - When new code needs what existing code does, extract a shared function instead of copying it.
   - Test the paths around the happy one: starting again after a cancel, a restart in the middle, an error in each step, state left behind (flags, saved files, guards) after each of these.
   - End with a summary that lists every check command you ran with its last output line, every check you could not run and why, and what is left open.
5. What is out of scope.

## Offload a task

1. Start the run as a **background** Bash command from inside the repo, so you are woken when it ends:

   ```sh
   local-agent run "<brief>"
   ```

   Its first line names the run id. Tell the user in one line that the task is running on the local model. Only one run talks to the local model at a time; a later run waits its turn and says so.

   Options, before the task:
   - `--read-only` for research, review or questions: the worker reads the repo as it is, with no edit tools, no commits and no branch. The repo's allow rules don't apply; `--allow` only read commands here.
   - `--watch` only when the user wants live reports: the run names a progress file; follow it with the Monitor tool (`tail -f <file>`) and relay milestones briefly.
   - `--allow '<rule>'` (repeatable, e.g. `--allow 'Bash(make test:*)'`) when the task needs a command beyond the base set (read, edit, `git status/diff/add/commit`, `ls`, `grep`) and the repo's own allow rules in `.claude/settings.json`. Network tools and `git push` stay blocked whatever you pass.
2. When the user asks how it's going, answer from `local-agent status <run>`. Its `context:` line shows how full the local model's context is and how often the worker compacted; pass on a `warning:` line as it stands. Also look at the worktree it names (`git -C <worktree> log --oneline` and `git -C <worktree> diff --stat`) to say what has been written and committed.
3. When the command ends, act on its exit code.

## Offload the build step of /implement or /implement-spec

Only when the user asks for it; otherwise implement as usual.

- For `/implement-spec`, check out the integration branch first.
- One run per ticket, one at a time: the local model takes one run at a time, so start the next ticket when the previous run has ended, not in parallel. The brief is the ticket (title, body, link to the spec) plus the check loop and the working rules above.
- Pass `--allow Skill` so the worker can call `tdd`, and `--allow` the repo's test and lint commands if its allow rules don't cover them.
- Exit codes as below, with one change for exit 0: after your review and `/code-review`, merge the branch into the current branch yourself; asking for /implement covers the merge. A fallback agent gets the same brief and also uses `tdd`.
- The final commit stays with you, after the last ticket.

## Exit codes

- **0, done**: run `local-agent result <run>`. Then:
  1. Read the summary. Any check the worker could not run is unverified: run it yourself before you trust the work.
  2. Review the whole diff as you would a colleague's work: the local model makes more mistakes than you. Look hardest at cancel, restart and error paths.
  3. Run the full check command yourself.
  4. Run `/code-review` over the branch's diff. This step is fixed, not optional.
  5. Fix what you found on the branch, or name it.
  6. Before anything leaves the machine, check the commit messages: a `Co-Authored-By` trailer that names a Claude model is wrong for the worker's commits (the local model wrote them), and session or conversation links never belong in a commit. Reword them on the run's branch.
  7. Propose merging the branch (`git merge local-agent/<run>`) and wait for the user's word; the merge is theirs to approve. When they reject the work, offer `local-agent discard <run>`, which removes the worktree and branch.
- **3, fall back**: the local model can't do the task. `local-agent result <run>` names the reason, and for setup problems what to fix. For a failure mid-run, its log summary's `context:` line says whether the context ran full: report that with the reason instead of guessing a cause. Hand the task to a normal agent: the Agent tool with model `sonnet`, the same brief, and the same limits: read-only stays read-only; a writing run works on a branch, not on the user's checkout, with no `git push` and no network tools.
  - When the result shows a **Log summary**, the local model failed mid-run and its work is committed in the worktree it names. Have the agent continue there, on that branch: give it the brief, the log summary, and the instruction to finish the work rather than start over.
  - Otherwise the worker never started; the agent starts fresh on a new branch.

  When the agent is done, review as in exit 0. Your report says that the fallback did the work, and why, including any fix the user should make, and suggests `local-agent check` once it's fixed.
- **4, timed out**: the run hit its time limit (2 hours, or `--timeout 30m`). Usually the model went in circles, and a fallback would repeat that at full cost, so don't fall back. Report the timeout with the last steps from `local-agent result <run>`; the branch keeps what was done. If the brief was large, suggest splitting it.
- **2, setup**: the message says what is missing in the configuration. Pass it on to the user.
- **1**: local-agent itself failed; show the user its message.

## Clean up

Each run's worktree keeps its own build output, which can take gigabytes. After a merge or discard, run `local-agent prune`, which cleans up merged worktrees and runs older than 14 days, and tell the user when a worktree's build folder is large enough to matter for their disk.
