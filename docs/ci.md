# CI and Actions minutes

The repo is private; GitHub Free includes 3,000 Actions minutes a month. In
the first two days about 2,500 of them were used, so since #151 the CI only
runs what a change needs (spec #146).

## What costs minutes

- Every job rounds up to a full minute; `changes` and `ci-ok` also cost one
  each.
- Windows runners count double. So the Windows job only runs by hand
  (`windows.yml`); the CI checks the PC service's Windows code under Linux.
- The Actions cache holds 10 GB and then evicts the oldest entries.
  A pull request's caches are visible only to that pull request, those of `main`
  to all. So only `main` writes caches (rust-cache `save-if`, Docker
  `cache-to` only in the `publish` and `publish-bench` jobs).
- rust-cache holds the dependencies; its key includes Cargo.lock, the
  manifests and the rustc version. On `main` the `test` job only checks
  whether the cache is there, and only runs in full to refill it.

## Test runner and disk

The `test` job installs cargo-nextest as a prebuilt binary with
`taiki-e/install-action`, at the version `.config/nextest.toml` pins (a few
seconds, no compile). Each crate's integration tests are one binary
(`tests/main.rs`): one per test file linked the whole proxy 25 times, about
10 GB, and the job had to delete preinstalled SDKs first to fit on the runner.
The last step still prints the free disk space.

## What a run needs

Which jobs a pull request starts is decided by `scripts/ci-select.sh` from
the changed paths; `scripts/ci-select.test.sh` checks the mapping against
the repo. The `changes` job also runs `scripts/check-language.sh` on every
run, docs-only pull requests included; it takes seconds and adds no job.
Reference values after #151:

| Run | Jobs | Minutes |
|---|---|---|
| PR with a code change | changes, test, ci-ok | approx. 6–10 |
| PR that changes the web UI, the proxy or the protocol | as above, plus ui | approx. 5–10; 18 when the Ubuntu mirror is slow (#321) |
| PR that changes only `tools/local-agent/` or the code-bench runner (not its `tasks/`) | changes, test without Cargo (`cargo=false`, `check.sh --no-cargo`), ci-ok | approx. 1–2 (estimate) |
| PR that changes a workflow, Cargo.lock or scripts/ | everything except Windows | approx. 5–18 (#309: 5, #308: 15) |
| Push to `main` | changes, test (check only), publish and publish-bench if their image changed, ci-ok | approx. 1–10 (estimate without a publish) |

The `ui` job installs Chromium's system packages with Playwright's
`--with-deps` (fonts, xvfb, libnss3) from the runner's Ubuntu mirror. That
usually takes 20 seconds; on #321 the mirror sent 29 kB/s and the step took
13 minutes, so a `ui` job still running after 10 minutes is worth a look at
that step before you wait longer.

The code-bench image's build also builds its repo task: it vendors the
crates, prebuilds and runs the PC service's tests three times (the checkout,
the real commit, the checkout with the commit's tests), about 2 minutes
outside the cache. `tools/code-bench/prepare_repo_task.py` and a task's
`repo.json` select it (`bench=true`, and `publish-bench=true` on `main`).

The `changes` job also checks a PR's commit messages for Claude session
links (`scripts/pr-text.sh`), at no extra job. The body is checked by the
workflow `PR text` (`pr-text.yml`, check run `session-links`) when the PR opens and on each edit: one
short job (1 minute), so fixing the body does not start the whole CI.

## What gets published

- `publish` pushes the proxy's image and `publish-bench` the code benchmark
  runner's image to ghcr.io, each as `:latest` and `:<commit>`. The NAS and
  `deploy/truenas-auto-update.sh` follow `:latest`.
- Each job only runs when the files that go into its image changed since the
  commit its `:latest` was built from (the `org.opencontainers.image.revision`
  label, read by `scripts/ci-publish.sh`). The lists are in
  `scripts/ci-select.sh` (`publish=`, `publish-bench=`). Test-only files
  (`crates/*/tests`, the PC service's fixtures and example config) publish
  nothing, and `.dockerignore` keeps them out of the image. A change to
  `ci.yml` publishes both, and so does a manual run of the workflow on `main`,
  e.g. to pick up a newer base image.
- Each job runs once at a time and ends after 30 minutes. A newer push to
  `main` cancels a publish that is still running: only the newest commit's
  image is published. `:latest` only moves once an image is pushed in full,
  and the next push catches up on what a cancelled or failed one missed. A
  commit whose publish was cancelled gets no `:<commit>` tag, and its run
  on `main` is listed as cancelled although its other jobs passed.
- The version is the crate version from each `Cargo.toml`. The web UI shows
  the proxy's under *System* (`crates/nas-proxy/src/admin.rs`). The PC service
  reports its own without the commit, so a new image offers a PC service
  update only after a version bump (`docs/pc-service.md`).
- There are no git tags or GitHub releases.

## Where the minutes go

The fastest source is the usage report under *Settings → Billing →
Usage* (export as CSV): minutes per repo, workflow and runner OS.
From the runs themselves it can only be reconstructed via the job times of each
run, one call per run.
