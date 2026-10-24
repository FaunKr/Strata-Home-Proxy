# PC service and gaming switch

`strata-pc-agent.exe` runs invisibly in the background as soon as you
log in. It

- puts the PC into standby when the NAS asks for it and nothing
  against it: gaming mode on, someone used the mouse or keyboard in the
  last 10 min, or Strata is computing;
- switches gaming mode: **on** stops Strata and blocks sleep,
  **off** starts Strata again. The state survives a restart;
- starts Strata when it is not running, and restarts it when it does not answer
  on `/health` for 60 s (e.g. after standby). After every start
  it waits 4 min for the model to load.

Two web UI buttons go past these rules; neither wakes the PC:

- **Force sleep** appears on the Overview under a refused sleep request
  (*Sleep now*) and in gaming mode. After a confirmation the PC sleeps anyway,
  even if someone just used the mouse or keyboard, Strata is processing a
  request or gaming mode is on (it stays on). A running Strata job is
  cancelled first; downloaded parts are kept. Only a PC service update
  refuses.
- **Force restart Strata** sits in the Strata area while the PC is awake and
  gaming mode is off, also when Strata is loading or not answering. After a
  confirmation the PC service cancels a running Strata job, kills Strata hard
  (a hand-started one too) and starts its own Strata again right away, without
  waiting for the watchdog. Without a start script or with
  `auto_start = false`, Strata stays stopped.

Both wait at most `cancel_wait_secs` (60 s) for the cancelled job to end and
then carry on anyway.

## Installing

1. In the NAS web UI, open the **PC** area and
   press **Create install command**.
2. Paste the command into a PowerShell on the PC, e.g.:
   ```powershell
   irm http://192.168.1.10:8090/admin/agent/install.ps1?code=K7F2Q9AB | iex
   ```
   Windows asks once for administrator rights. The script downloads
   `strata-pc-agent.exe` from the NAS to `%LOCALAPPDATA%\StrataHome`, registers the PC
   with the NAS, makes it start at login, opens port 8091 and Strata's port
   (the one in the proxy's Strata address, 8080 without one; firewall rule
   *Strata from the NAS*) only for the NAS and creates the desktop shortcut **Gaming an-aus**. The NAS fills in the token
   and `PC_AGENT_URL` itself; the service makes the Strata key at every start of Strata.
3. The **Strata** area is where you set the start script and Strata programs. Until then
   the service runs, but does not start Strata itself.

The code in the command is valid for 15 minutes and for one PC; a new command makes
the old one invalid. On a PC where the service is already running, a
new command replaces the .exe and registers the PC again (new token, e.g. after
token or address were reset on the NAS); the rest of the configuration
stays.

**Updating:** The **PC** area shows the PC service version and
**Update available** as soon as the NAS image contains a different one (after a
new image). The proxy keeps the last reported version in `DATA_DIR`, so both
show after a restart too, while the PC sleeps. On an awake PC the button reads
**Update to …**; on a sleeping one **Wake PC and update to …**: it wakes the
PC until the service answers, then updates. If the service already runs the
NAS's version after the wake, nothing is updated. The PC sleeps again after
the idle timeout.
The service downloads the .exe from the NAS, stops Strata (a hand-started one
too), puts the new .exe in the same place (task and shortcut stay valid),
starts it and exits. The new service starts Strata again right away if it was
running before. The update goes through even when the page that pressed the
button closes. Should the old service still hold the port 5 s after the new one
started, the new one ends it; a new service that cannot start writes why to its
log instead of a message box. As long as Strata is handling a request or a
Strata job is running, it refuses; so it does when Strata still answers 10 s
after the stop.
If the update fails, the old service keeps running, starts the Strata it
stopped again, and the **PC** area names the reason. A service set up before the update button existed needs
the command from above once.

**What's new:** While an update is available, the PC service card lists what
it brings, from the service's changelog (`crates/pc-agent/CHANGELOG.md`, built
into the proxy): the versions above the installed one up to the one on the
NAS, newest first, two open and older ones behind *Show older changes*. A
service that reports 0.1.0 or no version gets every version. An update that
changes only the build says *Only internal changes*; when the NAS carries an
older version than the PC runs, nothing is listed. The UI check swaps in
another changelog with `PC_AGENT_CHANGELOG`.

The version follows semver under 0.x. A PR that changes what the service does
for the user adds a `## x.y.z` section with short `- ` lines written for the
user and sets the same version in `crates/pc-agent/Cargo.toml`; refactors and
test-only changes add none. The minor number goes up when the user has to act
on the PC (run the install command again, change the config, anything the
update button can't do): the card marks a version whose minor number is above
that of the version before it in the changelog *Needs a step on the PC*, and
its lines say what to do. The first version, 0.2.0, needs none. A program on
the NAS whose version is no `x.y.z` counts as no program. The patch number goes up for features and fixes.
A unit test fails when the newest section and the crate version differ, the
versions do not descend, or a section is empty.

**Removing:** After *Create install command*, the area also shows the command for this
(`irm http://<NAS>:<Port>/admin/agent/install.ps1?uninstall | iex`). It removes
the task, both firewall rules, shortcut and folder. Afterwards, on the NAS, reset
`PC_AGENT_URL` and `PC_AGENT_TOKEN` in the *PC* area with *Reset*.

The service writes to `%LOCALAPPDATA%\StrataHome\strata-pc-agent.log`.

## Strata output

The service starts Strata without a console window and reads its output
itself. The lines of the running Strata are in
`%LOCALAPPDATA%\StrataHome\strata.log` (new on every start), the web UI shows the last
500 live in the *Output* tab of the *Strata* area
(`/admin/strata/output`). Python programs get `PYTHONUNBUFFERED=1`, so that the
output comes right away and not in bursts. A Strata that you start yourself by
double-click runs in its own window; the service does not see its
output. The same applies if the start script in turn starts Strata with `start` in
its own window. The Strata key appears in the output
as `***`.

Strata writes its output into the service. If the service is stopped or
restarted while Strata is running, Strata's output goes nowhere; if
Strata crashes because of it, the restarted service starts it again.

## Managed values

If the service sets up Strata itself (*Strata* area of the web UI),
it stores what starts Strata and what belongs to it in
`strata-managed.json` next to the TOML. For the Strata Terminal that is
`strata.cmd` with `serve --profile <run config> --host 0.0.0.0 --port 8080`
(`--host 127.0.0.1` in relay mode, switched under *Strata → Connection* or
`relay = true` in the TOML; the web UI's switch, kept here as `relay`, takes
precedence. The service sets the host at every start, so the mode counts
from the next start; the switch restarts Strata once it is idle) and
the folder `strata_home` (default `%LOCALAPPDATA%\Strata`); for a Strata folder
from source, its start script and the folder `strata_dir`. These values take precedence over `strata_start`,
`strata_args` and `strata_processes` from the TOML; the TOML
itself stays unchanged. Without this file the TOML applies as before. Anyone who
wants to set up by hand again deletes the file and restarts the service.

## Updating Strata

On the Strata Terminal, **Update Strata** (tab *Version & benchmark*) stops
Strata, runs `strata update` (the engine and newer Terminal files, each only
after a test start) and starts Strata again; if a part fails, the old one
stays and the job names Strata's reason. Once a day a visit of the page asks
`strata update --check`, and when something is newer the service asks GitHub
with Windows' own `curl.exe` what the commits since the installed build bring.
**Run benchmark** runs Strata's standard benchmark (`strata bench run`); its
last report stays in `strata-bench` next to the TOML. Results from the old
Strata folder's `BENCH.bat` are marked as not comparable.

## Switching to the Strata Terminal

A PC that still runs Strata from an old Strata folder from source keeps
running it after a service update. Tab *Version & benchmark* then offers
**Switch to Strata Terminal** (with a question first; a sleeping PC wakes
first). The job installs the Strata Terminal with Strata's `install.ps1` and
the engine for the PC with `strata update`, then runs the Terminal's setup
in its install mode for the active model with its context and speed
projection and the same Strata data folder: model files that are already
there are not downloaded again. The model's Chat settings come along, the
service sets loop recovery and sampling in the new run config as in every
other. Once Strata has no request at work, the service stops the old Strata
and starts `strata serve` with the new run config.

Until the old Strata stops, the old Strata folder stays the service's Strata
(`strata-switch.json` next to the TOML holds the start from before); a
service that ends meanwhile undoes the switch at its next start. Until the
Terminal's Strata answers on `/health`, any failure (no engine for the card,
a failed setup, Strata not answering within `context_start_secs`), *Cancel*
and gaming mode (which cancels the job) take the Terminal's run config away
and bring the old start back: the old Strata folder serves again, and the
job names the reason.

After the first answer of the Terminal's Strata (the service keeps
`strata-terminal-served` next to the TOML), **Remove old Strata folder**
deletes `strata_dir`, after a question. The Strata data folder, the Terminal's
folders and the service's own folder stay, also where they lie inside
`strata_dir`.

## Notes

- The service runs in the user account, not as a Windows service: only this way does it see
  mouse and keyboard and can start Strata in your session.
  After a restart, someone therefore has to log in (or Windows
  automatic login is set up).
- With the RX 6800 XT the first request after standby hangs if Strata
  stays loaded; after a restart of Strata it works. That is why
  `fresh_strata_after_standby = true` is in the template: Strata is stopped before
  standby and restarted afterwards. Wake-up then takes 1–3 min.
- A Strata that you started yourself is left alone as long as it answers.
