# Strata Home Proxy

A proxy on the NAS that forwards requests to Strata on the PC, wakes the PC for them and sends it to sleep after an idle timeout.

The glossary terms are the words the web UI shows. The _Avoid_ lists apply to docs, issues, code identifiers and all other text. The web UI may name a product where the user recognises the device by it (e.g. "Shelly plug").

## Parties

**Strata**:
Strata's own LLM engine on the PC (Niko1221/Strata) with an OpenAI- and Anthropic-compatible API. Not llama-server, even though some endpoints carry its names.
_Avoid_: llama-server, llama.cpp, model server

**Strata Terminal**:
Strata's `strata` command, which Strata's own `install.ps1` puts into `strata_home` (by default `%LOCALAPPDATA%\Strata`). A new setup from the web UI installs it, and the PC service runs Strata as `strata serve`. In the PC service, only `terminal::Terminal` knows where it lives and how it is called.
_Avoid_: CLI, Strata CLI, strata.exe (except in code and file names)

**Strata folder**:
The folder on the PC with the run configs and their chat settings and logs: `install` in `strata_home` for the Strata Terminal. An older setup from source has a Git clone in `strata_dir` instead, which also holds Strata's code and start scripts (the *old Strata folder*). The model files themselves live in the Strata data folder (`strata_data_dir`). In the PC service, only `install::Installation` knows how setup names a model's files.
_Avoid_: Strata directory, install folder, Strata-Ordner

**Run config**:
A model's `strata-<id>.json` in the Strata folder, as setup writes it: the engine, its flags, the Strata key and the chat defaults. `strata serve` starts Strata with one. Older text says *model config*.
_Avoid_: launch config, profile (Strata's own profiles are something else)

**Engine options**:
A model's engine options set by hand: `user_args` in its run config, which Strata puts over `args` at every start and which setup keeps. The web UI shows them one option per line in each installed model's *Engine settings*, with the args Strata starts with. Only a Strata since commit 8aaf188a keeps them (its run configs have `args_written`).
_Avoid_: user args (except `user_args` in code, the run config and the web UI's label *Engine options (user_args)*), custom flags, extra args

**Engine environment**:
A model's environment variables for the engine: `env` in its run config, which Strata puts into the engine's environment at every start. The web UI shows them one `NAME=value` per line in *Engine settings*.
_Avoid_: env vars (except `env` in code, the run config and the web UI's label *Engine environment (env)*)

**Engine settings**:
The dialog of an installed model with its Engine options and its Engine environment, saved together with one restart.

**PC service**:
The small program on the PC that reports state, gaming mode and user activity to the proxy and triggers standby on request. In code and file names it is `pc-agent`.
_Avoid_: agent, PC agent (except `pc-agent` in code and file names), PC-Dienst

**PC service changelog**:
What each PC service version brings, in short lines for the user: `crates/pc-agent/CHANGELOG.md`. The PC service card lists the versions an update brings and marks one that needs a step on the PC. Rules in `docs/pc-service.md`.
_Avoid_: release notes, agent changelog

**Proxy**:
The service on the NAS that all requests to Strata go through. In code and file names it is `nas-proxy`.
_Avoid_: NAS proxy (except `nas-proxy` in code and file names), gateway

**Web UI**:
The proxy's pages under `/admin`, split into the areas Overview, PC, Strata, Access and System, plus the login.
_Avoid_: admin UI, dashboard, Weboberfläche

**Install command**:
The PowerShell one-liner from the web UI's *PC* area that downloads the PC service from the NAS, installs it and registers it with the proxy.
_Avoid_: installer, setup link, setup command, Einrichtungsbefehl

**One-time code**:
The code in the install command. Short-lived and valid for one registration only; a new install command invalidates the old one.
_Avoid_: pairing code, token (that is the shared secret of proxy and PC service), Einmalcode

**Overview**:
The web UI's start page with the live state of PC and Strata and the buttons.
_Avoid_: status page, home, Übersicht

## State of the PC

**Wake-up**:
The period from the wake packet until Strata (or, for gaming mode, the PC service) answers.
_Avoid_: boot, start, Aufwachen

**Idle timeout**:
The time without a request through the proxy after which the proxy wants to send the PC to sleep. It restarts with every request, on wake-up and after gaming mode.
_Avoid_: standby time, Leerlaufzeit

**Sleep request**:
The proxy's request to the PC service to trigger standby. The PC service can refuse it with a reason.
_Avoid_: shutdown, sleep command, Schlafen-Anfrage

**Forced sleep**:
A sleep request from the web UI's *Force sleep* button that the PC service refuses only during an update. It overrides user activity, a request in Strata and gaming mode (which stays on), and cancels a running Strata job first; downloaded parts are kept. The proxy treats it like an accepted sleep request.
_Avoid_: hard sleep

**Settling**:
The short time after an accepted sleep request during which the PC may still answer and yet counts as asleep.
_Avoid_: Nachlauf

**Hardware readings**:
What the PC service reports about load, temperature, memory and fans of the graphics card and CPU; the Overview shows them.
_Avoid_: stats, telemetry, Hardwarewerte

**Load lines**:
A model's Strata output from a start by the PC service until Strata answers `/health`: at most the last 100 lines of its latest load, kept on the PC across restarts of the PC service. The optimization prompt shows them.
_Avoid_: startup log, boot log

**Machine facts**:
What the PC is, as the PC service reads it on the first request and keeps it: each RAM stick, each graphics card with VRAM, driver and PCIe link, the CPU, Windows and the kind of drive that holds the models. The optimization prompt shows them; unlike hardware readings they do not change while the PC runs.
_Avoid_: specs, system info, hardware info

**Power plug**:
The Shelly plug the PC is plugged into. The proxy asks it only when it is set up (`SHELLY_URL`): for the Overview and once a minute for the history (`PLUG_SAMPLE_SECS`). It has no influence on waking and sleeping.
_Avoid_: Shelly, smart plug, Messsteckdose

**Power**:
What the power plug reports as the PC's current power draw, in watts; the Overview shows it in the Hardware area as *Power*, also while the PC sleeps.
_Avoid_: power consumption, Leistung

**Energy**:
The energy the PC used over a period of the history, in kWh and € at today's price. Measured where the power plug has readings for at least half of that hour, otherwise from the energy estimate; the web UI says "measured", "estimated" or "mixed".
_Avoid_: power consumption, Verbrauch

**Energy estimate**:
Energy from the settings instead of readings: awake hours (gaming mode included) × `POWER_AWAKE_WATTS`, asleep hours × `POWER_ASLEEP_WATTS`. Unknown time does not count.
_Avoid_: Stromschätzung

**Savings**:
What sleep saves compared with a PC awake around the clock: asleep hours × (`POWER_AWAKE_WATTS` − `POWER_ASLEEP_WATTS`), in kWh and €; always from the settings, even when readings exist. The web UI calls it *Saved*.
_Avoid_: Ersparnis

**Gaming mode**:
A state of the PC in which Strata is stopped, requests are refused and the proxy does not send the PC to sleep. One missed status answer from the PC service does not end it; only a second miss in a row counts the PC as asleep.
_Avoid_: game mode, Spielemodus, Gaming-Modus

**Keep awake**:
A period, chosen on the Overview, during which the proxy does not send the PC to sleep.
_Avoid_: keep-alive, Wach halten

## State of Strata

**Model unloaded**:
Strata answers but does not hold the model in memory and loads it with the next request.
_Avoid_: entladen, Modell nicht geladen

**Loading**:
The PC service reports Strata as running, but it does not answer yet.
_Avoid_: starting, Lädt

**Not reachable**:
Strata answers the PC service on the PC, but not the proxy: the network between NAS and PC drops it, e.g. the PC's firewall after a Strata update.
_Avoid_: unreachable (in UI text), Offline

**Forced Strata restart**:
*Force restart Strata* in the web UI: the PC service cancels a running Strata job, kills Strata hard (a hand-started one too) and starts its own Strata again when it has a start script and auto-start is on. Refused in gaming mode and during a PC service update.
_Avoid_: Strata kill, hard restart

**Event**:
An entry in the proxy's history about the PC, waking, sleeping or the web UI.
_Avoid_: log entry, Ereignis

**History**:
What the proxy keeps of events and power plug readings over time; the web UI shows it as bars per day, week or month.
_Avoid_: log, Verlauf

**Strata key**:
The key Strata asks of every request (`api_key` in the model configs). The PC service makes a new one before every start of Strata it makes, and the proxy fetches it from the PC service whenever its generation changes. Only the proxy and the PC service know it; clients use their own API keys.
_Avoid_: Strata API key, STRATA_API_KEY (an older setting of the proxy), token (that is the shared secret of proxy and PC service)

**Strata output**:
What a Strata started by the PC service writes to its console; the PC service keeps the last 500 lines, the web UI shows them live in the Strata area (tab *Output*). Not an event.
_Avoid_: Strata log, log entry (except `strata-log` and `strata.log` in code and file names), Strata-Ausgabe

**Optimization prompt**:
The English text per installed model that the web UI's *Optimization prompt* button shows for pasting into an AI chat: the PC, Strata's version, the model with its preset and last tune, and its benchmarks, with the request for faster settings. An active preset is named with its settings, since it wins over the run config. The proxy builds it from the PC service's last report; it never holds a path, the MAC address, a key or the Git remote.
_Avoid_: tuning prompt, export, Optimierungsprompt

**Tune**:
A Strata job that runs Strata's own tuner on one model to find faster settings, in a quick (about 20 minutes) or thorough (up to two hours) run, for coding or chat. Strata is off while it runs and the proxy answers requests at once. The result is kept per model and shown; it does not change the settings the model runs with.
_Avoid_: optimize, auto-tune, Optimierung

**Preset**:
Strata's saved settings from a tune, for one model file and its choices (context among them). Strata starts the model with the active one; *Apply* makes a preset active, *Undo* brings back the one before, *Turn off* goes back to the standard settings and keeps the preset saved. Strata says whether the active one still fits: a re-check is offered after a new driver or engine, it is not applied after a new graphics card or model file. After a context switch the model has none until it is tuned again.
_Avoid_: profile, tuning (for the saved result), Voreinstellung

**Pictures**:
Picture reading for a model: setup's *Pictures* tick in the web UI's install and add-model form, on by default, which makes setup write a picture reader into the run config. An installed model shows *Pictures: on (CPU)*, *on (GPU)* or *off* and switches it with one setup run; a model without a picture reader reads no pictures.
_Avoid_: vision (except `--vision` and the `vision` section in configs and code), image support, multimodal

## Code benchmark

**Code benchmark**:
The model solving fixed programming tasks with opencode in the code-bench container on the NAS, scored by hidden tests (Strata → Version & benchmark). Not the benchmark, which measures speed.
_Avoid_: coding test, eval

**Repo task**:
A code benchmark task that is a real feature of this repo: the agent works in a checkout from before the feature, the feature's own tests score it.
_Avoid_: Rust task (except as a title), real task

**Run over all models**:
What *Benchmark all models* starts: each installed model in turn solves the tasks; other keys' requests wait meanwhile, and the model active before comes back at the end.
_Avoid_: batch run, all-models run
