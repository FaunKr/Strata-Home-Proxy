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
  `cache-to` only in the `publish` job).
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
| PR that changes a workflow, Cargo.lock or scripts/ | everything except Windows | approx. 18 |
| Push to `main` | changes, test (check only), publish, ci-ok | approx. 5–10 |

The `publish` job never runs twice at the same time and ends after 30 minutes.
A newer push to `main` waits until the running one is finished; that way no
older commit resets `:latest`.

## What gets published

- `publish` pushes both images to ghcr.io as `:latest` and `:<commit>`. The
  NAS and `deploy/truenas-auto-update.sh` follow `:latest`.
- The version is the crate version from each `Cargo.toml`. The web UI shows
  the proxy's under *System* (`crates/nas-proxy/src/admin.rs`). The PC service
  reports its own with the build commit appended, e.g. `0.1.0+5e8b9f1`
  (`crates/pc-agent/build.rs`, from `STRATA_BUILD_ID`).
- There are no git tags or GitHub releases.

## Where the minutes go

The fastest source is the usage report under *Settings → Billing →
Usage* (export as CSV): minutes per repo, workflow and runner OS.
From the runs themselves it can only be reconstructed via the job times of each
run, one call per run.
