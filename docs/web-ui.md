# Web UI

The web UI is at `http://<NAS-IP>:8080/admin/`, on the same port as the API.
Looking at it never wakes the PC and never keeps it awake; only buttons such as
*Wake PC* do. It has five areas:
*Overview*, *PC*, *Strata* (tabs *Models*, *Output*, *Version & benchmark*,
*Connection*, *Requests*), *Access* and *System*. Two badges at the top show
the state of the PC and of Strata live.

## Login

- **First login:** password `admin`, or the value of `UI_PASSWORD` if set.
  Then you must set your own password (at least 8 characters); nothing else
  works before that. It is stored as a hash in `/data/admin-password.json`.
- **Staying logged in:** 30 days, also across a restart. Changing the
  password logs out all other devices.
- **Forgot password:** delete `/data/admin-password.json` and restart the
  container. Then the default password applies again, all old logins are
  invalid and a new password is required at the first login.
- After 5 failed attempts from the same address, login is blocked for 1 min.
  `/admin/` is reachable wherever the API is reachable; the password is the
  only protection.

## Overview

Shows live whether the PC is asleep, waking up, awake or in gaming mode,
whether Strata is ready, loading, handling requests (with waiting requests and
tokens/s) or not reachable, the last request, a countdown to sleep (or why the
PC will not sleep) and the last ten PC events, plus the last 7 days of the
history.

As long as something is missing (Strata address, MAC address, PC service, API
key, `DATA_DIR`), the checklist *Finish setup* is shown at the top, with links
to the matching area.

While the web UI is open, the proxy asks the PC service, `/health` and
Strata's metrics every 5 s. This does not wake the PC and does not keep it
awake.

**Buttons:**

- *Wake PC*: the idle timeout counts from the wake-up.
- *Sleep now*: if the PC service refuses, the reason is shown, with *Force
  sleep* to put the PC to sleep anyway ([PC service](pc-service.md)).
- *Gaming mode*: wakes a sleeping PC first; after *off* the idle timeout
  starts again. If a request is running, a dialog asks first.
- *Keep awake* for 1 h, 4 h, 8 h or until 8 am tomorrow. It survives a restart
  (`/data/keep-awake.json`). "8 am tomorrow" follows the time zone in `TZ`
  (for example `Europe/Berlin`), or UTC without `TZ`.

Every button press ends up in the history as "from the web UI".

## PC

- **Connection, waking and sleeping:** the settings for the PC, such as the
  PC service address, the MAC address, the wake address, the power plug address
  and the idle timeout.
- **PC service:** *Create install command* shows a PowerShell command (with
  *Copy*) that downloads the PC service from the NAS, installs it and
  registers it with the proxy; the proxy then fills in the token and
  `PC_AGENT_URL` itself. The code in the command is valid once, for 15 min.
  The .exe is in the image and can be fetched without login at
  `/admin/agent/strata-pc-agent.exe`.
- **PC service version:** shown even while the PC is asleep (the last one
  reported). When the image contains a newer one, *Update to ...* appears
  (*Wake PC and update to ...* while the PC sleeps). If the update fails, the
  reason is shown there. The *Overview* only points to the update. Details:
  [PC service](pc-service.md).
- **History:** how long the PC was awake, in gaming mode, asleep or
  unobserved, per day, week or month, with an energy estimate.

## Strata

- **Models:** *Set up Strata* installs the Strata Terminal and the engine on
  the PC. The page then offers the models of Strata's own list, with Strata's
  recommendation for this PC preselected; *Load model* downloads one and
  starts Strata with it.
- **Output:** what Strata prints.
- **Version & benchmark:**
  - *Run benchmark* measures the active model, *Compare models* each
    installed one in turn (about 10 min each, requests wait that long) and
    then activates the previously active one again. The list shows decode and
    prompt tokens/s per model, with the context, speed projection and Strata
    version it was measured with, the fastest first, with *Make active*.
  - The **code benchmark**: the active model solves three Python tasks with
    opencode in the container `code-bench` next to the proxy (`CODE_BENCH_URL`
    points to it). Hidden tests grade the result; shown are tests, time,
    steps, tokens and tokens/s per task, expandable with code and test output.
    Details: [code benchmark](../tools/code-bench/README.md).
- **Connection:** Strata address, code benchmark address, relay mode and *Hide model name*
  ([public model name](how-it-works.md#public-model-name)).
- **Requests:** the request log, live. Each request through the proxy that
  made the model work, newest first, with the API key's name, the model, the
  API, its type (what started it and what came back, for example *tool result
  -> tool call*), prompt tokens with the cached share, output tokens, prompt
  reading and generation speed, time to first token, duration, the wait for
  the PC and the outcome. Only numbers, never the text; the last 1000
  requests are kept in `/data/events.db`. On a phone a row opens on tap.
  Above the list are a filter by API key, the average speeds per model (total
  tokens over total time) and two charts: generation speed over time, and
  generation speed against prompt size.

## Access

Create one API key per app. A key is shown in full only right after creation
and stored only as a hash in `/data/api-keys.json`. The list shows each key's
beginning, creation and last use; a revoked key stops working immediately.

Each key says whether its apps see the model's real name: as set in *Strata*,
tab *Connection* (the default), *Hide* or *Show*. It is chosen when creating
the key and can be changed in the list.

## System

The proxy's own settings (its address, body size limit, ping interval), the
energy settings for the history (power awake and asleep, price per kWh) and
the product version.

## Settings

Every value from [deploy/env.example](../deploy/env.example) except
`DATA_DIR`, `TZ` and `UI_PASSWORD` can be changed in its area and applies as soon as the section is
saved; only the proxy's own address (`LISTEN_ADDR`) needs a restart.

The value from the page applies, otherwise the environment variable,
otherwise the built-in default. *Reset* next to a field discards the value
from the page. The MAC address is the exception: one reported by the PC
service always applies. All settings with their defaults:
[crates/nas-proxy/src/config.rs](../crates/nas-proxy/src/config.rs).

Settings are stored in `/data/settings.json`. Without `DATA_DIR`, the *System*
area and the checklist warn that changes are lost on restart. The PC
service's token is never shown.
