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

## Installing

1. In the NAS web UI, open the **PC** area and
   press **Create install command**.
2. Paste the command into a PowerShell on the PC, e.g.:
   ```powershell
   irm http://192.168.1.10:8090/admin/agent/install.ps1?code=K7F2Q9AB | iex
   ```
   Windows asks once for administrator rights. The script downloads
   `strata-pc-agent.exe` from the NAS to `%LOCALAPPDATA%\StrataHome`, registers the PC
   with the NAS, makes it start at login, opens port 8091 only for
   the NAS and creates the desktop shortcut **Gaming an-aus**. The NAS fills in the token,
   `PC_AGENT_URL` and the Strata key itself.
3. The **Strata** area is where you set the start script and Strata programs. Until then
   the service runs, but does not start Strata itself.

The code in the command is valid for 15 minutes and for one PC; a new command makes
the old one invalid. On a PC where the service is already running, a
new command replaces the .exe and registers the PC again (new token, e.g. after
token or address were reset on the NAS); the rest of the configuration
stays.

**Updating:** The **PC** area shows the PC service version and
**Update available** as soon as the NAS image contains a different one (after a
new image). The **Update to …** button works only when the PC is awake and never wakes it.
The service downloads the .exe from the NAS, puts it in the same place (task and
shortcut stay valid), starts it and exits. As long as Strata is handling a
request or a Strata job is running, it refuses; Strata may be
stopped along with the service when it restarts and then starts again. If
the update fails, the old service keeps running and the **PC** area names the
reason. A service set up before the update button existed needs
the command from above once.

**Removing:** After *Create install command*, the area also shows the command for this
(`irm http://<NAS>:<Port>/admin/agent/install.ps1?uninstall | iex`). It removes
the task, firewall rule, shortcut and folder. Afterwards, on the NAS, reset
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
it stores the start script, Strata folder and Strata key in
`strata-managed.json` next to the TOML. These values take precedence over `strata_start`,
`strata_args`, `strata_processes` and `strata_api_key` from the TOML; the TOML
itself stays unchanged. Without this file the TOML applies as before. Anyone who
wants to set up by hand again deletes the file and restarts the service.

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
