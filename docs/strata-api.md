# What the proxy uses from Strata

Strata is its own engine ([Niko1221/Strata](https://github.com/Niko1221/Strata)), not llama-server. Some endpoints carry llama-server or vLLM names so that third-party tools work. What they really return is only in Strata's code.

In the proxy, only `crates/nas-proxy/src/strata_client.rs` talks to Strata: `/health`, the metrics, the request for an update's summary and the forwarding. A new call belongs there.

As of: commit [`477ac13`](https://github.com/Niko1221/Strata/tree/477ac13). The line numbers refer to it.

## Reading the sources

Download a file raw into the scratchpad and read it with `grep` and `sed`:

```sh
curl -sS -o server.py https://raw.githubusercontent.com/Niko1221/Strata/477ac13/serve/server.py
```

`serve/server.py` has over 7000 lines. A web fetch with a summary cuts it off and quotes no handlers. The routes are in `do_GET` from line 5372.

## API key

If a key is set in Strata, the protected endpoints require `Authorization: Bearer <key>` or `x-api-key: <key>`. If it is missing, they answer with 401 (`_authorized`, line 5362). The proxy uses `STRATA_API_KEY` for this.

## `GET /health`

Always 200 and without a key (line 5460):

```json
{"status": "ok", "max_context": 32768, "model": "…", "images": false, "api_key": true,
 "image_generation": false, "loaded": true, "service": "strata"}
```

`loaded` is false as long as the model is not in memory. Strata loads it with the next request. If `/health` does not answer at all, Strata is not running or not up yet.

## Summary of an update: `POST /v1/chat/completions`

For the summary of a Strata update on the *Version & benchmark* tab, the proxy asks Strata itself, with its key and without streaming. Strata takes `reasoning_effort` per request (`none`, `low`, `medium` or `high`); the proxy sets `medium`. The answer is in `choices[0].message.content`, the thinking is not in it. On its own it only asks when `/health` reports `loaded: true` and the metrics show no running or waiting request; the *Generate summary now* button also asks a busy or unloaded Strata and then gives it two more minutes to load. `max_context` from `/health` limits the length of the request.

## Metrics: `GET /metrics?format=prometheus`

The Overview uses this endpoint. It needs the key (line 5416) and returns Prometheus text with vLLM names and `strata:` names (`serve/prometheus.py`, `render`). Every line carries the label `model_name`:

| Name | Meaning |
|---|---|
| `vllm:num_requests_running` | requests currently reading or generating |
| `vllm:num_requests_waiting` | waiting requests |
| `strata:live_tok_s` | generated tokens/s of the last few seconds, 0 without generation |
| `strata:live_state{state="…"}` | 1 for `unloaded`, `idle`, `reading` or `generating` |

Without `format=prometheus` (and without `Accept: text/plain`), `/metrics` returns JSON for the monitor tab. The fields there are the same, e.g. `live.tok_s`.

The tests read real output of `render` from `crates/nas-proxy/tests/fixtures/strata-metrics-*.prom`. If Strata changes, regenerate them from the new `serve/prometheus.py` and update the commit above.

## `GET /metrics/prometheus`: not for the Overview

This endpoint returns llama-server names (`llamacpp:*`, `Service.prometheus`, line 3792), needs the key and is not suitable for the Overview. Two reasons:
- `llamacpp:predicted_tokens_seconds` counts only finished requests and stays at 0 during a running request.
- Every fetch resets the time window of these values, also for other readers such as Grafana.

## Other endpoints

- **`/slots`:** in the style of llama-server, with the state of each slot.
- **`/status` and `/v1/status`:** what the server is doing right now.

The proxy uses neither. The PC service polls `/slots` (see [pc-agent-api.md](pc-agent-api.md)).

## Setup and files (for the PC service's Strata jobs)

Sources: `setup.py`, `START-HERE.bat`, `UPDATE.bat`, `BENCH.bat`, `tools/strata_bench.py`.

- **Model ID** = family prefix + size: `qwen` without a prefix (`IQ3_XXS`), otherwise `swift-`, `coder-`, `unsloth-`, `qwen36-`. `crates/pc-agent/src/catalog.rs` mirrors the tables for this.
- **Setup without questions:** `START-HERE.bat` runs `.venv\Scripts\python.exe setup.py %*`. Which arguments the service passes is in `strata.rs` (`setup`). `--setup` is needed as soon as a model is installed.
- **Experimental speed projection:** `--experimental-speed-projection on|off`, only for `qwen` and `coder`. Despite the name, no speed-up: a control vector that, according to Strata's `docs/DETAILS.md`, removes the model's refusal direction. Without the argument, setup takes over the value of an earlier installation, so the service always passes it. Clients turn it off per request with `"experimental_speed_projection": false`.
  - For this, setup writes these flags into `args` of the model configuration (`setup.py`, line 7011): `--control-vector-scaled <Strata>\data\experimental-speed-projection\Qwen3.8-Flash-Next-experimental-speed-projection.gguf:1.0 --control-vector-layer-range 4 44 --cvec-mode project --cvec-dir per-layer`. A later setup reads them back (`choices_from_config`); calibration does not depend on them.
  - For installed models, the PC service writes these flags itself (`set_speed_projection`). If Strata changes them, update `strata.rs` (`write_speed_projection`).
- **Sampling without values in the request** (`Service._run`): first the model card's set for the thinking state (`MODEL_CARD_SAMPLING` by `arch` of the configuration, without `arch` `qwen4exp`; since commit a8091f4d, 2026-10-15), on top of that the configuration's `sampling` block (`sampling_defaults_from_config`, since 9e6e5b7a, 2026-09-26; Strata names unknown keys at startup and skips them), on top of that the chat settings from Strata's web app (`strata-<id>.shared-settings.json`). The request's fields always win. Before 9e6e5b7a a request without sampling is greedy. Values at e88ee39d: `qwen4exp` with thinking temperature 1.0, top_p 0.95, top_k 20, without 0.7, 0.8, 20, presence_penalty 1.5; `qwen35moe` with thinking adds presence_penalty 1.5, without the same as `qwen4exp`. `crates/pc-agent/src/sampling.rs` mirrors them.
  - The block has one set for both states and hides the model card. So the PC service writes the thinking set only into a Strata without `MODEL_CARD_SAMPLING` that knows `sampling_defaults_from_config`; otherwise it removes the block (see [pc-agent-api.md](pc-agent-api.md)). It recognises the version by these names in `serve/server.py`.
- **Loops:** Strata ends an answer that repeats a token `repeat_stop_tokens` times (256) in a row (#606, commit bb162e2e). Thinking that repeats whole passages is only handled with `"reasoning_loop_recovery"` in the model configuration (commit c3473262): `"stop"` ends the answer, `"recover"` (or `true`) continues with the instruction for low effort; without the key, off. The PC service sets `"recover"` (see [pc-agent-api.md](pc-agent-api.md)). Older Strata versions read the configuration with `cfg.get` and skip unknown keys.
- **Setup output:** steps as `=== Step N: title ===`, downloads as `file: x / y GB (p%)`, errors start with `[X]`.
- **Where a model's files go:**
  - `strata-<id lowercase>.json` (configuration), `run-<id lowercase>.bat` (start) and `strata-<id lowercase>.log` in the Strata folder
  - `<data>/models/<ID>` (GGUF files, also `.part`) and `<data>/packs/<id lowercase>` (tokenizer)
  - `<data>/mtp` is shared by all models.
- **Model configuration:** a `strata-*.json` with `exe` and an array `args` (that is how `find_configs` in `strata_bench.py` recognises them). `strata-*.shared-settings.json` is not one. Strata writes them with `json.dumps(cfg, indent=1)`. The key is in `api_key`.
- **Context:** setup writes `--context` as `--max-context <tokens>` into `args` and reads it back that way (`choices_from_config`). Above 262144 (the trained length), `--rope-scaling yarn --rope-scale <context / 262144>` is added. From the context, setup also derives KV streaming (`--kv-resident`), parallel slots and the calibration key; the PC service's context switch leaves these as they are.
- **Active model:** `run-<id>.bat` starts `serve/server.py`, which starts `strata.exe` and restarts it when it dies; so stop it via the folder. Without its own start script, `START-HERE.bat` starts the configuration that was used last (the most recent modification time, because a start touches it).
- **Setup's recommendation:** from 60 GB RAM `IQ3_XXS`, from 44 GB `Q2_0`, from 28 GB `coder-IQ1_M`. Context 64K with 14–20 GB VRAM; the skill `local-agent` needs more (see `tools/local-agent/README.md`).
- **Update** like `UPDATE.bat`: `git pull --ff-only`, then `START-HERE.bat --update` with Strata stopped (otherwise the engine is not replaced). Model files stay.
- **Benchmark:** `BENCH.bat --config <strata-<id>.json>` uses a running Strata, otherwise it starts one itself. The result is the line `  chat decode tok/s   38.50   (37.10 - 39.20, n=3)` of the table at the end. More lines follow after it.
