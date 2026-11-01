# Work on an OpenCode Instance

How a Claude thread hands a step of the pipeline (implement, review, fix) to an OpenCode Instance of the Open Code Orchestrator. The Template `strata-home-proxy` has everything `scripts/check.sh` and `scripts/ui-check.sh` need.

## The Task prompt

- Start with the skill by OpenCode's `@` syntax, as Faun asks (`@implement 417`, `@code-review …`), then the brief: the issue, the branch, `CLAUDE.md` and `CODING_STANDARDS.md` to read, and what to leave alone.
- **Push early.** Commit and push as soon as `scripts/check.sh` passes, before `scripts/ui-check.sh`; push again after any fix ui-check needs. A local model can end its turn on a tool call it wrote as text: the work is then only in the Instance's working tree, and a new Task has to commit it (#417 lost about 25 minutes this way).
- The PR is opened by the Claude thread, not the Instance: it carries the attribution block and the steward rules (`.claude/skills/steward/SKILL.md`).
- A review Task changes no files and ends on a numbered list of findings with file:line and the fix.

## When a Task is idle

- Read the branch on GitHub (`git ls-remote origin <branch>`) before trusting the reply: an idle Task has not always pushed.
- A ui-check run in the Instance takes 10 to 20 minutes; a Task in it is not stuck before that.
- Stop the Ephemeral Instance once the branch is pushed and the review is read.
