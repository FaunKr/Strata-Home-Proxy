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
  (`update`, `benchmark`, `compare_models`) ignore extra fields.

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

`strata_job` is `true` while a Strata job is running (see below).

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
compares it only for a change. The field is missing while no model
configuration holds a key, and from services before the key per start.

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
(`/admin/agent/strata-pc-agent.exe`), renames the running .exe to
`strata-pc-agent.exe.alt`, puts the new one in its place and starts it
with the same arguments. The response comes after that; 1 s later the old
service exits, the new one waits for the port until then and then deletes the
`.alt`.

- `202` (`Answer`): the new .exe is about to run.
  From the download until the end, the service refuses Strata jobs and
  sleeping.
- `409` (`Refused` with a reason): not now (Strata is handling a request, a
  Strata job or an update is running) or set up without `nas_url`.
- `500` (`Refused`): download or replacement failed; the old service keeps
  running, nothing has changed.

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
is installed, Strata's catalog with a recommendation for the PC's RAM,
free space, last benchmark and the running or last job.
Never contains the Strata key. Along the way it asks Git at most once per
hour whether there is a newer Strata version (`update_available`).

If commits are incoming, `update` (`Incoming`) says what they bring; without
an update, before the first check and with an older PC service the field is
missing. `commits` are newest first, at most 200 (`total` counts all).
`files` comes from `git diff --numstat HEAD..@{u}`; binary files count 0
lines. The proxy keeps all of this on the NAS; the web UI only gets
short hash, subject and date.

Each installed model says with `speed_projection` whether its configuration
loads a control vector (`--control-vector-scaled`, Strata's or a custom one).
An older PC service leaves the field out.

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

`benchmarks` holds the last run of each model: `decode_tps` (`chat decode
tok/s`), `prompt_tps` (`4K prompt tok/s`, missing if the benchmark does not
print the line), plus `context`, `speed_projection` and `version` (Strata commit)
at the time of the run. A model that did not start in the comparison or
reported nothing has `error` instead of the values. `benchmark` stays the last
successful run, for proxies that only know one. An older PC service
leaves out `benchmarks`.

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
| `install` | `family`, `size`, `context`, `api_key`, optional `experimental_speed_projection` | Git (via winget, if needed), `git clone`, setup without questions, starts Strata |
| `add_model` | `family`, `size`, `context`, optional `api_key` and `experimental_speed_projection` | setup with `--setup` and the key Strata runs with, the active model stays active |
| `activate` | `model` | waits until Strata is not handling a request, restarts with the other start script |
| `delete` | `model` | deletes this model's model folder, pack, configuration, start script and log |
| `update` | – | `git pull --ff-only`, then `START-HERE.bat --update` with Strata stopped, restarts |
| `benchmark` | – | `BENCH.bat --config <active model>`, keeps tokens/s with model and date |
| `compare_models` | – | `benchmark` for each installed model in turn, each as it is set up; activates each model for this and at the end (also after cancelling) the previously active one again |
| `set_key` | `api_key` | waits until Strata is not handling a request and restarts it, which brings a new key |
| `set_speed_projection` | `model`, `on` | turns speed projection on or off in this model's configuration, without setup; for the active model it waits until Strata is not handling a request, and restarts it |
| `set_context` | `model`, `context` | sets the context in this model's configuration, without setup; for the active model it waits until Strata is not handling a request, and restarts it |

The service ignores `api_key` in every job: the key comes with the next start
of Strata (see `GET /strata/key`). An older proxy still sends one; the field
stays so its requests still parse. Setup gets the key Strata runs with, if
there is one. The job's output shows `***` instead of the key.

`experimental_speed_projection` (`false` if not given) passes setup
`--experimental-speed-projection on` instead of `off` for Qwen and Coder. For
other families, `true` is a `400`. The proxy sends the field only when it is
checked (see "Older PC services").

`set_speed_projection` only changes `args` in `strata-<id>.json`. `on: true`
removes setup's flags and appends them again, with Strata's vector from the
Strata folder (see [strata-api.md](strata-api.md)); if the vector is missing,
the job fails and the configuration stays. `on: false` removes the
four flags with their values, including a custom vector. Families other
than Qwen and Coder are a `400`, a model from the catalog that is
not installed is a `409`, as with `activate`. The configuration's modification
time stays, because without its own start script Strata starts the
most recently modified one.

`set_context` only sets the value of `--max-context` in `args` (if the flag
is missing, it appends it) and removes `--rope-scaling` and `--rope-scale` with
their values. It only takes contexts from the catalog up to 262144, the
trained length
(`MAX_SWITCHED_CONTEXT`): above that the model needs rope scaling, which only
setup sets up (`400`). What setup derived from the old context
(KV streaming with `--kv-resident`, `--kv`, parallel slots, calibration)
stays. A model from the catalog that is not installed is a
`409`. The modification time stays as with `set_speed_projection`.

For the active model, the job waits after the restart until Strata answers on
`/health`. If Strata exits before that or does not answer within
`context_start_secs` (default 600), it writes the old `args` back,
restarts Strata with them and fails; the last lines of the
Strata output are then in the job's `lines`. If the wait is
cancelled, the old `args` are restored as well. In gaming mode Strata
does not restart, and there is no waiting.

### `GET /strata/job`

The running or last job (`StrataJob`, `null` without one): `state` (`running`, `done`, `failed`,
`cancelled`, `interrupted` after a restart), step, progress with
bytes, speed and remaining time, the last 20 output lines, `error`, `result`.

### `POST /strata/job/cancel`

Cancels the running job (`202` with the job), otherwise `409`
(`Refused`).
