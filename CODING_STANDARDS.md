# Coding Standards

Rules for review. What a test or Clippy already checks is not listed here.
Behaviour of Strata (endpoints, auth, metrics): `docs/strata-api.md`.
Questions already decided: `docs/adr/`.

## Web UI and login

- **Sessions end on the server.** Logging out, changing the password and deleting
  `admin-password.json` invalidate existing cookies, copied ones too.
  Deleting a cookie only in the browser is not enough.
- **Tokens are unique.** Two logins in the same second yield
  different tokens; a revocation ends only its own session.
- **Every password check counts towards the lockout**, including the check of the
  current password when changing it.
- **Errors when saving are reported.** A value that does not end up in `DATA_DIR`
  does not count as saved, and the page says so.
- **No stale state overwrites a newer one.** Whoever takes a copy under
  a lock and writes outside it holds a write lock while doing so, which
  every other write of the same file takes too.
- **What Leptos does while building the router happens once per process.**
  Generating the route list sets a process-wide switch that halts every
  resource started meanwhile (#69). New code that repeats this
  on every router build needs the same `OnceLock` as
  `server::router`.
- **A resource is not created inside a `Suspend`'s content.** If it is
  created there, the page does not hydrate ("expected a marker node"). The
  component creates it and shows it next to the `Suspend`, like
  `CodeBenchCard` next to `with_page`.
- **Server functions read context without panicking** (`backend()?`, `use_context`).
  The server renders the content of a `Transition` several times per request; the
  resources of the discarded passes keep running, without context. A
  panic there means the response never ends.

## Proxy and PC

- **`/admin` never wakes the PC** and does not reset the idle
  timeout. `no_server_function_wakes_the_pc` checks server functions on its own;
  a route of its own like `overview-stream` needs a test of its own.
- **Timers in tests with a margin.** If a test must see something before a
  timer expires (idle timeout, settling), the timer fires at least 2 s
  after it; the CI runner is slower than the session. Tight timers only
  when the test only waits afterwards.
- **Waits a test would have to sit through are configurable.** A limit
  or retry interval that behaviour depends on comes from `Config` (like
  `idle_timeout`) or through a setter for tests (like `Monitor::set_poll`,
  `UpdateSummaries::set_timing`), not from a fixed `const`. Otherwise the
  behaviour stays untested.
- **Precedence of settings:** MAC reported by the PC, then value from the
  page, then environment variable, then default. New settings follow
  the same order and are read live via `AppState::cfg()`. This
  applies to values from the web UI's settings sections, not to fixed paths in the image like
  `LEPTOS_SITE_ROOT` or `PC_AGENT_EXE`.
- **Strata jobs run only fixed programs.** The PC service takes
  values only from `catalog.rs` and checks model IDs against the catalog before they
  go into a path. The Strata key is in no response to the
  web UI and in no stored job.
- **Every way out of a Strata job leaves a state Strata
  ran with.** The review goes through every `?`, every cancellation check and every
  failed step: what is on disk afterwards, is Strata running?
  A rolled-back configuration comes back in `cleanup`, even after
  a cancellation, and Strata's last lines stay visible whether
  writing back works or not (`StrataProcess::restart_or_roll_back`).
- **Clients for services on the same machine set a short
  `connect_timeout`** (like `STRATA_CONNECT_TIMEOUT` in `pc-agent/src/agent.rs`).
  Windows retries a refused connection to localhost for about 2 s;
  without a limit every query to a stopped Strata waits that long.
- **When a field becomes optional, the review goes through every reader.** Whoever
  assumed a value so far needs a path for "missing" that fits the
  state (a missing start script does not mean "PC is asleep").

## Code benchmark (`tools/code-bench`)

- **Every change to the runner or to how the proxy calls it goes through
  the threat model** in `tools/code-bench/README.md`: does foreign code
  then reach anything from the table, or does one of the barriers fall away? A new
  access (port, path, user, mount) gets a row with its barrier.

## Install script (`deploy/windows/install.ps1`)

It runs in Windows PowerShell 5.1, not in PowerShell 7:

- **Write files for the PC service without a BOM**
  (`[IO.File]::WriteAllLines`). `Set-Content -Encoding UTF8` puts one in front.
- **Errors from the NAS come as a script with a message**, not as an error status:
  on an error status `irm` shows only "(403)" and no text.
- **What the one-liner needs, the NAS inserts into the script.** `irm … | iex`
  takes no parameters.
- **Download first, then stop the running service**, and check everything that can
  fail before the one-time code is used up.
