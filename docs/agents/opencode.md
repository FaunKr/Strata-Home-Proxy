# Work on an OpenCode Instance

How a Claude thread hands a step of the pipeline (/implement, /code-review, the review's fixes) to an OpenCode Instance of the Open Code Orchestrator. A Task is one OpenCode session the Orchestrator starts on an Instance with one prompt. Create the Instance from the Template `strata-home-proxy`, kind `ephemeral`, with Git credentials: the Template has everything `scripts/check.sh` and `scripts/ui-check.sh` need.

## The Task prompt

- When Faun asks for a skill by OpenCode's `@` syntax, the prompt starts with it (`@implement 417`, `@code-review …`); these skills live on the OpenCode side, not in `.claude/skills/`. The brief follows: the issue, the branch, `CLAUDE.md` and `CODING_STANDARDS.md` to read, and what to leave alone.
- **Push early.** Commit and push as soon as `scripts/check.sh` passes, before `scripts/ui-check.sh`; push again after any fix ui-check needs. A local model can end its turn on a tool call it wrote as text: the work is then only in the Instance's working tree, and a new Task has to commit it (#417 lost about 25 minutes this way).
- The PR is opened by the Claude thread, not the Instance: it carries the attribution block and the steward rules (`.claude/skills/steward/SKILL.md`).
- A review Task changes no files and ends on a numbered list of findings with file:line and the fix.

## When a Task is idle

- Before trusting the reply, read the branch's newest commit on GitHub (`git fetch origin <branch>`, `git log -1 --stat origin/<branch>`) and match it with the commit and files the reply names: an idle Task has not always pushed.
- A ui-check run in the Instance takes 10 to 20 minutes; a Task in it is not stuck before that.
- Stop the Instance once the branch is pushed and the review is read; an ephemeral Instance wipes its workspace on stop.
