# What the proxy uses from Strata

Strata is its own engine ([Niko1221/Strata](https://github.com/Niko1221/Strata)), not llama-server. Some endpoints carry llama-server or vLLM names so that third-party tools work. What they really return is only in Strata's code.

In the proxy, only `crates/nas-proxy/src/strata_client.rs` talks to Strata: `/health`, the metrics, the request for an update's summary and the forwarding. A new call belongs there.

As of: commit [`cbfd5b9`](https://github.com/Niko1221/Strata/tree/cbfd5b9) (2026-10-20). The line numbers refer to it.

## Reading the sources

Download a file raw into the scratchpad and read it with `grep` and `sed`:

```sh
curl -sS -o server.py https://raw.githubusercontent.com/Niko1221/Strata/cbfd5b9/serve/server.py
```

To see what changed in Strata since the commit this page is pinned to, clone it without the file contents and read the log of the files the proxy and the PC service depend on (`setup.py` for setup's output, `serve/` for the HTTP API). The GitHub API without a token may refuse the request; a clone works.

```sh
git clone -q --filter=blob:none https://github.com/Niko1221/Strata.git strata
git -C strata log --oneline cbfd5b9..HEAD -- setup.py serve/
git -C strata grep -n 'GB ({\|=== Step' HEAD -- setup.py   # the lines the PC service reads
```

`serve/server.py` has over 8000 lines. A web fetch with a summary cuts it off and quotes no handlers. The routes are in `do_GET` from line 5950.

## Re-checking this page

Move the pin above only after comparing the code, not only the commit subjects: a change with a plain subject can still break the PC service. For each item, diff it from the pin to `HEAD` (`git -C strata diff cbfd5b9..HEAD -- <file>`) and follow a change to every place that reads it here:

| In Strata | Mirrored in | Read by |
|---|---|---|
| `setup.py`: `MODELS`, `FAMILIES`, `CONTEXTS`, `MEMORY_TIERS`, `mem_have` | `crates/pc-agent/src/catalog.rs` | `fits`, `recommended`, the Web UI's preselect in the model choice |
| `setup.py`: speed projection flags, `--max-context`, rope flags, `choices_from_config` | `crates/pc-agent/src/install.rs` (`set_speed_projection`, `set_context`) | the jobs `set_speed_projection` and `set_context` |
| `strata_control/user_args.py`, `engine_args` in `serve/server.py` | `install.rs` (`started_args`, `change_args`) | `speed_projection` and `context` in `GET /strata` |
| `serve/server.py` (the engine's environment from the run config's `env`), `setup.py` (`SETUP_ENV`, `carry_over`) | `install.rs` (`set_env`, `REFUSED_ENV`) | `env` in `GET /strata`, `set_engine_settings` |
| `setup.py`: `step`, `fail`, download and check lines | the PC service's setup output reader | the job's step and progress |
| `serve/prometheus.py` | `crates/nas-proxy/tests/fixtures/strata-metrics-*.prom` | the Overview |
| `serve/server.py`: `do_GET`, `_authorized`, `/health`, `MODEL_CARD_SAMPLING`, `sampling_defaults_from_config` | `strata_client.rs`, `crates/pc-agent/src/sampling.rs` | the proxy, the sampling the PC service writes |
| `START-HERE.bat`, `UPDATE.bat`, `BENCH.bat`, `tools/strata_bench.py` | `crates/pc-agent/src/strata.rs` | install, update and benchmark jobs on a Strata folder from source |
| `packaging/install.ps1`, `packaging/package_terminal.py` (the Terminal's files, `STRATA_DATA_DIR`) | `crates/pc-agent/src/terminal.rs` (`root`, `program`, `version`, `install`) | the install job, `install` and `version` in `GET /strata` |
| `strata_cli/update_cmd.py` (`run`), `update_engine`, `update_control`, `check` in `strata_control/updater.py` | `terminal.rs` (`engine_installed`, `updated`, `update_available`, their tests) | the install job's engine step, the update job, `update_available` |
| `strata_cli/handover.py` (`find_target`), `_VersionAction` in `strata_cli/cli.py`, `COMMIT` in `packaging/package_terminal.py` | `terminal.rs` (`root`, `version_text`, `commit`) | `version`, the update summary, the files setup runs from |
| `_bench_standard` in `strata_cli/cli.py`, `summaries_of` in `strata_cli/bench_standard.py` | `terminal.rs` (`bench_speeds`) | the Terminal's benchmark |
| `strata_cli/serve_cmd.py`, setup's install mode (`STRATA_CONTROL_INSTALL`), `STRATA_INSTALL_DIR` | `terminal.rs` (`serve_args`, `setup`, `command`) | the start of Strata, `install` and `add_model` on the Terminal |

## API key

If a key is set in Strata, the protected endpoints require `Authorization: Bearer <key>` or `x-api-key: <key>`. If it is missing, they answer with 401 (`_authorized`, line 5929). The PC service writes a new key into `api_key` of every model configuration before each start of Strata, and the proxy fetches it from the PC service (see [pc-agent-api.md](pc-agent-api.md), `GET /strata/key`).

Since commit e5266885, Strata also starts on the network without a key and only warns; `"api_key": "none"` in a model configuration means no key on purpose. Because the PC service always writes a key, this changes nothing for the proxy.

## `GET /health`

Always 200 and without a key (line 6213):

```json
{"status": "ok", "max_context": 32768, "model": "…", "images": false, "api_key": true,
 "image_generation": false, "loaded": true, "service": "strata"}
```

`loaded` is false as long as the model is not in memory. Strata loads it with the next request. If `/health` does not answer at all, Strata is not running or not up yet.

## Summary of an update: `POST /v1/chat/completions`

For the summary of a Strata update on the *Version & benchmark* tab, the proxy asks Strata itself, with its key and without streaming. Strata takes `reasoning_effort` per request (`none`, `low`, `medium` or `high`); the proxy sets `medium`. The answer is in `choices[0].message.content`, the thinking is not in it. On its own it only asks when `/health` reports `loaded: true` and the metrics show no running or waiting request; the *Generate summary now* button also asks a busy or unloaded Strata and then gives it two more minutes to load. `max_context` from `/health` limits the length of the request.

Since commit f31b6efd, Strata shortens a `max_tokens` that does not fit into the context to the room left, instead of answering 400 (`"fit_max_tokens": false` in the configuration brings back the 400). A prompt that alone does not fit is still 400. With `max_queue` in the configuration, Strata also answers 429 (`admission_full`, `queue_wait_timeout`) or 503 (`server_draining`) with `Retry-After`; setup does not write `max_queue`, so the PC service's Strata never does this.

## Metrics: `GET /metrics?format=prometheus`

The Overview uses this endpoint. It needs the key (line 5997) and returns Prometheus text with vLLM names and `strata:` names (`serve/prometheus.py`, `render`). Every line carries the label `model_name`:

| Name | Meaning |
|---|---|
| `vllm:num_requests_running` | requests currently reading or generating |
| `vllm:num_requests_waiting` | waiting requests |
| `strata:live_tok_s` | generated tokens/s of the last few seconds, 0 without generation |
| `strata:live_state{state="…"}` | 1 for `unloaded`, `idle`, `reading` or `generating` |

Without `format=prometheus` (and without `Accept: text/plain`), `/metrics` returns JSON for the monitor tab. The fields there are the same, e.g. `live.tok_s`.

The tests read real output of `render` from `crates/nas-proxy/tests/fixtures/strata-metrics-*.prom`. If Strata changes, regenerate them from the new `serve/prometheus.py` and update the commit above. `serve/prometheus.py` has not changed between 477ac13 and cbfd5b9.

## `GET /metrics/prometheus`: not for the Overview

This endpoint returns llama-server names (`llamacpp:*`, `Service.prometheus`, line 4289), needs the key and is not suitable for the Overview. Two reasons:
- `llamacpp:predicted_tokens_seconds` counts only finished requests and stays at 0 during a running request.
- Every fetch resets the time window of these values, also for other readers such as Grafana.

## Other endpoints

- **`/slots`:** in the style of llama-server, with the state of each slot.
- **`/status` and `/v1/status`:** what the server is doing right now.
- **`/v1/telemetry`** (new since 477ac13): a snapshot of the work and the hardware for Strata's terminal; it takes the API key or a separate `telemetry_read_key`.

The proxy uses none of them. The PC service polls `/slots` (see [pc-agent-api.md](pc-agent-api.md)).

## Strata Terminal (for the PC service's Strata jobs)

Checked at commit [`011c9e6`](https://github.com/Niko1221/Strata/tree/011c9e6) (2026-10-21), only for the points below; the pin at the top stays until the rest of this page is compared. Sources: `install.ps1`, `strata_cli/` (`update_cmd.py`, `serve_cmd.py`, `launch_cmd.py`, `handover.py`, `cli.py`, `bench_standard.py`, `__init__.py`), `strata_control/updater.py`, `packaging/package_terminal.py`, `setup.py`.

- **Install:** `install.ps1` from the latest release (`https://github.com/Niko1221/Strata/releases/latest/download/install.ps1`) checks the archive's SHA-256 against the signed feed and unpacks it into `$STRATA_DATA_DIR\terminal\<version>`, with the junction `terminal\current` to it. `STRATA_DATA_DIR` defaults to `%LOCALAPPDATA%\Strata`; the PC service sets it to `strata_home`. In `terminal\current`: `bin\strata.cmd`, `python\python.exe`, `setup.py`, `serve\`, `data\` (with the speed projection's control vector) and `VERSION`. `crates/pc-agent/src/terminal.rs` knows these names.
- **Run configs:** the Terminal looks for them in `STRATA_INSTALL_DIR`, else in the installs remembered in `settings.json` (an old Strata folder from source among them). The PC service sets `STRATA_INSTALL_DIR` to `<strata_home>\install` for every call. The files there are the same `strata-<id>.json` as in a Strata folder from source; the model files go to `--data-dir` as before.
- **Engine:** `strata update --no-control --output json` installs the engine for this PC from the signed feed and leaves the Terminal as it is. Its answer is the last line that is a JSON object: `results[]` with `component: "engine"` and `status` `updated`, `current`, `none-for-this-pc` (with `note` and `reasons`) or `failed` (with `error`), or `{"ok": false, "error": {"code", "message"}}`. `terminal.rs` (`engine_installed`) reads it; its tests hold real answers.
- **Model install:** setup in its install mode, as `strata launch` and Strata's desktop app call it: `python\python.exe setup.py --yes --no-start --no-browser …` with `STRATA_CONTROL_INSTALL=1`. It skips the engine fetch, the Python packages and the start, and prints the usual setup output (below).
- **Serving:** `strata serve --profile <run config> --host … --port …` serves in the foreground. It has no key option; the key is the run config's `api_key`. Stop it via the folder `strata_home`: its Python starts the engine.
- **Update:** `strata update --output json` installs the engine and newer Terminal files ("Control") from the signed feed, each only after a test start; it refuses with exit 7 while an engine serves, so the PC service stops Strata first. The answer has one result per part: `engine` as above, `control` with `updated`, `current`, `not-in-feed` or `failed`. `terminal.rs` (`updated`) reads it.
- **Newer Terminal files:** an update puts Control into `<components>\control\<version>` and names it in `control\active.json`; `components` is Strata's own folder: `STRATA_COMPONENTS_DIR`, else `%LOCALAPPDATA%\Strata\components`. The PC service's `strata_components` follows the same rule, and `strata_processes` names it too when it is not inside `strata_home`. `strata.cmd` in `terminal\current` hands each command over to it when its `VERSION` is newer and it holds `python\python.exe`, `strata.py` and `strata_control` (`find_target` in `strata_cli/handover.py`). The PC service follows the same rule for the files it runs itself (`setup.py`, `VERSION`, `COMMIT`, `data`, `serve`): `Terminal::root`.
- **Update check:** `strata update --check --output json` downloads only the feed and answers `engine.status` (`available`, `install`, `current`, `none-for-this-pc`, `failed-before`, `skipped`, `not-in-feed`) and `control.status` (`available`, `current`, `not-in-feed`). `terminal.rs` (`update_available`) counts `available`/`install` for the engine and `available` for Control.
- **Version:** `strata --version` prints `strata <PRODUCT_VERSION> (build <VERSION file>; python … at …)` (`_VersionAction` in `strata_cli/cli.py`). The files also carry `COMMIT`, the full Strata commit they were built from; the update summary compares it with the latest release's tag through GitHub's compare API.
- **Benchmark:** `strata bench run --url <server> --out <new folder> --output json` runs the standard suite (`docs/BENCHMARK_PROTOCOL.md`) against a running server with the key from `STRATA_API_KEY` (`--credential-env`). `--out` must not exist yet. Its answer: `{"ok", "path", "status", "quality", "summaries": [{"phase", "case_id", "metric", "median", …}]}`; exit 6 when the run is not complete. The PC service takes the `performance` medians: `short`/`decode_tps` and `prompt-4k`/`prompt_tps` (`bench_speeds`). These are not comparable with `BENCH.bat`'s figures.
- **Tune:** `strata tune auto <run config> --goal speed --mode quick|thorough --use chat|coding --status-file <file> --output json` starts the engine itself, so the PC service stops Strata first. While it runs it rewrites the status file at each step (`message`, `runs_done`, `runs_planned`, `eta_s`, and `speeds`: the measurements so far as `{label, tok_s}`, `base` first, then the setting tried by its label). Its answer is `{"schema_version": 1, "ok": true, "result": {…}}` with `code` (`faster`, `already_fastest`, `busy_pc`, `failed`, `stopped`), `values` (`base_tok_s`, `tuned_tok_s`, `gain_pct`), `settings` (`args`, `env`, `per_request`), `preset` (`{id, name}` or `null`) and `message`; exit 0 for faster or confirmed settings, 130 when cancelled, 6 otherwise. A saved preset is active at once (`presets.create(activate=True)`). The tuner knows the families `qwen36` and `flash-next` (`detect_family`; Swift and Gyro are Flash-Next builds). `terminal.rs` (`tune`, `tune_progress`, `tune_result`) reads it; its tests hold a real answer.
- **Preset undo:** `strata presets undo <id> --output json` brings back the settings that were active before the preset, or the standard settings when there were none. `terminal.rs` (`undo_preset`).
- **Preset off:** `strata presets off <run config> --output json` switches the slot's saved tuning off (answers `"active": null`); the presets stay saved. `terminal.rs` (`preset_off`).
- **Presets:** they live in Strata's own folder (`presets` beside the profiles, `STRATA_PRESET_DIR` if set), so they outlive both install folders. A preset belongs to a slot: the model file's digest, context, KV format, pictures and goal (`slot_key` in `strata_control/presets.py`); another context is another slot. `strata presets apply <id> --output json` makes it the slot's active one and keeps the one before for undo; `strata serve` puts the active preset over the run config when it starts (`presets.overlay`), so a change counts from the next start. `strata presets status <run config> --output json` answers `active` (a preset summary or `null`), `state` (`fresh`, `soft`, `hard` from `staleness`), `reasons` (plain words, e.g. "the graphics driver changed") and `presets`, the slot's presets newest first, each as `_summary` in `strata_cli/tune_commands.py` writes it (`id`, `name`, `active`, `gain_pct`, `settings` with `args`, `env` and `per_request`, `decode_tok_s`, `baseline_decode_tok_s`, …). A refusal is `{"ok": false, "error": {"code", "message"}}`. `terminal.rs` (`apply_preset`, `presets_status`, `read_presets`) reads them; its tests hold an answer in that shape.
- **Options** (for the optimization prompt): the Terminal has no command that lists them, so the PC service runs `python\python.exe -c <script>` in the Terminal files that run (`Terminal::root`, so newer Control after an update; `terminal.rs`, `OPTIONS_SCRIPT`). It prints one JSON line with the tables of `strata_control/tune_knobs.py` (`BOUNDS`, `NEVER`, `ANSWER_OF_FLAG`, `BOOL_FLAGS`, `ARMS` with each arm's `set_args` and `env`) and, for the run config named as argument, the engine's `--help`, started the way the tuner starts it (`engine_component.expand` for a `component:` reference, `tuning.engine_command` and `tuning.engine_env`, which puts the engine package's folders on `PATH`). The engine writes its usage (`usage()` in `src/program/generate.cpp`) to stderr and exits 0; as in `strata_cli/launch/params.py`, it counts only with a `--` in it. A flag missing from `ANSWER_OF_FLAG` is `exact`. `read_options` reads the line; its test holds the real output.

## Setup and files (for the PC service's Strata jobs)

Sources: `setup.py`, `START-HERE.bat`, `UPDATE.bat`, `BENCH.bat`, `tools/strata_bench.py`.

- **Model ID** = family prefix + size: `qwen` without a prefix (`IQ3_XXS`), otherwise `swift-`, `coder-`, `unsloth-`, `qwen36-`, `gyro-` (Gyro-S and Gyro-M, since ba64156f) and `nvfp4-` (NVIDIA only). `crates/pc-agent/src/catalog.rs` mirrors the tables for this, without `nvfp4-`. Gyro's memory (`ram_gb` 27.6 and 35) is RAM and VRAM together (`mem_total`); the catalog rounds it up to 28.
- **Setup without questions:** `START-HERE.bat` runs `.venv\Scripts\python.exe setup.py %*`, or `python\python.exe` when a bundled Python (Strata's terminal archive) is in the folder. Which arguments the service passes is in `strata.rs` (`setup`). `--setup` is needed as soon as a model is installed.
- **Experimental speed projection:** `--experimental-speed-projection on|off`, only for `qwen` and `coder`. Despite the name, no speed-up: a control vector that, according to Strata's `docs/DETAILS.md`, removes the model's refusal direction. Without the argument, setup takes over the value of an earlier installation, so the service always passes it. Clients turn it off per request with `"experimental_speed_projection": false`.
  - For this, setup writes these flags into `args` of the model configuration (`setup.py`, line 8498): `--control-vector-scaled <Strata>\data\experimental-speed-projection\Qwen3.8-Flash-Next-experimental-speed-projection.gguf:1.0 --control-vector-layer-range 4 44 --cvec-mode project --cvec-dir per-layer`. A later setup reads them back (`choices_from_config`); calibration does not depend on them.
  - For installed models, the PC service writes these flags itself (`set_speed_projection`). If Strata changes them, update `strata.rs` (`write_speed_projection`).
- **Options set by hand** (since commit 8aaf188a, `strata_control/user_args.py`): the model configuration also has `args_written` (`args` as setup last wrote it) and `user_args`. A setup run that writes the configuration again moves every option of `args` that differs from `args_written` into `user_args`, except the options setup chooses itself (`SETUP_CHOICE_FLAGS`: `--max-context`, `--kv`, `--vision`, `--mtp`, `--spec-min-p`, `--batch-mtp`, `--layer-split`). `user_args` wins over `args` at every start (`engine_args` in `serve/server.py`) and in every later setup run, so an option there cannot be taken out through `args` alone. `--update` does not move anything (`upgrade_config`); an engine option changed in Strata's web settings and `start --vram-reserve-mib` do. When the PC service switches the speed projection or the context, it changes `args_written` the same way and takes its options out of `user_args` (see [pc-agent-api.md](pc-agent-api.md)). The control vector options are not in `SETUP_CHOICE_FLAGS`.
- **Engine environment:** the run config's `env` (an object of strings) goes into the engine's environment at every start, after the variables of the GPU choice (`CUDA_VISIBLE_DEVICES`, `HIP_VISIBLE_DEVICES`, `CUDA_DEVICE_ORDER`), so it would override them; Strata's offline policy is applied after it. A setup run writes its own names again (`SETUP_ENV` in `setup.py`: the AMD tuning tables and switches) and keeps every other entry (`carry_over`). An active preset's `env` goes over it (`presets.overlay`). It reaches only the engine, not Strata's server process.
- **Sampling without values in the request** (`Service._run`): first the model card's set for the thinking state (`MODEL_CARD_SAMPLING` by `arch` of the configuration, without `arch` `qwen4exp`; since commit a8091f4d, 2026-10-15), on top of that the configuration's `sampling` block (`sampling_defaults_from_config`, since 9e6e5b7a, 2026-09-26; Strata names unknown keys at startup and skips them), on top of that the chat settings from Strata's web app (`strata-<id>.shared-settings.json`). The request's fields always win. Before 9e6e5b7a a request without sampling is greedy. Values at e88ee39d, unchanged at cbfd5b9: `qwen4exp` with thinking temperature 1.0, top_p 0.95, top_k 20, without 0.7, 0.8, 20, presence_penalty 1.5; `qwen35moe` with thinking adds presence_penalty 1.5, without the same as `qwen4exp`. `crates/pc-agent/src/sampling.rs` mirrors them.
  - The block has one set for both states and hides the model card. So the PC service writes the thinking set only into a Strata without `MODEL_CARD_SAMPLING` that knows `sampling_defaults_from_config`; otherwise it removes the block (see [pc-agent-api.md](pc-agent-api.md)). It recognises the version by these names in `serve/server.py`.
- **Loops:** Strata ends an answer that repeats a token `repeat_stop_tokens` times (256) in a row (#606, commit bb162e2e). Thinking that repeats whole passages is only handled with `"reasoning_loop_recovery"` in the model configuration (commit c3473262): `"stop"` ends the answer, `"recover"` (or `true`) continues with the instruction for low effort; without the key, off. The PC service sets `"recover"` (see [pc-agent-api.md](pc-agent-api.md)). Older Strata versions read the configuration with `cfg.get` and skip unknown keys.
- **Setup output:** steps as `=== Step N: title ===`, downloads as `file: x / y GB (p%)`, errors start with `[X]`. Since [`1972af6`](https://github.com/Niko1221/Strata/commit/1972af6), the SHA-256 check of a file of 1 GiB or more prints the same figure as `checking file: x / y GB (p%)` (`verify_sha256` in `setup.py`), after each download and for the engine in step 4; the PC service shows it as the step `N/7: Checking file`.
- **Where a model's files go:**
  - `strata-<id lowercase>.json` (configuration), `run-<id lowercase>.bat` (start) and `strata-<id lowercase>.log` in the Strata folder
  - `<data>/models/<ID>` (GGUF files, also `.part`) and `<data>/packs/<id lowercase>` (tokenizer)
  - `<data>/mtp` is shared by all models.
- **Model configuration:** a `strata-*.json` with `exe` and an array `args` (that is how `find_configs` in `strata_bench.py` recognises them). `strata-*.shared-settings.json` is not one. Strata writes them with `json.dumps(cfg, indent=1)`. The key is in `api_key`.
- **Context:** setup writes `--context` as `--max-context <tokens>` into `args` and reads it back that way (`choices_from_config`). Above 262144 (the trained length), `--rope-scaling yarn --rope-scale <context / 262144>` is added. From the context, setup also derives KV streaming (`--kv-resident`), parallel slots and the calibration key; the PC service's context switch leaves these as they are.
- **Active model:** `run-<id>.bat` starts `serve/server.py`, which starts `strata.exe` and restarts it when it dies; so stop it via the folder. Without its own start script, `START-HERE.bat` starts the configuration that was used last (the most recent modification time, because a start touches it).
- **Setup's recommendation** (`MEMORY_TIERS`, since aa82a886): by RAM and VRAM together, from 62 GB `IQ3_S`, from 46 GB `IQ3_XXS`, from 38 GB `swift-IQ2_XS`; below that the older rules (Gyro-S, or `coder-IQ1_M`). `catalog.rs` (`recommend`) mirrors the tiers, with the biggest card's VRAM; below them it keeps `coder-IQ1_M` from 28 GB of RAM. Gyro is never recommended. Context 64K with 14–20 GB VRAM; the skill `local-agent` needs more (see `tools/local-agent/README.md`).
- **Update** (Strata folder from source) like `UPDATE.bat`: `git pull --ff-only`, then `START-HERE.bat --update` with Strata stopped (otherwise the engine is not replaced). Model files stay.
- **Benchmark:** `BENCH.bat --config <strata-<id>.json>` uses a running Strata, otherwise it starts one itself. The result is the line `  chat decode tok/s   38.50   (37.10 - 39.20, n=3)` of the table at the end. More lines follow after it.
