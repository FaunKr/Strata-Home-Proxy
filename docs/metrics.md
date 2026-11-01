# Metrics endpoint

The proxy answers `GET /metrics` and `GET /metrics/prometheus` itself, in Prometheus' text format (`text/plain; version=0.0.4`), whatever the query. Neither path reaches Strata any more. A scrape:

- needs one of the proxy's API keys, as a Bearer token or in `x-api-key`; without a valid one it gets `401` with `x-strata-proxy-status: unauthorized`;
- never wakes the PC, never counts as activity for the idle timeout, never waits for a wake-up and makes no row in the request log;
- answers `200` while the PC sleeps, Strata does not answer or is not set up: the proxy's own lines are always there.

Code: `crates/nas-proxy/src/metrics.rs`. Tests: `crates/nas-proxy/tests/metrics.rs`.

## The proxy's own metrics

| Name | Type | Labels | Present |
|---|---|---|---|
| `strata_proxy_info` | gauge, always 1 | `version`: the product version | always |
| `strata_proxy_pc_state` | gauge, 1 for the current state, 0 for the others | `state`: `unknown`, `asleep`, `waking`, `awake`, `gaming` | always |
| `strata_proxy_tuning` | gauge, 0 or 1 | none | always |
| `strata_proxy_strata_up` | gauge, 0 or 1 | `model`: the active model, while up and the PC service's last report marks one | always |
| `strata_proxy_power_watts` | gauge | none | with a power plug and a latest reading |
| `strata_proxy_requests_total` | counter | `key`, `model`, `outcome`: `answered`, `refused`, `failed` | per series with at least one request |
| `strata_proxy_prompt_tokens_total` | counter | `key`, `model` | per series with at least one request |
| `strata_proxy_cached_tokens_total` | counter | `key`, `model` | per series with at least one request |
| `strata_proxy_output_tokens_total` | counter | `key`, `model` | per series with at least one request |
| `strata_proxy_request_energy_joules_total` | counter | `model` | with a power plug, per model with request energy |
| `strata_proxy_pc_energy_joules_total` | counter | none | with a power plug |
| `strata_proxy_wake_packets_total` | counter | none | always |

- **PC state** is the state the proxy holds; a scrape does not ask the PC service. **Tuning** is 1 while the PC service reports a tune.
- **Strata up** is 0 while the PC is asleep or waking, during a tune and while Strata's address is missing; otherwise Strata answered `/health` in the last seconds or answers one now, with the probe timeout.
- **Power** is the plug loop's latest reading (`PLUG_SAMPLE_SECS`); a scrape never asks the plug itself.
- **Requests and tokens** come from the request totals: every finished row of the request log adds to its day, API key, model and outcome. `answered` is *done* and *cut off*; `refused` is every request the proxy answered itself; `failed` is Strata's error status, no answer, lost, and cancelled by the client. Requests the request log does not count (the list of models, `/health`) are not here either. The code benchmark's requests count under its key name, `Code benchmark`.
- **Model** is the PC service's name, as the Strata page lists it (e.g. `IQ3_S`), the one request energy counts for; the image model keeps Strata's name; a request refused before it reached Strata has no `model` label.
- **Request energy** is the request energy of all days per model, in joules (Wh × 3600), so it adds up to the same figures as the energy per 1000 tokens.
- **PC energy** counts the power plug's readings in the history: each interval of `PLUG_SAMPLE_SECS` with readings counts for that long at their mean. Only measured readings count, never the energy estimate from `POWER_AWAKE_WATTS` and `POWER_ASLEEP_WATTS`. It agrees with the history's *Energy* for every fully measured hour.
- **Wake packets** counts the wake packets in the history.

The counters carry on across restarts: they are read from the history's database. The request totals started empty with the release that brought them; the request energy, the PC energy and the wake packets include everything saved before. A deleted database, or a changed `PLUG_SAMPLE_SECS` for the PC energy, makes a counter drop, which Prometheus treats as a reset.

## Strata's lines

While Strata is up, its own lines from `/metrics?format=prometheus` (`vllm:*`, `strata:*`, `docs/strata-api.md`) follow the proxy's, unchanged. While it is not up, or does not answer, they are missing and the scrape still answers `200`. Strata's `/metrics/prometheus` (`llamacpp:*`) is never fetched: every read resets its time window.

## Keys that hide the model

For an API key that hides the model, every model name in the answer is the public model name: Strata's `model_name` label and the proxy's `model` label. The image model keeps its own name, as in answers. Series of the proxy that then share all labels add up into one line. Every key sees the counters of all keys by name.

## Scrape config

```yaml
scrape_configs:
  - job_name: strata-home-proxy
    scrape_interval: 30s
    static_configs:
      - targets: ["nas.local:8080"]
    authorization:
      type: Bearer
      credentials: "<an API key from Access>"
```
