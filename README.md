# Strata Home Proxy

The Strata PC at home goes to standby when no requests come in, and wakes up
via Wake on LAN as soon as one arrives. In gaming mode neither happens.
Background and decisions are in the handoff in the project.

All clients talk only to the NAS. The proxy there wakes the PC when needed
and asks it to go to sleep after the idle timeout.

## Status

| Step | Status |
| --- | --- |
| 1. One-time setup | Guide: [docs/setup.md](docs/setup.md) |
| 2. Proxy | built, tested against simulated Strata |
| 3. PC service and gaming switch | built, tested against simulated PC; setup via command from the *PC* area: [docs/pc-service.md](docs/pc-service.md) |
| 4. Web UI | built (English, areas Overview, PC, Strata, Access, System), with history and energy estimate |
| 5. Acceptance on real hardware | done: in daily use on the NAS and the PC |

## What the proxy does

- Forwards all paths to Strata (OpenAI and Anthropic format), responses
  are streamed through live.
- **API keys:** Each client needs its own key from the web UI
  (*Access* area), as `Authorization: Bearer …` or `x-api-key`. Without a
  valid key it gets `401` with `x-strata-proxy-status: unauthorized` right away;
  the PC is not woken for it. Strata never gets the client's key, but the
  Strata key, which only the proxy and the PC service know. The PC service
  makes a new one at every start of Strata, and the proxy fetches it.
- **PC asleep:** Send the wake packet (repeated every 10 s), wait until Strata
  answers on `/health`, then forward. Several simultaneous requests
  share one wake-up.
- **Streamed requests** get a response right away and a ping event every
  15 s, even while Strata computes silently for a long time (Claude Code aborts
  after 300 s without data). Anthropic: `event: ping`, OpenAI: SSE comment.
- **Gaming mode:** `503` right away with header `x-strata-proxy-status: gaming`.
  If the PC does not wake up in time: `504` with `wake_timeout`. From this the
  skill `local-agent` can postpone a run.
- **Not set up:** Without a Strata address the proxy still starts and
  answers with `503` and `not_configured`; without a MAC address waking
  fails with `503` and `no_mac`.
- **Idle:** 15 min without a request and without an open response, then a sleep
  request to the PC service. If it refuses, the proxy asks again after 1 min.
  If Strata metrics show no running or waiting request for 5 min,
  the proxy gives up the open ones (error to the client), so that a lost
  response does not keep the PC awake.
- **MAC address:** The PC service reports it itself; the proxy remembers it in
  `/data/pc-mac` and wakes with it. `PC_MAC` is only needed for the very first
  start, before the PC service has ever answered.
- **History:** Wake and sleep events and per-minute readings of the
  power plug in `/data/events.db` (SQLite). Under *PC → History* the
  web UI shows how long the PC was awake, in gaming mode, asleep or
  unobserved, per day, week or month, with energy estimate; the
  Overview shows the last 7 days. An existing `events.jsonl` is imported once
  on first start and not written afterwards; it
  is kept as a backup.

## Starting on the NAS

**TrueNAS SCALE:** [deploy/truenas-app.yaml](deploy/truenas-app.yaml) as a custom app via *Install via YAML*. Instructions are at the top of the file.

**Other NAS with Docker Compose:**

CI builds the image on every push to main: `ghcr.io/faunkr/strata-home-proxy:latest`.

The repo is private, so `curl` downloads files from it only with a
GitHub token (fine-grained, *Contents: Read-only* for this repo):

```sh
mkdir -p strata-proxy && cd strata-proxy
read -rs TOKEN           # paste token, Enter
gh_file() { curl -fsSL -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github.raw" \
  "https://api.github.com/repos/FaunKr/Strata-Home-Proxy/contents/$1"; }
gh_file deploy/docker-compose.yml >docker-compose.yml
gh_file deploy/env.example >.env
unset TOKEN
nano .env                # fill in values
docker compose pull
docker compose up -d
docker compose logs -f
```

The container needs `network_mode: host`, otherwise the wake packet does not
reach the home network. All settings are listed with explanations in
[deploy/env.example](deploy/env.example) and
[crates/nas-proxy/src/config.rs](crates/nas-proxy/src/config.rs).

Send just a wake packet, for testing: `docker compose run --rm strata-proxy wake`

The second service `code-bench` (image `ghcr.io/faunkr/strata-code-bench:latest`)
is only there for the code benchmark; without it everything else runs the same. The
update script for TrueNAS does not update it; for that, run
`docker pull ghcr.io/faunkr/strata-code-bench:latest` once and restart the app.

## Automatic updates (TrueNAS)

[deploy/truenas-auto-update.sh](deploy/truenas-auto-update.sh) pulls the image every 15
minutes. If it is new, the script backs up `/data`, redeploys the app via
the TrueNAS middleware and waits until `/admin/` answers. If that does not work
within 2 minutes, it restores the backup, starts the
old image and marks the new one as faulty; only a newer image is
tried again. Then it pulls the image of the code benchmark (service
`code-bench`) and redeploys the app if it is new; without backup and
rollback, since the service has no data and the proxy runs without it.

Prerequisites: the app is called `strata-proxy` and is installed from
[deploy/truenas-app.yaml](deploy/truenas-app.yaml), with
`pull_policy: missing` (otherwise the rollback pulls the faulty image
again), and root is logged in to ghcr.io (`sudo docker login ghcr.io`, as
at the top of the YAML file).

1. Put the script in its own directory on the pool, not in the
   app's data directory. The log and backups also end up there.
   The repo is private, so with a GitHub token (fine-grained,
   *Contents: Read-only* for this repo):
   ```sh
   sudo mkdir -p /mnt/POOL/apps/strata-proxy-update
   cd /mnt/POOL/apps/strata-proxy-update
   read -rs TOKEN   # paste token, Enter
   sudo curl -fsSL -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github.raw" \
     -o truenas-auto-update.sh \
     https://api.github.com/repos/FaunKr/Strata-Home-Proxy/contents/deploy/truenas-auto-update.sh
   unset TOKEN
   sudo chmod 700 truenas-auto-update.sh
   sudo ./truenas-auto-update.sh   # once by hand: without a new image nothing happens
   ```
2. *System → Advanced Settings → Cron Jobs → Add*:
   - Command: `/mnt/POOL/apps/strata-proxy-update/truenas-auto-update.sh`
   - Run As User: `root`
   - Schedule: Custom, `*/15 * * * *`
   - *Hide Standard Output* on, *Hide Standard Error* off
3. Optional: write a Home Assistant webhook URL into the file `auto-update.webhook`
   next to the script. On a failed update,
   `{"event":"nas_update_failed","data":{"app":"strata-proxy","from":…,"to":…,"rolled_back":true}}` goes there.

A different app name, a different `LISTEN_ADDR` or a different path: as variables before the command in the cron job,
e.g. `STRATA_PROXY_APP_NAME=proxy STRATA_PROXY_HEALTH_URL=http://127.0.0.1:9000/admin/ /mnt/…/truenas-auto-update.sh`.
All variables are listed at the top of the script. It reads the data directory from the
container (mount on `/data`).

Next to the script are:

- `auto-update.log`: every update and every error (rotated into
  `auto-update.log.1` at 200 KB).
- `backups/data-*.tar.gz`: the last 10 backups of `/data`, with
  password hash and API keys, so readable only by root.
- `auto-update.bad-images`: images that did not come up. Delete a line
  so that the image is tried once more.
- `failed-data-*`: the data as the faulty image left it.

If the log says `ROLLBACK FAILED`, roll back by hand: stop the app under *Apps*, replace the
contents of the data directory with the latest backup
(`sudo tar -C /mnt/POOL/apps/strata-proxy --numeric-owner -xzpf backups/data-….tar.gz`),
`sudo docker tag ghcr.io/faunkr/strata-home-proxy:previous ghcr.io/faunkr/strata-home-proxy:latest`,
start the app.

## Web UI

At `http://<NAS-IP>:8080/admin/`, so on the same port as the API.
Calls there do not wake the PC and do not keep it awake. The web UI
is in English and split into five areas: *Overview*, *PC*, *Strata*
(tabs *Models*, *Output*, *Version & benchmark*, *Connection*), *Access* and
*System*; at the top, two badges show the state of PC and Strata live.

- **Overview:** shows live whether the PC is asleep, waking up, awake or in
  gaming mode, whether Strata is ready, loading, handling requests (with
  waiting requests and tokens/s) or offline, the last request, a
  countdown to sleep (or why it will not sleep) and the last
  ten PC events. As long as something is still missing (Strata address, MAC,
  PC service, API key, `DATA_DIR`), the checklist *Finish setup* is shown at the top
  with links to the matching area. As long as the web UI is open, the proxy polls
  the PC service, `/health` and Strata's metrics every 5 s; this
  does not wake the PC and does not keep it awake. For the metrics the
  proxy needs the Strata key, which it fetches from the PC service. Which Strata endpoints
  the proxy uses: [docs/strata-api.md](docs/strata-api.md).
- **Buttons:** *Wake PC* (the 15 min idle timeout counts from the wake-up),
  *Sleep now* (if the PC service refuses, the reason is shown), the switch
  *Gaming mode* (wakes a sleeping PC first; after *off* the
  idle timeout restarts) and *Keep awake* for 1 h, 4 h, 8 h or until 8 am tomorrow.
  Keep awake survives a restart (`/data/keep-awake.json`). "8 am
  tomorrow" follows the time zone in `TZ` (e.g. `Europe/Berlin`), or UTC
  without `TZ`. If a request is running, *Gaming mode* asks first in
  a dialog. Every button ends up in the history with "from the web UI".
- **PC service version:** shown in the *PC* area, even when the PC is asleep
  (the last one reported). If the image contains a different one,
  *Update to …* appears; the button works only when the PC is awake and never wakes it. If
  the update fails, the reason is shown there. The Overview only points to the update.
- **First login:** password `admin`, or the value of `UI_PASSWORD`, if
  set. After that you must set your own password (at least 8 characters);
  nothing else works before that. It is stored as a hash in
  `/data/admin-password.json`.
- **Staying logged in:** 30 days, also across a restart. Changing the password
  logs out all other devices.
- **Forgot password:** delete `/data/admin-password.json` and restart the
  container, then the default password applies again.
- **Settings:** Every value from [deploy/env.example](deploy/env.example)
  except `DATA_DIR` can be changed in its area (*PC*: connection,
  waking and sleeping; *Strata → Connection*: address and key; *System*: the
  rest) and applies as soon as the section is saved, only the address of the
  proxy (`LISTEN_ADDR`) not until a restart. The value from the
  page applies, otherwise the environment variable, otherwise the default; *Reset* discards
  the value from the page. For the MAC, the one reported by the PC service takes precedence.
  It is stored in `/data/settings.json`; without `DATA_DIR`, *System* and the checklist warn
  that changes are lost on restart. The PC service's token is
  never shown.
- **PC service:** *Create install command* in the *PC* area shows a PowerShell command (with *Copy*) that
  downloads the PC service from the NAS, installs it and registers it with the proxy; the proxy then
  fills in the token and `PC_AGENT_URL` itself. The code in it is valid once,
  for 15 min. The .exe is in the image and can be fetched without login at
  `/admin/agent/strata-pc-agent.exe`. Details:
  [docs/pc-service.md](docs/pc-service.md).
- **Benchmark** (*Strata → Version & benchmark*): *Run benchmark* measures the
  active model, *Compare models* each installed one in turn (about
  10 min each, requests wait that long) and then activates the previously
  active one again. The list shows decode and prompt tokens/s per model, with
  which context, which speed projection and which Strata version
  it was measured, the fastest first, with *Make active*.
- **Code benchmark** (same tab): The active model solves
  three Python tasks with opencode in the container `code-bench` next to the proxy (in both
  Compose files; `CODE_BENCH_URL` points to it). Hidden tests grade
  the result; shown are tests, time, steps, tokens and tokens/s per
  task, expandable with code and test output. Details:
  [tools/code-bench/README.md](tools/code-bench/README.md).
- **API keys** (*Access* area): Create one key per client. It is shown in full only right after
  creation and stored only as a hash in `/data/api-keys.json`.
  The list shows the beginning, creation and last use; a
  revoked key stops working immediately. As long as no key exists, every
  client gets `401`.
- After 5 failed attempts from the same address, login is blocked for 1 min.
  `/admin/` is reachable wherever the API is reachable; the password is
  the only protection.

## Development

```sh
cargo nextest run  # starts a simulated Strata, PC service and WoL receiver
scripts/check.sh   # all CI checks: fmt, clippy (also wasm), tests
```

The web UI (Leptos, [crates/admin-ui](crates/admin-ui)) needs, for the
browser part, `rustup target add wasm32-unknown-unknown` and
`cargo install cargo-leptos --locked`. Then `cargo leptos build` builds the proxy and
the web UI, `cargo leptos watch` starts both and rebuilds on changes.

## License

MIT, see [LICENSE](LICENSE).
