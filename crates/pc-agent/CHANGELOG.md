# PC service changelog

What each version brings, newest first. The web UI's PC service card shows
the versions an update brings; each line is written for the user. Rules:
`docs/pc-service.md`.

## 0.2.1

- Relay mode switches on or off in the web UI under Strata → Connection; Strata restarts with it once it has finished its requests.

## 0.2.0

- Lists the models to install from Strata's own catalog, with the memory each one needs.
- Installed models keep working when Strata's catalog cannot be read.
- Switches Pictures on or off for an installed model with one setup run.
- Shows and saves each installed model's Engine options and Engine environment in Engine settings, with one restart.
- Applies, undoes and turns off a preset; a tune shows its steps and measured speeds, and an engine download its progress.
- Runs a Strata set up from the web UI with the Strata Terminal (`strata serve`).
- Relay mode: with `relay = true` in the TOML, Strata listens on the PC only and the proxy reaches it through the PC service.
