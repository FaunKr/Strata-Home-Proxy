---
name: steward
description: Bring a PR in this repo to merge. Read when opening a PR, after every push, before waiting on CI, on every CI or review event and before every merge.
---

1. **Mergeable first.** Before waiting for CI and before merging, read the PR's `mergeable_state`. On `dirty`, merge in `origin/main` with a merge commit, never by rebase, and run `scripts/check.sh`. Then push, and only then wait for CI. A CI run on a head with a conflict does not count.
2. **Fail fast.** While CI runs, read each job as it finishes (`actions_list` on the run's jobs). A failed job is work now: read its log and fix it, without waiting for the rest of the run.
3. **Green means:** the check `ci-ok` succeeded on the current head (it combines `test`, `image` and `ui`; jobs that are not needed are skipped), the check `PR text` succeeded on the body as it is now (`ci-ok` does not cover it), and `mergeable_state` is `clean`.
4. **Merge slot:** In the turn the PR turns green, tell the coordinator and ask for the merge slot; merge only after its go. A green PR that waits unreported lets main move on and brings conflicts.
5. **Merge** with a merge commit (`merge_method: merge`), not squash, and with `expectedHeadSha` on the checked head.
6. **PR text:** After creating or editing a PR, read its body back. Tooling may append a Claude session link; remove it (CLAUDE.md, the check `PR text`). One closing keyword per issue: `Closes #241. Closes #242.`, not `Closes #241, #242`.
7. **Review request:** For PRs that Faun started, Faun is the author. A review request to Faun therefore fails. The assignment is enough.
