---
name: steward
description: Bring a PR in this repo to merge. Read when opening a PR, after every push, before waiting on CI, on every CI or review event and before every merge.
---

1. **Mergeable first.** Before waiting for CI and before merging, read the PR's `mergeable_state`. On `dirty`, merge in `origin/main` with a merge commit, never by rebase, and run `scripts/check.sh`. Then push, and only then wait for CI. A CI run on a head with a conflict does not count.
2. **Check-in:** After opening a PR and after each push, schedule one check-in for when CI can first be done: the lower value in the Minutes column of `docs/ci.md` for the run's kind, counted from the push (e.g. 6 minutes for a code change). If CI still runs then, check again every 3 minutes until it is done. A check-in at the upper value lets a green PR wait unreported for up to the whole spread. A check run listed as `in_progress` long past its usual time can be a stale answer: read the run's jobs (`actions_list`) before you wait longer. A green run sends no event, and other events can be lost, so the check-in is what finds a green PR. Keep it even when events arrive.
3. **Fail fast.** While CI runs, read each job as it finishes (`actions_list` on the run's jobs). A failed job is work now: read its log and fix it, without waiting for the rest of the run.
4. **Green means:** the check `ci-ok` succeeded on the current head (it combines `test`, `image` and `ui`; jobs that are not needed are skipped), the check `PR text` (check run `session-links`) succeeded on the body as it is now (`ci-ok` does not cover it), and `mergeable_state` is `clean`.
5. **Merge slot:** In the turn the PR turns green, tell the coordinator and ask for the merge slot, naming the green checks and that `/code-review` ran over the diff (with what it found fixed); merge only after its go. A green PR that waits unreported lets main move on and brings conflicts.
6. **Merge** with a merge commit (`merge_method: merge`), not squash, and with `expectedHeadSha` on the checked head.
7. **Closed without a merge:** At every check-in, read the PR's `state` too. A PR of yours that closed unmerged is not reopened or replaced on your own: tell Faun, with what the PR's comments and checks show about why, and reopen only on their word.
8. **PR text:** After creating or editing a PR, read its body back; after a push, also check that the body still describes the head (a note about a fixture or a follow-up can be outdated now). Tooling may append a Claude session link; remove it (CLAUDE.md, the check `PR text`). One closing keyword per issue: `Closes #241. Closes #242.`, not `Closes #241, #242`.
9. **Review request:** For PRs that Faun started, Faun is the author. A review request to Faun therefore fails. The assignment is enough.
