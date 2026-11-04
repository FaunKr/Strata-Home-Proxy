# Development

## Layout

- `crates/nas-proxy`: the proxy on the NAS.
- `crates/admin-ui`: the web UI (Leptos), served by the proxy under `/admin/`.
- `crates/pc-agent`: the PC service (`strata-pc-agent.exe`).
- `crates/pc-protocol`: what the PC service takes and answers over HTTP, as
  types shared by both sides.
- `tools/code-bench`: the runner of the code benchmark (container `code-bench`).
- `tools/local-agent`: the skill `local-agent`.
- `deploy`: Docker Compose, the TrueNAS app, the TrueNAS update script and the
  PC service's install script.

## Building and testing

```sh
cargo nextest run  # starts a simulated Strata, PC service and wake packet receiver
scripts/check.sh   # all CI checks: fmt, clippy (also wasm), tests
```

Tests run with [cargo-nextest](https://nexte.st), at least the version
`.config/nextest.toml` pins; `scripts/check.sh` prints the install command
when it is missing. `scripts/check.sh <crate>...` checks only those crates.

The web UI needs, for the browser part,
`rustup target add wasm32-unknown-unknown` and
`cargo install cargo-leptos --locked`. Then `cargo leptos build` builds the
proxy and the web UI, and `cargo leptos watch` starts both and rebuilds on
changes.

`scripts/ui-check.sh` checks the web UI in a real browser against a simulated
PC and power plug (needs Node and Playwright with Chromium); `SHOTS=1` also
saves screenshots to `target/ui-check/`.

To build the image yourself instead of pulling it, replace the `image:` line
in [deploy/docker-compose.yml](../deploy/docker-compose.yml) with `build: ..`.

## Further reading

- [CODING_STANDARDS.md](../CODING_STANDARDS.md): rules for review.
- [GLOSSARY.md](../GLOSSARY.md): the terms used in the web UI, docs and code.
- [CI](ci.md): what each workflow runs, and how releases are made.
- [ADRs](adr/): decisions already taken.
- [Strata API](strata-api.md): what the proxy and the PC service use from Strata.
- [PC service API](pc-agent-api.md): the interface between proxy and PC service.
