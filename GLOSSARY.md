# Strata Home Proxy

A proxy on the NAS that forwards requests to Strata on the PC, wakes the PC for them and sends it to sleep after an idle timeout.

The glossary terms are the words the web UI shows. The _Avoid_ lists apply to docs, issues, code identifiers and all other text. The web UI may name a product where the user recognises the device by it (e.g. "Shelly plug").

## Parties

**Strata**:
Strata's own LLM engine on the PC (Niko1221/Strata) with an OpenAI- and Anthropic-compatible API. Not llama-server, even though some endpoints carry its names.
_Avoid_: llama-server, llama.cpp, model server

**Strata folder**:
The folder on the PC that setup installs Strata into (`strata_dir`): model configs `strata-<id>.json` with their chat settings, start scripts and logs. The model files themselves live in the Strata data folder (`strata_data_dir`). In the PC service, only `install::Installation` knows how setup names a model's files.
_Avoid_: Strata directory, install folder, Strata-Ordner

**PC service**:
The small program on the PC that reports state, gaming mode and user activity to the proxy and triggers standby on request. In code and file names it is `pc-agent`.
_Avoid_: agent, PC agent (except `pc-agent` in code and file names), PC-Dienst

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
A state of the PC in which Strata is stopped, requests are refused and the proxy does not send the PC to sleep.
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
