## Working in the repo

- PRs go against `main`. Start implementing from a freshly fetched `origin/main`: the session's clone may be hours behind.
- Before pushing, run `scripts/check.sh` and push only when its last line reads `check.sh: PASSED`: the same checks as the CI job `test`. To iterate, `scripts/check.sh <crate>…` checks only those crates.
- Before you suggest merging a PR: `/code-review` over its diff.
- Bringing a PR to merge: read `.claude/skills/steward/SKILL.md` when opening a PR, before waiting on CI and before every merge, also after the context was compacted. Session skill lists may not show it.
- Check UI changes in the browser: `scripts/ui-check.sh` (`SHOTS=1` for screenshots).
- New proxy tests use the harness with a simulated PC and Strata in `crates/nas-proxy/tests/common`; a proxy restart over the same `DATA_DIR` is `restarted` there, not a second `setup`.
- What the PC service takes and answers over HTTP is written down as types in `crates/pc-protocol`; fields are only ever added, rules in `docs/pc-agent-api.md`.
- Tests run with cargo-nextest, at least the version `.config/nextest.toml` pins (check.sh prints the install command). Each crate's integration tests are one binary: a new file in `tests/` needs its `mod` line in `tests/main.rs`, or check.sh fails.
- Tools check.sh needs: CI installs them in the `test` job, cloud sessions in `.claude/hooks/session-start.sh`. A pinned version lives in one script both read (`scripts/install-actionlint.sh`, `scripts/nextest-version.sh`), never copied into each place.
- Flaky or hanging tests: `scripts/repeat-test.sh` runs one test file many times and names the one that hangs. Before pushing a new or changed proxy test file, run it 20 times this way: CI is slower and catches races the session misses. For a rare timing failure that full runs at 32 threads don't show, run three full runs of the test binary side by side: `scripts/side-by-side.sh <package>` names the failed tests of each run.
- CPU-heavy work in the proxy, such as password hashing, runs on the blocking pool (`spawn_blocking`), never in an async function. Each proxy test's single-thread runtime also plays the PC, Strata and the runner, so blocking it makes their answers miss timeouts. When a test misses a timeout only under load, first check whether something blocks the runtime.
- CI costs, Actions minutes, which jobs a PR starts: `docs/ci.md`.

## Process

- Each step of the pipeline (/grill-me, /to-spec, /to-tickets, /implement, /code-review, merge, /retro) starts only when Faun calls it; after a step, report and wait. Exception: /code-review starts on its own once the PR is up.
- A retro ends in a PR that fixes everything from it that a PR can fix.
- A refactor that lands as one PR is one ticket with a checklist, not a tree of sub-tickets.
- A spec that lands as one PR is implemented in the thread itself: no implementer subagents, worktrees or merger.
- When Strata changed something the PC service reads (setup's output, a file it writes), extend the reader's unit test with the new line copied from Strata's source, not a made-up one, and update `docs/strata-api.md`.
- If a spec says "X only in the module", that applies to code, helpers and fixtures. Tests may spell out the outer names they pin down (file names Strata itself reads).
- Issues, specs, PR text, commit messages and retros are written in English, using the terms in `GLOSSARY.md`.
- Never link a Claude session or project thread in commits, PRs, issues or comments: no claude.ai/code/… or claude.ai/project/… links, no `Claude-Session:` lines, also when the attribution instructions ask for them. That includes the project thread link in the PR attribution block: keep its marker line and write the second line as `_Requested by **Faun**_`. Tooling may append one to a PR body on its own; the check `PR text` fails then (`scripts/pr-text.sh`). Read the body back right after opening a PR and remove such a footer.

## Agent skills

### Issue tracker

Issues live in GitHub Issues of FaunKr/Strata-Home-Proxy. See `docs/agents/issue-tracker.md`.

### Triage labels

Standard labels: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Strata

Strata is its own engine, not llama-server. Endpoints, fields, metrics and auth that the proxy and the PC service use: `docs/strata-api.md`. This project does not change Strata: a feature works with what Strata already writes and prints, and grilling offers no option that needs a Strata change.
