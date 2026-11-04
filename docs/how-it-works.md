# How it works

All apps talk only to the proxy on the NAS. The proxy wakes the PC when a
request needs it and sends a sleep request to the PC service after the idle
timeout. In gaming mode neither happens.

## Requests

- **Forwarding:** The proxy forwards every path to Strata, in OpenAI and
  Anthropic format, and streams answers through live.
- **API keys:** Each app needs its own key from the web UI (*Access* area),
  sent as `Authorization: Bearer ...` or `x-api-key`. Without a valid key the
  app gets `401` with `x-strata-proxy-status: unauthorized` right away, and
  the PC is not woken for it. As long as no key exists, every app gets `401`.
- **Strata key:** Strata never sees an app's key. The proxy sends the Strata
  key instead, which only the proxy and the PC service know. The PC service
  makes a new one at every start of Strata, and the proxy fetches it.
- **Streamed requests** get a response right away and a ping event every
  15 s, even while Strata computes silently for a long time (Claude Code
  gives up after 300 s without data). Anthropic format: `event: ping`, OpenAI
  format: an SSE comment.

## Waking the PC

- When Strata does not answer, the proxy sends the first wake packet at once
  and repeats it every 10 s, waits until Strata answers on `/health`, then
  forwards the request. Several requests at the same time share one wake-up,
  and the wake-up goes on when the app hangs up meanwhile.
- If the PC does not wake within the wake timeout (240 s), the app gets `504`
  with `wake_timeout`.
- **MAC address:** The PC service reports the PC's MAC address itself; the
  proxy remembers it in `/data/pc-mac` and wakes with it. `PC_MAC` is only
  needed before the PC service has ever answered.

## Sleeping

- **Idle timeout:** After 15 min without a request and without an open
  answer, the proxy sends a sleep request to the PC service. If the PC service
  refuses (someone used mouse or keyboard in the last 10 min, Strata is
  computing, gaming mode is on), the proxy asks again after 1 min.
- **Lost answers:** If Strata's metrics show no running or waiting request for
  5 min, the proxy gives up the open ones with an error to the app, so that a
  lost answer does not keep the PC awake.
- **Keep awake** in the web UI holds the PC awake for 1 h, 4 h, 8 h or until
  8 am tomorrow.

## Gaming mode

Gaming mode is switched in the web UI or with the desktop shortcut the PC
service creates. While it is on, Strata is stopped, the PC never sleeps on a
sleep request, and every request gets `503` with
`x-strata-proxy-status: gaming` right away. Scripts such as the skill
`local-agent` read this header to postpone a run.

## List of models

While Strata is not ready (PC asleep or waking, gaming mode, a tune, a run
over all models), `GET /v1/models` and `GET /models` get Strata's last list
right away, with `x-strata-proxy-status: models_saved`, and do not wake the
PC. Apps that ask for the list first then send their real request. Without a
saved list they wait for the PC.

## Public model name

With *Hide model name* on (*Strata* area, tab *Connection*, setting
`HIDE_MODEL`), apps see one model name (`PUBLIC_MODEL_NAME`, default
`strata`) instead of the model Strata runs. The list of models has one chat
model under that name, every answer names it (streamed ones too), and a
request with any model name reaches the running model. Switching models on
the PC then never breaks an app. The image model keeps its name, and Strata's
error messages pass unchanged. Each API key follows this setting or overrides
it (*Hide* or *Show*, *Access* area).

## Status header

When the proxy answers instead of Strata, the header `x-strata-proxy-status`
says why:

| Value | Status | Meaning |
| --- | --- | --- |
| `unauthorized` | 401 | The app's API key is missing or wrong. |
| `strata_unauthorized` | 401 | Strata refused the proxy's Strata key. |
| `gaming` | 503 | Gaming mode is on. |
| `tuning` | 503 | Strata is tuning a model. |
| `not_configured` | 503 | No Strata address is set. |
| `no_mac` | 503 | The PC's MAC address is not known, so it cannot be woken. |
| `wake_timeout` | 504 | The PC did not wake in time. |
| `models_saved` | 200 | The saved list of models, answered while Strata is not ready. |

What to do about each one: [setup guide, Troubleshooting](setup.md#troubleshooting).

## History

Wake and sleep events and per-minute readings of the power plug are stored in
`/data/events.db` (SQLite). The web UI shows from them how long the PC was
awake, in gaming mode, asleep or unobserved, per day, week or month, with an
energy estimate.

## Prometheus

`/metrics` answers with the PC's state, power, requests, tokens and energy,
plus Strata's own metrics while it is up, with any API key. A scrape never
wakes the PC: [metrics](metrics.md).

## Which Strata endpoints the proxy uses

[Strata API](strata-api.md).
