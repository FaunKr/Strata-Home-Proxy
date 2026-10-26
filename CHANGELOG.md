# Changelog

What each release of Strata Home Proxy brings, newest first. The version is
the one in `[workspace.package]` of `Cargo.toml`, and the web UI shows it
under *System*. A release publishes the image as `:x.y.z`, `:x.y` and `:x`
and makes a GitHub release with its section (`docs/ci.md`, *Releases*). The
PC service has its own version and changelog (`crates/pc-agent/CHANGELOG.md`).

Major: a change that needs a step by hand on the NAS or the PC (a setting
removed or renamed, a new required environment variable, a data layout the
proxy can't migrate). Minor: new features. Patch: fixes.

## 1.0.0

The first release: in daily use on the NAS and the PC.

- One address on the NAS for every client: requests in OpenAI and Anthropic format go to Strata on the PC and stream back live, with a ping every 15 s while Strata computes.
- Wakes the PC by Wake on LAN when a request comes in, and sends it to sleep after 15 minutes without requests; several requests share one wake-up.
- Gaming mode: the PC is neither woken nor sent to sleep, and clients get `503` with `x-strata-proxy-status: gaming` right away.
- API keys per client, created and revoked in the web UI; Strata only ever sees its own key, which the PC service makes new at every start of Strata.
- The list of models answers from the last saved list while the PC sleeps, without waking it; *Hide model name* gives apps one stable model name.
- PC service for Windows, installed with one command from the web UI: gaming switch, sleep requests, Strata setup, models, engine settings, tunes, relay mode and updates from the NAS.
- Web UI in English with the areas Overview, PC, Strata, Access and System: live state, setup checklist, settings, history with energy estimate (also from a power plug), benchmarks, code benchmark and request log.
- Automatic updates on TrueNAS with backup and rollback (`deploy/truenas-auto-update.sh`).
- `local-agent` hands coding tasks from Claude Code to Strata through the proxy, and postpones a run while the PC is in gaming mode.
