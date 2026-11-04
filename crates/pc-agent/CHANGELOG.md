# PC service changelog

What each version brings, newest first. The web UI's PC service card shows
the versions an update brings; each line is written for the user. Rules:
`docs/pc-service.md`.

## 1.5.0

- In relay mode, tells the proxy how many bytes of each request reached Strata, so the web UI's Transport check shows where a broken request lost them.

## 1.4.1

- Stopping Strata, *Force restart Strata* and updates of the PC service work again after Strata was stopped cleanly while an expert profile learned.

## 1.4.0

- Restarts the PC when the proxy asks for a Planned restart (*Restart at next sleep* on the Overview), without installing Windows updates.

## 1.3.1

- Stopping Strata cleanly while an expert profile learns no longer ends the PC service too.

## 1.3.0

- Learned expert profiles: each model can learn expert profiles of its own from your requests, switch between them and back to Standard, all from Engine settings. Benchmarks never teach them.
- While an expert profile learns, Strata stops cleanly, so the engine saves what it learned before it ends.

## 1.2.0

- Reports the graphics card's power in watts on AMD cards, from the Adrenalin driver, so the Overview and the metrics endpoint show it.

## 1.1.0

- Tells the web UI Strata's speed switches and the documented engine environment variables that suit this PC, so the optimization prompt can suggest them.

## 1.0.1

- Models start again after a Strata update deleted an older Strata folder their settings pointed into: the PC service sets them up again by itself.

## 1.0.0

- The first stable release, together with Strata Home Proxy 1.0.0. Nothing changes on the PC.

## 0.2.2

- Installs the Python package jsonschema into Strata's Python, so Strata checks JSON answers against the schema a request asks for.

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
