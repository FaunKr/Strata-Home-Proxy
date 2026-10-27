# Forced sleep and forced Strata restart in the PC service

This folder is a checkout of the Strata Home Proxy repository (Rust). Add two
endpoints to the PC service (`crates/pc-agent`) and their answer types to the
protocol crate (`crates/pc-protocol`). Read `CLAUDE.md`, `GLOSSARY.md` and
`docs/pc-agent-api.md` first.

## Goal

The web UI needs two buttons for a stuck PC: *Force sleep* and *Force restart
Strata*. The PC service gets the endpoints behind them. Both take the same
bearer token as every other endpoint and an empty JSON body (`{}`).

### `POST /sleep/force`

- Puts the PC to standby whatever stands against a normal `POST /sleep`:
  user activity, a busy Strata, gaming mode.
- The only refusal: while the PC service updates itself, 409 with the
  refusal body `{"reason": "The PC service is updating"}`.
- A running Strata job is cancelled first. Downloaded parts stay, so the same
  job started again continues.
- The answer comes first; standby follows after the same delay as after an
  accepted `POST /sleep`, and only once a cancelled job has ended. Everything
  that happens around a normal standby happens here too, e.g.
  `fresh_strata_after_standby`.
- Gaming mode stays on: after the PC wakes it is still on and Strata stays off.
- The answer, `200`, says what it overrode:
  `{"user_active": bool, "strata_busy": bool, "gaming_mode": bool, "cancelled_job": "<kind>"}`.
  `cancelled_job` is the cancelled job's kind as the protocol names job kinds
  (e.g. `"install"`), and is left out when no job ran.

### `POST /strata/kill`

- Refused in gaming mode (409, `{"reason": "Gaming mode is on"}`) and while
  the PC service updates (409, `{"reason": "The PC service is updating"}`).
- Cancels a running Strata job and waits until it has ended. Then it kills
  Strata hard: the configured programs, also a Strata someone started by
  hand, also one that hangs and no longer answers.
- Then it starts Strata again, if the service starts Strata at all: it has a
  start script and `auto_start` is on. Otherwise Strata stays stopped.
- The answer, `200`, comes after all that:
  `{"cancelled_job": "<kind>", "started": bool}`; `cancelled_job` is left
  out when no job ran.
- The watchdog leaves the new Strata alone while it loads, as after any start.

### `GET /status`

While a freshly started Strata loads and does not answer yet, `/status`
reports `strata_running: true`, so the web UI shows it loading after a
forced restart.

### Protocol types

In `crates/pc-protocol`, next to the other `/status` and `/sleep` types:
`ForcedSleep` (fields `user_active`, `strata_busy`, `gaming_mode`,
`cancelled_job: Option<StrataJobKind>`) and `ForcedRestart` (fields
`cancelled_job: Option<StrataJobKind>`, `started`), both exported from the
crate root. Refusals use the existing refusal type. Add samples of both
answers and both refusals under `crates/pc-protocol/tests/samples/`
(`sleep-force.json`, `sleep-force-refused.json`, `strata-kill.json`,
`strata-kill-refused.json`) and describe both endpoints in
`docs/pc-agent-api.md`.

## Files involved

- `crates/pc-agent/src/agent.rs`: the router, `/sleep`, `/status`, the watchdog.
- `crates/pc-agent/src/strata_process.rs`, `crates/pc-agent/src/job.rs`:
  stopping and starting Strata, running and cancelling jobs.
- `crates/pc-protocol/src/status.rs`, `crates/pc-protocol/src/lib.rs`.
- Tests to follow: `crates/pc-agent/tests/agent.rs`, `strata.rs`,
  `update.rs`, `strata_models.rs` (the samples test), and
  `crates/pc-protocol/tests/samples.rs`. They use a faked platform and Strata
  (`crates/pc-agent/tests/common`).

## How to check

```sh
cargo test --offline -p strata-pc-agent -p strata-pc-protocol
```

There is no network; all crates are vendored. The tests that score this task
are the real ones of the feature and replace the test files named above, so
keep the names above exact.

All tests must pass at the end: the ones that were in the repo before you
started as well as the ones you add. Run the whole command above, not only
your new tests, and fix every earlier test you broke.

## Done

Both endpoints work as described, `/status` reports a loading Strata as
running, the protocol types and samples exist, and the command above passes
with no failing test, old or new.
