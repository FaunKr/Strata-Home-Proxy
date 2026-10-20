---
name: steward
description: Bring a PR in this repo to merge. Read when opening a PR, after every push, before waiting on CI, on every CI or review event and before every merge.
---

1. **Mergeable first.** Before waiting for CI and before merging, read the PR's `mergeable_state`. On `dirty`, merge in `origin/main` with a merge commit, never by rebase, and run `scripts/check.sh`. Then push, and only then wait for CI. A CI run on a head with a conflict does not count.
2. **Green means:** the check `ci-ok` succeeded on the current head (it combines `test`, `image` and `ui`; jobs that are not needed are skipped), and `mergeable_state` is `clean`.
3. **Merge** with a merge commit (`merge_method: merge`), as with all previous PRs, and with `expectedHeadSha` on the checked head.
4. **Review request:** For PRs that Faun started, Faun is the author. A review request to Faun therefore fails. The assignment is enough.
