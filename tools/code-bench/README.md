# Code benchmark

The **Run code benchmark** button under Strata → Version & benchmark has the
active model solve three fixed programming tasks. For this a second container,
`code-bench`, runs on the NAS next to the proxy:

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
  forwards only `/v1` to the proxy (`PROXY_URL`), not its web UI, plus
  Docker's DNS and the internet; blocked are the proxy directly, loopback (the
  runner's API), private, link-local and CGNAT networks, i.e. the NAS and the
  home network, and IPv6. For this the container needs `NET_ADMIN`; without it
  the runner refuses runs and the card names the reason.
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

Tests: `python3 tools/code-bench/test_runner.py` (with a faked opencode;
`scripts/check.sh` runs them).

## Threat model

What opencode, the model's commands and the code in the task folders do is
untrusted code. It does not reach:

| What | Protected by |
| --- | --- |
| the runner's API (cancelling or starting runs) | user `agent`, the firewall blocks loopback except the relay and port 53 of Docker's DNS (127.0.0.11) |
| the NAS, the home network, the proxy's web UI | the firewall blocks private, link-local and CGNAT networks and IPv6; the relay forwards only `/v1/*` |
| keys other than the run's | the proxy creates one key per run and revokes only that one |
| files of the runner and of other tasks | user `agent` owns only its own task folder |
| the scoring | hidden tests only after opencode, `python -I`, a process group of their own; writing without following symlinks (`O_NOFOLLOW`); marker only from stdout |
| processes outliving the task | opencode's process group ends with opencode, the tests' after scoring |

Known gap: the code under test runs in the test run's process and can print a
marker itself on exit. That cannot be prevented in this process; the score is
a measure, not a check against cheating.
