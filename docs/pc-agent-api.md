# Interface between proxy and PC service

The PC service offers these endpoints, the proxy uses them. What each
endpoint takes and answers is defined as types in the crate `strata-pc-protocol`
(`crates/pc-protocol`); this document describes what happens behind them.
Saved responses of today's PC service live in
`crates/pc-protocol/tests/samples`.

## Older PC services

The PC service updates itself from the NAS, so an older PC service often
talks to a newer proxy. Therefore:

- Fields are only added; none is renamed or removed.
- A response field added after the first version has a default value, so
  that an older PC service's response stays readable.
- An optional request field is absent as long as it is not set: the
  PC service rejects request fields it does not know. Kinds without fields
  (`update`, `benchmark`, `compare_models`, `switch_to_terminal`,
  `remove_old_strata_folder`) ignore extra fields.
- A request field a newer PC service ignores is still sent with a value an
  older one accepts: older services keep checking it (the `api_key` of
  `install` is a random throwaway key, because older services refuse an
  empty one).

The crate's tests read the saved responses plus responses that lack the
newer fields. A PC service test checks that the samples show every field it
sends; so a new field comes with its sample. `scripts/fake-pc.js` builds on
the same samples.

All requests carry `Authorization: Bearer <PC_AGENT_TOKEN>`; the service
always requires the token. It accepts requests only from `allowed_clients`
(the NAS) and from 127.0.0.1.

## `GET /status`

Response: `PcStatus`.

`strata_running` is `true` when Strata answers `/health`, or when the
service started it less than `load_time_secs` ago and has not stopped it
since. A Strata still loading its model (e.g. after a forced restart) thus
already counts as running; the Overview shows it as loading.

`strata_job` is `true` while a Strata job is running (see below), and
`strata_job_kind` names its kind; missing without a job and from an older PC
service. The proxy reads `tune` there: while a tune runs, Strata is off and
the proxy answers requests with a `503` instead of waking (see `tune`
below).

`version` is the crate version plus the build's commit (`STRATA_BUILD_ID`
at build time, otherwise `dev`). The same value is in the .exe as the text
`strata-pc-agent-version:<version>`; this is how the proxy reads the
version of the .exe in its image without running it. If it differs or
`version` is missing (service from before the update button), the *PC* area
shows "Update available".

`mac` is the MAC address of the network card the request came through (that
is, the one that must receive the wake packet), or the value `mac` from the
configuration. If it is unknown, the field is missing. The proxy remembers the
reported MAC in `DATA_DIR` and wakes with it; it takes precedence over `PC_MAC`
and the values from the web UI.

`hardware` are the hardware readings for the Overview. The service reads them
from Windows itself, without drivers: as long as `/status` is being asked, it
measures every 2 s in the background and answers with the last measurement.
The first request after 30 s of quiet starts measuring and gets only `null`
values (`Hardware`).

- `gpu` is the graphics card with the most VRAM, `null` without a graphics card.
- `power_pct` is the power draw as a share of the card's power limit, not
  watts.
- `fan_rpm: 0` means the fan is stopped. If the driver reports 0 and no
  maximum speed, it does not know the fan, and the value is `null`.
- The driver reports temperature and power as 0 when it does not know them;
  the service turns that into `null`.
- Every value the service cannot read is `null`; the others stay.
- CPU load covers the time since the previous measurement and is `null` in
  the first.

If `hardware` is missing, the service is older; the Overview then asks for an
update.

`strata_key_generation` changes with every new Strata key, never the key
itself; the proxy then fetches the key from `GET /strata/key`. The number
starts at a random value with each start of the service, so the proxy
compares it only for a change. It is there even while no model
configuration holds a key; only services before the key per start lack it.

If the service does not answer, the proxy assumes the PC is asleep and wakes
it.

## `GET /strata/key`

Response: `StrataKey`, the Strata key (`api_key`) and the
`strata_key_generation` it belongs to. `404` while no model configuration
holds a key.

Before every start of Strata (auto-start, gaming mode off, resume, watchdog,
the restart of a job, the forced restart, the start after a service update)
the service makes a new key: 32 random bytes as hex. It writes it into
`api_key` of every model configuration, keeping each file's modification
time, and only then starts Strata, which reads it from there. A configuration
it cannot read is noted in its log and stays as it is; if it cannot write
one, none keeps the new key and Strata does not start. Until its first start
of Strata the service reports the key the configurations already hold, so a
service restarted beside a running Strata still knows it. When
`serve/server.py` starts `strata.exe` again after a crash, or Strata is
started by hand, the key stays.

`strata_api_key` in the TOML is ignored, with a warning in the log.

## `POST /sleep`

The proxy sends this after the idle timeout. The service checks gaming mode,
running Strata requests (via `/slots`, if Strata offers it) and user activity
(last keyboard or mouse input, default 10 min).
The response (`Answer`) comes first, standby follows 2 s later.

- `200`, accepted: standby is triggered.
- `409`, refused with a reason, e.g. "User active". The proxy asks again
  after `SLEEP_RETRY_SECS`.

## `POST /sleep/force` (for the web UI)

The forced sleep behind *Force sleep* on the Overview, with no request body.
The service skips every check of `/sleep` except the update: user activity, a
request in Strata and gaming mode don't stop it. It cancels a running Strata
job first, as gaming mode does; downloaded parts are kept. Standby follows 2 s
after the answer, once the cancelled job has ended (at most
`cancel_wait_secs`, then anyway), on the same path as after an accepted
`/sleep` (`fresh_strata_after_standby` included). Gaming mode stays as it is,
its saved switch too.

- `200` (`ForcedSleep`): what it overrode (`user_active`, `strata_busy`,
  `gaming_mode`), and in `cancelled_job` the kind of the cancelled job; the
  field is missing without one.
- `409` (`Refused` with reason): only during an update of the service.

For `/sleep` and `/sleep/force`: while a standby is already pending, the
service accepts another sleep request without starting a second standby, so a
double click doesn't put the PC back to sleep right after it wakes. If an
update begins before the standby, the standby is skipped.

The proxy sends it only when it considers the PC neither asleep nor waking,
and never wakes the PC for it. An accepted one counts like an accepted
`/sleep` (settling, then asleep). A service without this endpoint answers 404;
the web UI then asks for a PC service update.

## `POST /strata/kill` (for the web UI)

The forced restart of Strata behind *Force restart Strata* in the Strata area,
with no request body. The service cancels a running Strata job and waits for it
to end, at most `cancel_wait_secs` (a cancelled `activate` or `set_context`
may restart Strata itself while rolling back; the kill comes after that). Then
it ends Strata as gaming mode does, with `taskkill /F /T` by the configured
programs and folders; that also hits a Strata started by hand. With a start
script and `auto_start` it starts Strata again after the usual pause, unless a
Strata job has begun meanwhile (that job then takes care of Strata); otherwise
Strata stays stopped. The watchdog counts afresh and leaves the new Strata
alone while it loads, and `/status` reports it as running meanwhile
(`strata_running`), so the web UI shows it as loading.

- `200` (`ForcedRestart`): `cancelled_job` as above; `started`, whether Strata
  was started again since the kill. `false` without a start script, with
  `auto_start = false`, after a failed start, or when a new job takes over
  Strata.
- `409` (`Refused` with reason): gaming mode on, or the service is updating. If
  the update only begins during the wait or the pause after the kill, the
  service refuses too and doesn't start Strata; the updated service does.

The proxy sends it only when it considers the PC neither asleep nor waking, and
never wakes the PC for it. A service without this endpoint answers 404.

## `POST /update` (for the web UI)

Downloads `strata-pc-agent.exe` from `nas_url` in the configuration
(`/admin/agent/strata-pc-agent.exe`). Then it ends Strata as *Force restart
Strata* does (a Strata started by hand too) and waits up to
`update_stop_wait_secs` (10 s) until Strata no longer answers `/health`. Then
it renames the running .exe to `strata-pc-agent.exe.alt`, puts the new one in
its place and starts it with the same arguments. The response comes after
that; 1 s later the old service exits, the new one waits for the port until
then and then deletes the `.alt`.

If Strata was running (or loading) before, the old service leaves the file
`strata-after-update` in its state folder. The new service removes it at its
start, brings the model configs up to date and then starts Strata once, with
a start script and `auto_start`, outside gaming mode; a failed start is left
to the watchdog. Without the file the watchdog starts Strata as usual. From
the stop until the old service exits, nothing in it starts Strata (watchdog,
waking from standby, gaming mode switched off).

- `202` (`Answer`): the new .exe is about to run.
  From the download until the end, the service refuses Strata jobs and
  sleeping.
- `409` (`Refused` with a reason): not now (Strata is handling a request, a
  Strata job or an update is running; a request that began during the
  download counts too), set up without `nas_url`, or a running Strata could
  not be stopped or still answered after the stop ("Strata did not stop");
  then nothing was swapped and no second Strata is started next to it.
- `500` (`Refused`): download or replacement failed; the old service keeps
  running. A failed download leaves Strata alone; after a failed replacement
  the old service starts the Strata it stopped again.

The proxy sends this only while the PC is awake and never wakes it for it. A
service without this endpoint answers 404; the *PC* area then says to set it
up again once with the command from there.

## `POST /gaming` (for the web UI)

Request: `GamingRequest`, response: `PcStatus`. On: stop Strata, block
sleeping. Off: start Strata, allow sleeping.

## `GET /strata-log` (for the *Output* tab in the *Strata* area)

The Strata output as Server-Sent Events: what a Strata started by the service
writes to its console. First come the remembered lines (at most 500,
separator lines included), then each new one, plus every change of the
notice. Each message is a `StrataOutputMessage` as JSON in
`data:`; `replace` replaces the last line (progress that overwrites the same
line with `\r` in the console), `notice` says whether there is output
and why not. `not_started_by_agent`: Strata answers but was not started
by the service, so it cannot read its output.

Color codes and other control characters are removed. If the reader cannot
keep up, the stream ends; on the next connect the remembered lines come
again. The proxy opens the stream only while someone has the page open and
the PC is awake anyway.

## Managing Strata (for the *Strata* area)

The service sets up and manages Strata itself. Every job calls fixed
programs with values from Strata's own tables; a request never contains
commands or paths. At most one job runs; while it runs, the service refuses
sleeping and the watchdog stays quiet. Turning gaming on cancels it; a
started download resumes next time.

### `GET /strata`

Response: `StrataReport`, that is `StrataInfo` and `update` next to it. What
is installed, Strata's catalog with a recommendation for the PC's RAM and
VRAM (setup's memory tiers, see [strata-api.md](strata-api.md)),
free space, last benchmark and the running or last job.
Never contains the Strata key. `install` says how Strata is installed:
`terminal` (the Strata Terminal in `strata_home`) or `source` (a Strata folder
from source in `strata_dir`); missing when Strata is not installed and from an
older PC service. `old_strata_folder` says whether an old Strata folder from
source (`strata_dir`) is still there, the one Strata runs from or the one left
after the switch; `terminal_served` whether the Terminal's Strata has answered
`/health` once. Both are `false` from an older PC service. `version` is the folder's short commit, or for the
Terminal its version and build from `strata --version` (`0.2.2 (build
0.1.44.1)`; before the first check only `build 0.1.44.1` from its `VERSION`
file). Along the way it asks whether there is a newer Strata version
(`update_available`): for a Strata folder from source Git, at most once per
hour; for the Terminal `strata update --check --output json`, at most once a
day, which counts a newer engine or newer Terminal files.

If commits are incoming, `update` (`Incoming`) says what they bring; without
an update, before the first check and with an older PC service the field is
missing. `commits` are newest first, at most 200 (`total` counts all).
For a Strata folder from source, `files` comes from `git diff --numstat
HEAD..@{u}`; binary files count 0 lines. For the Terminal, commits and files
come from GitHub's compare API (no token, asked with Windows' `curl.exe`)
between the build's commit (`COMMIT` in its files) and the latest release's
tag; `remote` is Strata's repository. If GitHub does not answer, `update`
is missing and `update_available` stays. The proxy keeps all of this on the NAS; the web UI only gets
short hash, subject and date.

In the catalog, `memory_with_vram` says that `ram_gb` is RAM and VRAM
together (Gyro), and `fits` then counts the biggest card's VRAM too, against
setup's own figure (27.6 GB for Gyro-S). The recommended size always fits,
because setup's low-RAM mode starts it. `vram_gb` is that card's VRAM, missing
without a card. An older PC service leaves these fields out.

Each installed model says with `speed_projection` whether its configuration
loads a control vector (`--control-vector-scaled`, Strata's or a custom one).
An older PC service leaves the field out. This and `context` are read from
what Strata starts with: `args` with `user_args` over it, as Strata's
`engine_args` does.

`user_args` are the run config's engine options set by hand, `[]`
without any, each path cut to its file name as in `config`; missing for a
configuration without `args_written`, whose Strata does not read them.
`started_args` are the args Strata starts with, cut the same way and without
an `--api-key` with its value. An older PC service leaves both out.

`env` is the run config's engine environment (`env`) as name and value,
`{}` without any, each path cut to its file name as in `config` and
without variables whose name says they hold a secret (as `config` leaves
them out). A PC service from before `set_engine_settings` leaves it out;
the web UI offers *Engine settings* only with it.

`context` is the value of `--max-context` in its configuration (tokens),
`null` if the flag is missing or its value is not a number. An older
PC service leaves the field out.

`loop_recovery` is `reasoning_loop_recovery` from its configuration, as
Strata reads it: `recover` (also `true`), `stop` or `off` (if the value is
missing or unknown). An older PC service leaves the field out.

`sampling` says what a request without sampling fields gets: `support`
is `model_card`, `config` or `none` (this Strata reads none and is
greedy), `thinking` and `instruct` are lists of `{"name", "value",
"source"}` with `source` `model_card`, `config` or `chat_settings`, in the
order temperature, top_p, top_k, min_p, presence_penalty,
repetition_penalty, frequency_penalty. An empty list is greedy. From the
chat settings (`strata-<id>.shared-settings.json`, Strata's web app)
temperature, top_p and top_k count; the PC service only reads the file and,
like Strata, takes one that is not valid JSON as empty. `sampling` is
`null` as long as `serve/server.py` is not readable. An older PC service
leaves the field out.

`config` is the model's run config as JSON text, laid out as Strata writes
it, for the optimization prompt: without any field whose name ends in
`key`, `token`, `secret` or `password` (`api_key` among them), and with every
absolute or Windows path (`exe`, the GGUF in `args`, `cwd`, `tokenizer`,
`log`, `lib_dirs`, the `vision` block) cut to its last part; an option keeps
its name (`--x=D:\a\b.bin` gives `--x=b.bin`), a user's own folder gives `~`,
and other text such as a URL stays. So neither a key nor the Windows user
name leaves the PC. Missing if the
config cannot be read. `chat_settings` is the model's Chat settings file
(`strata-<id>.shared-settings.json`) as JSON text, cut the same way, and
missing without the file or if it is not valid JSON. An older PC service
leaves both out.

`load_lines` is the model's latest load, for the optimization prompt: the
Strata output from a start by the PC service until that Strata answers
`/health` with the key of its start (`answered: true`), or until it ends
without answering, e.g. after a crash (`answered: false`). At most the last
100 finished lines (a progress bar's states are left out), with `at`, the
Unix time the load ended. A load the service cuts short by stopping Strata
(gaming mode, an update, a context change) leaves the earlier lines in
place; a later load of the same model replaces them, and other models keep
theirs. Deleting a model deletes its lines. The PC service keeps them in
`load-lines.json` in its state folder, so they survive its restart. Missing
before a load of the model was seen, and from an older PC service.

`benchmarks` holds the last run of each model: `decode_tps` (`chat decode
tok/s`), `prompt_tps` (`4K prompt tok/s`, missing if the benchmark does not
print the line), plus `context`, `speed_projection` and `version` (Strata commit,
or the Terminal's version) at the time of the run. `tool` says what measured
it: `bench_bat` or `strata_bench` (the Terminal's standard benchmark, which
measures differently); older records leave it out and came from `BENCH.bat`.
For `strata_bench`, `decode_tps` is the median decode speed of the short
requests and `prompt_tps` the median prompt speed of the 4K prompts. A model that did not start in the comparison or
reported nothing has `error` instead of the values. `benchmark` stays the last
successful run, for proxies that only know one. An older PC service
leaves out `benchmarks`.

`tunable` says whether `tune` can run for this model; when it is `false`,
`tune_unavailable` says why (a Strata folder from source, or a family the
tuner does not know). `last_tune` is the model's last finished tune: `at`,
`mode`, `use`, `code` (Strata's: `faster`, `already_fastest`, `busy_pc`,
`failed`), `base_tps` and `tuned_tps` (tokens/s before and with the tuned
settings), `gain_pct`, `changed` (each setting the tuner changed: `flag
value`, a switch alone, `NAME=value` for the environment, `per request key
value`), `message`, and `preset` and `preset_name` of the preset Strata saved.
The PC service keeps it in `strata-tunes.json` in its state folder; deleting
the model forgets it. All three are missing from an older PC service.

`presets` is what Strata says about the model's presets for its current
run config (`strata presets status`): `latest` is the active preset, else
the newest, with `id`, `name`, `active`, `gain_pct`, `settings` (written as
`changed` in `last_tune`), `decode_tps` and `base_tps` (answer tokens/s with
it and with the model's own settings, as the tuner measured) and, for the active one,
`state` (`fresh`, `soft`: re-check offered, `hard`: not applied) and
`reasons` in Strata's words. `other_context` is `true` when the model's last
tune saved a preset that is not among them: it was made for other choices,
such as another context. The PC service reads it in the background when
`/strata` is asked for, outside jobs and gaming mode, one model after the
other, and again when the run config, the last tune or the Terminal's build
changed, after `apply_preset`, `undo_preset` and `preset_off`, and every 30 minutes (Strata's
verdict follows driver and engine changes). Missing without a Terminal,
before the first read and from an older PC service.

`machine` (`MachineFacts`) says what the PC is, for the optimization prompt.
The PC service reads it with the first `GET /strata` and keeps it; a read
without a graphics card or the drive (a driver not ready yet after boot) is
read again after 10 minutes:
- `ram_sticks`: one entry per filled slot with `size_bytes`, `kind` (e.g.
  `DDR5`), `rated_mts` and `configured_mts` (the speed it runs at), from the
  SMBIOS memory devices; `ram_slots` counts all slots.
- `gpus`: every card with VRAM of its own, the most first: `name`,
  `vram_bytes`, `driver_version` as Windows shows it, and the PCIe link the
  card can do (`pcie_max_gen`, `pcie_max_lanes`) and has now (`pcie_gen`,
  `pcie_lanes`). A slower slot shows only in the link now, and so does an idle
  card, which runs a lower generation. Cards are matched to their link by bus,
  device and function; a second PCI segment is not told apart.
- `cpu`: `name`, `cores`, `threads` (over all processor groups), `base_mhz`
  (Windows' rated clock, not the boost), `avx2`, `avx512`.
- `windows`: `edition` (e.g. `Windows 11 Pro`), `version` (`24H2`), `build`
  (`26100.4652`).
- `model_drive`: the drive holding the Strata data folder, `nvme`, `ssd` or
  `hdd`; a string, so a later kind does not break older proxies.

Whatever the PC service cannot read is `null` or an empty list. An older PC
service leaves out `machine`.

`options` (`StrataOptions`) says what the installed Strata Terminal offers to
tune, for the optimization prompt. The PC service reads it in the background
when `/strata` is asked for, outside jobs and gaming mode, with the Terminal's
own Python (see [strata-api.md](strata-api.md)), and again only when the
Terminal's build or the engine the run config names (`exe`) changes. A read
that failed, took longer than a minute or lacks the tuner's table or (with a
model) the engine's help is tried again after 10 minutes. An answer during the
read is still without it. An engine replaced under the same `exe` (a
`component:` reference) is read again with the next Terminal build.
- `version`: the build of the Terminal files it was read from (their
  `VERSION`); the field is missing while another build runs.
- `tuner`: each engine option of the tuner's table, sorted by `flag`:
  `takes_value` (`false` for a switch), `min` and `max` of the searched range,
  `choices` and `answer` (`exact`, `placement`, `quality` or
  `nonrepeatable`). Values are text as Strata writes them.
- `never`: what the tuner never picks: `setting` (an engine flag, or an
  environment variable when `env`), `values` (all when empty), `test` and
  `limit` (Strata's name for the check on the value and its bound), `when`
  (PC traits such as `hybrid_cpu`), `families` (Strata's model families) and
  `reason`.
- `tries`: the tuner's own tries in Strata's order: `label`, `answer`, `args`
  (the engine arguments it sets; empty for one that sets something else),
  `env` (the engine environment variables it sets, name to value, `null` for
  one it removes; empty from an older PC service) and `families`.
- `engine_help`: what the engine of the active model's run config (else the
  first model's) prints for `--help`; `null` without a model or when it does
  not run.

Missing without a Terminal (a Strata folder from source), before the first
read and from an older PC service.

### What the PC service keeps in the model configurations

When the PC service starts, and after `install`, `add_model` and `update`, it
sets `"reasoning_loop_recovery": "recover"` in every `strata-<id>.json` (see
[strata-api.md](strata-api.md)) and overwrites any other value. It sets the
`sampling` block to the model card's thinking set if the installed Strata
only knows the block; if it knows the model card or does not know the
block, it removes it. A hand-written block is replaced in the process.
If `serve/server.py` is not readable, the block stays as it is. It makes its
changes at startup, before it accepts jobs. Everything else in the
configuration stays, including its modification time. A configuration it
cannot read or write is noted in its log and stays as it is. If the
configuration of the active model changes while Strata is running, it
restarts Strata as soon as Strata is not handling a request; after `install`
and `update` Strata starts with it anyway.

### `POST /strata/jobs`

Request: `StrataJobRequest`, one case per kind. Response `202` with the
job (`StrataJob`). `400` (`Refused`) for values outside the catalog,
`409` if it is not possible right now (gaming on, a job is running, too
little space, deleting the active model).

| `kind` | Fields | What happens |
| --- | --- | --- |
| `install` | `family`, `size`, `context`, `api_key`, optional `experimental_speed_projection` | the Strata Terminal with Strata's `install.ps1` (if it is not there yet), the engine for this PC with `strata update`, setup without questions in its install mode, then serves the model with `strata serve`; a Strata folder from source keeps its own setup (`START-HERE.bat`) |
| `add_model` | `family`, `size`, `context`, optional `api_key` and `experimental_speed_projection` | setup with `--setup` (for the Strata Terminal in its install mode) and the key Strata runs with, the active model stays active |
| `activate` | `model` | waits until Strata is not handling a request, restarts with the other run config (`strata serve --profile`) or start script |
| `delete` | `model` | deletes this model's model folder, pack, configuration, start script and log |
| `update` | – | Terminal: `strata update --output json` (engine and Terminal files, each after a test start) with Strata stopped, restarts; a part that is not in place fails the job with Strata's reason. Folder from source: `git pull --ff-only`, then `START-HERE.bat --update` with Strata stopped, restarts |
| `benchmark` | – | Terminal: `strata bench run --url <strata_url> --out <new folder> --output json` with the Strata key in `STRATA_API_KEY`. Folder from source: `BENCH.bat --config <active model>`. Keeps tokens/s with model, date and tool |
| `compare_models` | – | `benchmark` for each installed model in turn, each as it is set up; activates each model for this and at the end (also after cancelling) the previously active one again |
| `switch_to_terminal` | – | only while Strata runs from an old Strata folder: the Strata Terminal and its engine as with `install`, the Terminal's setup in its install mode for the active model with its context and speed projection on the same Strata data folder (no new download of files already there), its Chat settings copied; once Strata is idle, the old Strata stops and `strata serve` starts with the new run config. Until the old Strata stops, `GET /strata` still shows the old folder (`install: source`), also after a restart of the service, which undoes the unfinished switch. Until the Terminal's Strata answers on `/health` (within `context_start_secs`), a failure or a cancel removes the Terminal's run config, restores the managed start and starts the old Strata again |
| `remove_old_strata_folder` | – | deletes `strata_dir`, leaving the Strata data folder, the Terminal's folders and the service's state folder; `409` while Strata runs from the old folder, without one, and before the Terminal's Strata has answered once (`terminal_served`) |
| `set_key` | `api_key` | waits until Strata is not handling a request and restarts it, which brings a new key |
| `set_speed_projection` | `model`, `on` | turns speed projection on or off in this model's configuration, without setup; for the active model it waits until Strata is not handling a request, and restarts it |
| `set_context` | `model`, `context` | sets the context in this model's configuration, without setup; for the active model it waits until Strata is not handling a request, and restarts it |
| `set_user_args` | `model`, `user_args` | replaces the engine options set by hand in this model's configuration, without setup; for the active model it waits until Strata is not handling a request, and restarts it |
| `set_engine_settings` | `model`, `env`, `user_args` (optional) | replaces the engine environment and, if given, the engine options set by hand, as `set_user_args` does; one restart for both |
| `apply_preset` | `model`, `preset` | Terminal only: `strata presets apply <preset>`; the active model restarts once Strata is not handling a request |
| `undo_preset` | `model`, `preset` | Terminal only: `strata presets undo <preset>`, back to the preset active before (or the standard settings); the active model restarts as with `apply_preset` |
| `preset_off` | `model` | Terminal only: `strata presets off <run config>`, back to the standard settings; the preset stays saved and can be applied again; the active model restarts as with `apply_preset` |
| `tune` | `model`, `mode` (`quick`, `thorough`), `use` (`coding`, `chat`) | Terminal only: waits until Strata is not handling a request, stops it, runs `strata tune auto <run config> --goal speed` and starts Strata again; the result goes to `last_tune`, the active settings stay as they were |

`tune` takes about 20 minutes (`quick`) or up to two hours (`thorough`).
Strata's tuner writes its progress to `strata-tune-status.json` in the
PC service's state folder; the job's `step`, `percent` and `eta_secs` follow it
every second (`percent` stays below 100 until the tuner ends). Each new step
adds a line to the job's `lines` with the time since the tune started
(`[10m] Trying: …`, from an hour on `[1h 05m] …`); the tuner's JSON answer is
no line. The job's `speeds` lists the measurements so far as Strata's status
file has them (`{label, tok_s}`, `base` for the model's own settings first);
it is left out when empty, and only a tune fills it. Strata activates
the preset it saves for faster settings; the PC service undoes that right away
(`strata presets undo <id>`), so the model keeps running as before and the
tuned settings wait in the preset; also after a cancel. If Strata refuses
the undo, the job fails and says the tuned settings stay active, and the
result is kept. A cancelled tune keeps nothing. While a tune runs, the proxy
does not trust Strata's `/health`: the tuner's engine may answer there. A source
folder or a family the tuner does not know is a `409` with the reason of
`tune_unavailable`.

The service ignores `api_key` in every job: the key comes with the next start
of Strata (see `GET /strata/key`). The proxy still sends a random one with
`install`, because a service from before refuses an empty key. Setup gets the key Strata runs with, if
there is one. The job's output shows `***` instead of the key.

`experimental_speed_projection` (`false` if not given) passes setup
`--experimental-speed-projection on` instead of `off` for Qwen and Coder. For
other families, `true` is a `400`. The proxy sends the field only when it is
checked (see "Older PC services").

`set_speed_projection` only changes `args` in `strata-<id>.json`, and
`args_written` the same way if the configuration has it, so that Strata does
not take the change for one set by hand; the four flags and `--control-vector`
(a vector of the user's own, which Strata keeps with them) leave `user_args`,
which Strata puts over `args` (an empty `user_args` goes). A configuration
without these keys gets none. `on: true`
removes setup's flags and appends them again, with Strata's vector from the
Strata folder (see [strata-api.md](strata-api.md)); if the vector is missing,
the job fails and the configuration stays. `on: false` removes the
four flags with their values, including a custom vector. Families other
than Qwen and Coder (also Gyro) are a `400`, a model from the catalog that is
not installed is a `409`, as with `activate`. The configuration's modification
time stays, because without its own start script Strata starts the
most recently modified one.

`set_context` only sets the value of `--max-context` in `args` (if the flag
is missing, it appends it) and removes `--rope-scaling` and `--rope-scale` with
their values; `args_written` and `user_args` as with `set_speed_projection`,
with `--max-context`, `--rope-scaling` and `--rope-scale`. It only takes
contexts from the catalog up to 262144, the
trained length
(`MAX_SWITCHED_CONTEXT`): above that the model needs rope scaling, which only
setup sets up (`400`). What setup derived from the old context
(KV streaming with `--kv-resident`, `--kv`, parallel slots, calibration)
stays. A model from the catalog that is not installed is a
`409`. The modification time stays as with `set_speed_projection`.

`set_user_args` replaces `user_args` with the list as given (an empty
list removes the key); `args`, `args_written` and the modification time
stay. The list comes as `GET /strata` shows it: a group (an option and its
values) equal to how one shows there keeps that one's whole value, so a
cut path stays whole; each one serves once. An `--api-key` there, which
`GET /strata` never shows, stays. A list that starts with a value is a `400`, as is a group that adds
or changes `--max-context`, the rope flags, the speed projection flags
with `--control-vector`, `--host`, `--port` or `--api-key` (also as
`--flag=value`): context and speed projection have their own jobs, and the
proxy reaches Strata through the others. Such a group Strata put there
itself may come back unchanged. A configuration without `args_written`
(its Strata reads no `user_args`) and a model from the catalog that is not
installed are a `409`. Whether Strata knows an option is not checked: the
restart below shows it.

`set_engine_settings` replaces `env` with the names and values as given (an
empty object removes the key) and, if `user_args` is there, `user_args` as
`set_user_args` does, with its refusals; without it `user_args` stays. The
values come as `GET /strata` shows them: a value equal to how the old one
of that name shows keeps the old one, so a cut path stays whole. A variable
with a secret, which never shows, stays. A name that is not letters, digits
and `_` (not starting with a digit) is a `400`, as are `PATH`,
`CUDA_VISIBLE_DEVICES`, `HIP_VISIBLE_DEVICES` and `CUDA_DEVICE_ORDER`:
Strata puts `env` over the GPU choice's variables, so these would override
it. Such a variable already there may come back unchanged. `user_args`
for a configuration without `args_written` is a `409`; `env` is taken from
every Strata.

For the active model, the job waits after the restart until Strata answers on
`/health`. If Strata exits before that or does not answer within
`context_start_secs` (default 600), it writes the old `args`,
`args_written`, `user_args` and `env` back,
restarts Strata with them and fails; the last lines of the
Strata output are then in the job's `lines`. If the wait is
cancelled, the old `args` are restored as well. A job does not start in
gaming mode (`409`, as every job); if gaming mode comes on while the job
waits, Strata does not restart, and there is no waiting.

### `GET /strata/job`

The running or last job (`StrataJob`, `null` without one): `state` (`running`, `done`, `failed`,
`cancelled`, `interrupted` after a restart), step, progress with
bytes, speed and remaining time, the last 20 output lines, `error`, `result`.

### `POST /strata/job/cancel`

Cancels the running job (`202` with the job), otherwise `409`
(`Refused`).
