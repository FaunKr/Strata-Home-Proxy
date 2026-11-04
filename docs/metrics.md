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
| `strata_proxy_gpu_load_ratio` | gauge, 0 to 1 | none | with hardware readings (below) |
| `strata_proxy_gpu_temperature_celsius` | gauge | none | with hardware readings |
| `strata_proxy_gpu_vram_used_bytes` | gauge | none | with hardware readings |
| `strata_proxy_gpu_vram_total_bytes` | gauge | none | with hardware readings |
| `strata_proxy_gpu_fan_rpm` | gauge, 0 while the fan stands still | none | with hardware readings |
| `strata_proxy_gpu_power_ratio` | gauge, share of the power limit, 0 to 1 | none | with hardware readings |
| `strata_proxy_gpu_power_watts` | gauge | none | with hardware readings, AMD cards with PC service 1.2.0 or newer |
| `strata_proxy_cpu_load_ratio` | gauge, 0 to 1 | none | with hardware readings |
| `strata_proxy_ram_used_bytes` | gauge | none | with hardware readings |
| `strata_proxy_ram_total_bytes` | gauge | none | with hardware readings |
| `strata_proxy_requests_total` | counter | `key`, `model`, `outcome`: `answered`, `refused`, `failed` | per series with at least one request |
| `strata_proxy_prompt_tokens_total` | counter | `key`, `model` | per series with at least one request |
| `strata_proxy_cached_tokens_total` | counter | `key`, `model` | per series with at least one request |
| `strata_proxy_output_tokens_total` | counter | `key`, `model` | per series with at least one request |
| `strata_proxy_expert_tokens_total` | counter | `model`, `tier`: `vram`, `pcie`, `cpu` | all three tiers per model with at least one counted Strata request, also while the PC sleeps |
| `strata_proxy_request_energy_joules_total` | counter | `model` | with a power plug, per model with request energy |
| `strata_proxy_pc_energy_joules_total` | counter | none | with a power plug |
| `strata_proxy_wake_packets_total` | counter | none | always |
| `strata_proxy_request_body_broken_total` | counter | `side`: `arrival`, `network`, `strata` | always, all three sides |

- **PC state** is the state the proxy holds; a scrape does not ask the PC service. **Tuning** is 1 while the PC service reports a tune.
- **Strata up** is 0 while the PC is asleep or waking, during a tune and while Strata's address is missing; otherwise Strata answered `/health` in the last seconds or answers one now, with the probe timeout.
- **Power** is the plug loop's latest reading (`PLUG_SAMPLE_SECS`); a scrape never asks the plug itself.
- **Hardware readings** are those of the PC service's last answer to `/status`, which the proxy asks on every idle check (`IDLE_CHECK_SECS`) and for the Overview; a scrape never asks the PC service. They are there while the PC is awake or in gaming mode and that answer came within the last two idle checks; otherwise there are no hardware lines. Each reading the PC service does not know is left out, never 0, and without a graphics card there are no GPU lines. The GPU is the one with the most VRAM. **GPU power in watts** is AMD's ASIC power, the GPU chip's draw from the Adrenalin driver (`docs/pc-agent-api.md`), not the whole card's; other cards have no watts line.
- **Requests and tokens** come from the request totals: every finished row of the request log adds to its day, API key, model and outcome. `answered` is *done* and *cut off*; `refused` is every request the proxy answered itself; `failed` is Strata's error status, no answer, lost, and cancelled by the client. Requests the request log does not count (the list of models, `/health`) are not here either. The code benchmark's requests count under its key name, `Code benchmark`.
- **Model** is the PC service's name, as the Strata page lists it (e.g. `IQ3_S`), the one request energy counts for; the image model keeps Strata's name; a request refused before it reached Strata has no `model` label.
- **Request energy** is the request energy of all days per model, in joules (Wh × 3600), so it adds up to the same figures as the energy per 1000 tokens.
- **PC energy** counts the power plug's readings in the history: each interval of `PLUG_SAMPLE_SECS` with readings counts for that long at their mean. Only measured readings count, never the energy estimate from `POWER_AWAKE_WATTS` and `POWER_ASLEEP_WATTS`. It agrees with the history's *Energy* for every fully measured hour.
- **Wake packets** counts the wake packets in the history.
- **Broken request bodies** counts the rows of the web UI's *Transport check*, kept in the history's database and never trimmed: a JSON body that reached the proxy broken (`arrival`, which the proxy refuses itself), one the PC service's relay got with other bytes than the proxy sent (`network`, relay mode with a PC service from 1.5.0), and one that went on whole and Strata refused as invalid JSON (`strata`; also every such refusal without relay mode or with an older PC service). The counter started at 0 with the release that brought it.
- **Expert tokens** are the output tokens of Strata's requests split by the expert tiers: where the routed experts of the decode ran, from the GPU's expert cache in VRAM (`vram`), copied over PCIe (`pcie`) or computed by the CPU from RAM (`cpu`). A request with n output tokens, Strata's `hit_rate` h and `pcie_share` p adds n × (1 − p) × h, n × p and n × (1 − p) × (1 − h); the three add up to n. After each finished request that reached Strata (not image requests), the proxy reads Strata's finished requests (`docs/strata-api.md`) in the background, never while the PC sleeps or wakes, and counts each Strata request once, by Strata's own id, for the model of the request that started the read (when several requests finish during one read, the latest of them). A request without a hit rate or a PCIe share (no expert lookups, an older engine) counts in no tier. There is no `key` label: Strata names no client there, and the code benchmark's requests count too. A failed read counts nothing; the next one catches up while Strata still holds the requests (its last 500). The counter started at 0 with the release that brought it: the requests Strata held at the first read do not count.

  The CPU's share of the decode over 5 minutes, and the expert cache hit rate the same way with `vram`:

  ```promql
  sum by (model) (rate(strata_proxy_expert_tokens_total{tier="cpu"}[5m]))
    / sum by (model) (rate(strata_proxy_expert_tokens_total[5m]))
  ```

The counters carry on across restarts: they are read from the history's database. The request totals started empty with the release that brought them; the request energy, the PC energy and the wake packets include everything saved before. A deleted database, or a changed `PLUG_SAMPLE_SECS` for the PC energy, makes a counter drop, which Prometheus treats as a reset.

## Strata's tool-call counters

Strata prints two counters only on its `/metrics/prometheus` (`docs/strata-api.md`). The proxy reads them there on every scrape while Strata is up, and they come after the proxy's own lines, with Strata's names, help text and type and without labels:

| Name | Type | Meaning |
|---|---|---|
| `strata:tool_calls_unread_total` | counter | `<tool_call>` blocks Strata returned as text: an undeclared tool name or a form the template does not use |
| `strata:tool_args_repaired_total` | counter | tool call arguments (a JSON array or object) that were damaged and read as written |

- Both are always in the answer. Each shows the latest value Strata printed, also a lower one: Strata keeps these totals in memory, so they drop when Strata restarts.
- While the PC sleeps or wakes, Strata is not up or does not answer, its `/metrics/prometheus` answers an error, or a Strata without these counters does not print them, each shows the last value the proxy read. A sleeping PC therefore makes no counter reset, and Prometheus counts no tool calls twice after a wake-up.
- Before the proxy read a value since its start, each shows 0. The last values live in memory only: a restart of the proxy starts at 0 again until Strata is up.
- No other line of Strata's `/metrics/prometheus` reaches the answer: each has a stand-in among Strata's lines below.

## Strata's lines

While Strata is up, its own lines from `/metrics?format=prometheus` (`vllm:*`, `strata:*`, `docs/strata-api.md`) follow, unchanged. While it is not up, or does not answer, they are missing and the scrape still answers `200`. Of Strata's `/metrics/prometheus` (`llamacpp:*`) only the two tool-call counters above are read.

## Keys that hide the model

For an API key that hides the model, every model name in the answer is the public model name: Strata's `model_name` label and the proxy's `model` label. The tool-call counters carry no model name and stay the same. The image model keeps its own name, as in answers. Series of the proxy that then share all labels add up into one line. Every key sees the counters of all keys by name.

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
