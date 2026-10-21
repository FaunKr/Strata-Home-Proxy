# Code benchmark

The **Run code benchmark** button under Strata → Version & benchmark has the
active model solve four fixed programming tasks: three small Python tasks and
a real feature of this repo in Rust (the *repo task*). **Benchmark all
models** does the same for each installed model in turn. For this a second
container, `code-bench`, runs on the NAS next to the proxy:

- `runner.py` accepts a run from the proxy (`POST /runs`), lets `opencode run`
  work on each task in a fresh folder and afterwards checks what is left with
  the hidden tests. The proxy polls `GET /runs/current` until the run ends.
- opencode talks through the relay to the proxy's OpenAI endpoint (`PROXY_URL`), with a
  key the proxy creates for the run and revokes afterwards. So the run wakes
  the PC like any client.
- opencode sends no sampling settings; Strata takes the set from the model card.
  With `temperature: 0` every answer would be greedy, and Qwen models get
  stuck in loops with that.
- The proxy embeds the tasks in `tasks/` and sends them with the run; the
  hidden tests are put into the folder only after opencode.
- The coding agent may read, write and run `bash`, but not `webfetch`.
  opencode and the hidden tests run as user `agent` in this container, not on
  the NAS. At start the runner (root) sets up a firewall for `agent` only:
  allowed are a relay in the runner (`RELAY_ADDR`, 127.0.0.1:8097) that
  forwards only `/v1` to the proxy (`PROXY_URL`), not its web UI, Docker's
  DNS, and loopback except the runner's API port, so the tests' own servers
  work. Everything else is rejected, the internet included: the repo is
  public, so with internet the repo task could be solved by fetching the real
  commit. For this the container needs `NET_ADMIN`; without it the runner
  refuses runs and the card names the reason.
- Before each run and on each request to `/health` the runner checks that it
  really is the proxy that answers under `PROXY_URL` (its login page, which
  never wakes the PC). If another service answers there, or none, it refuses
  runs and the card names the address and the answer.
- The prompt asks the model to save intermediate state: a first working
  version of each file early and again after each improvement, so a task
  stopped at the limit still leaves something to score, and its plan and
  progress in `NOTES.md`, so it finds its place again after opencode compacts
  the context. Only `.py` files go into the result, so `NOTES.md` does not.
  Results from before this prompt are not strictly comparable; a new run
  replaces a model's old result.

Measured per task: total time, steps of the model, input and output tokens,
output tokens per second (over the time of the steps) and passed tests. After
the limit per task opencode is stopped; what it has written is checked all
the same. The proxy sends the limit at the start (`timeout_secs`):
`CODE_BENCH_TASK_TIMEOUT_SECS` under Strata → Timeouts, otherwise a suggestion
from the slowest installed model in "Compare models".

## The repo task

`tasks/pc-force-endpoints` is the commit "PC service: forced sleep and forced
Strata restart endpoints" (`repo.json`). At image build `prepare_repo_task.py`
puts this repo as it was before the commit into
`/opt/code-bench/repos/pc-force-endpoints/checkout` as one fresh commit
without history, vendors the crates to `/opt/code-bench/vendor`, sets cargo
offline and prebuilds the tests; the Rust toolchain sits in `/opt/rust`. It
takes the commit's test files as the hidden tests and counts the tests the
commit added. The build fails unless the commit's own change passes all
counted tests and breaks none, and the untouched checkout passes fewer.

To check a repo task without Docker (a new one, or after changing
`prepare_repo_task.py`): `python3 tools/code-bench/prepare_repo_task.py
tools/code-bench/tasks/<name> . <empty folder>` in a clone that has the
commit. It runs the same checks as the image build, with the local Rust
toolchain, in about 2 minutes.

A run copies the checkout into the task folder; opencode gets only
`TASK.md`, which the proxy sends. Afterwards the runner records the diff
against the checkout (what the agent committed included), lays the hidden
test files over the folder and runs `cargo test`. The score is the counted
tests that pass; tests that passed on the checkout and fail now are listed as
broken. Code that does not build scores 0 with the compiler's output. cargo's
home is the task's own (`target/cargo-home`), so nothing reaches the next run.
The proxy gives this task twice the limit of the others.

## Benchmark all models

The proxy activates each installed model in turn (a Strata job, waking the PC
if needed), runs the four tasks with one key for the whole run, and activates
the model active before at the end or after a cancel. A model that fails
keeps its earlier result with the failure noted; without one it gets an error
row. While the run goes, requests with any other key wait until it ends, like
a request waiting for the PC to wake, with the same time limit and answer. A
single run holds no one, and a restarted proxy holds no one either.

Tests: `python3 tools/code-bench/test_runner.py` (with a faked opencode;
`scripts/check.sh` runs them).

## Threat model

What opencode, the model's commands and the code in the task folders do is
untrusted code. It does not reach:

| What | Protected by |
| --- | --- |
| the runner's API (cancelling or starting runs) | user `agent`, the firewall rejects the API port |
| the NAS, the home network, the internet, the proxy's web UI | the firewall rejects everything but the relay, Docker's DNS and loopback; the relay forwards only `/v1/*` |
| the repo task's real commit | no internet; the checkout has no history |
| the repo task's hidden tests and counted list | `hidden/` and `task.json` are root's only (700/600) |
| keys other than the run's | the proxy creates one key per run and revokes only that one |
| files of the runner and of other tasks | user `agent` owns only its own task folder |
| the scoring | hidden tests only after opencode, `python -I`, a process group of their own; writing without following symlinks (`O_NOFOLLOW`); marker only from stdout |
| processes outliving the task | opencode's process group ends with opencode, the tests' after scoring |

Known gap: the code under test runs in the test run's process and can print a
marker itself on exit. That cannot be prevented in this process; the score is
a measure, not a check against cheating. The same holds for the repo task,
more so: the agent controls the build (`build.rs`, `.cargo/config.toml`, a
`rustc-wrapper`) and could print passing test lines itself. The diff shows
such a change; the score does not catch it.
