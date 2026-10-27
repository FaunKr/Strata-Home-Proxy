## Working in the repo

- PRs go against `main`. Start implementing from a freshly fetched `origin/main`: the session's clone may be hours behind.
- Before pushing, run `scripts/check.sh` and push only when its last line reads `check.sh: PASSED`: the same checks as the CI job `test`. To iterate, `scripts/check.sh <crate>…` checks only those crates. Keep its full output (`mkdir -p target && scripts/check.sh 2>&1 | tee target/check.log`) and filter that file: a test that fails once is read there, not rerun blind.
- Before you suggest merging a PR: `/code-review` over its diff. In this repo that is the skill `mattpocock-skills:code-review` (Standards against `CODING_STANDARDS.md`, Spec against the issue); the built-in `code-review` reads neither.
- Bringing a PR to merge: read `.claude/skills/steward/SKILL.md` when opening a PR, before waiting on CI and before every merge, also after the context was compacted. Session skill lists may not show it.
- Right after opening a PR or pushing to one, schedule the check-in of steward rule 2 (`send_later`) in the same turn. A green CI run sends no event: without the check-in a green PR waits unreported until someone asks.
- Check UI changes in the browser: `scripts/ui-check.sh` (`SHOTS=1` for screenshots). Work that local-agent built on the Mac, which has no Playwright, gets this run in a cloud session before its PR opens.
- New proxy tests use the harness with a simulated PC and Strata in `crates/nas-proxy/tests/common`; a proxy restart over the same `DATA_DIR` is `restarted` there, not a second `setup`. Files the proxy writes on the blocking pool can still be on their way then: a restart after a list of models waits with `list_saved_on_disk`.
- What the PC service takes and answers over HTTP is written down as types in `crates/pc-protocol`; fields are only ever added, rules in `docs/pc-agent-api.md`.
- Tests run with cargo-nextest, at least the version `.config/nextest.toml` pins (check.sh prints the install command). The integration tests of nas-proxy and pc-agent are one binary each, named `integration`, with each file a module: one file's tests run with `cargo nextest run -p strata-nas-proxy --test integration <file>::`. A new file in `tests/` needs its `mod` line in `tests/main.rs`, or check.sh fails.
- Tools check.sh needs: CI installs them in the `test` job, cloud sessions in `.claude/hooks/session-start.sh`. A pinned version lives in one script both read (`scripts/install-actionlint.sh`, `scripts/nextest-version.sh`), never copied into each place. No cargo-nextest or cargo-leptos in a cloud session: the hook did not run or an install in it failed; run `CLAUDE_CODE_REMOTE=true .claude/hooks/session-start.sh` once and read its output.
- Flaky or hanging tests: `scripts/repeat-test.sh` runs one test file many times and names the one that hangs. Before pushing a new or changed proxy test file, run it 20 times this way: CI is slower and catches races the session misses. For a rare timing failure that full runs at 32 threads don't show, run three full runs of the test binary side by side: `scripts/side-by-side.sh <package>` names the failed tests of each run.
- CPU-heavy work in the proxy, such as password hashing, runs on the blocking pool (`spawn_blocking`), never in an async function. Each proxy test's single-thread runtime also plays the PC, Strata and the runner, so blocking it makes their answers miss timeouts. When a test misses a timeout only under load, first check whether something blocks the runtime.
- CI costs, Actions minutes, which jobs a PR starts: `docs/ci.md`.
- A release of the product (proxy and web UI) raises `[workspace.package]` `version` in `Cargo.toml` with a section in `CHANGELOG.md`; after the merge, the workflow *Release* publishes it. Tagging and running it is Faun's go: `docs/ci.md`, *Releases*.
- A PR that changes what the PC service does for the user adds a section to `crates/pc-agent/CHANGELOG.md` and bumps the version in `crates/pc-agent/Cargo.toml` to it: the major number when the user has to act on the PC, the minor for features, else the patch number. Refactors and test-only changes add none. A fix counts too: without a bump it reaches PCs only with the next bump or a new install command. Rules: `docs/pc-service.md`.
- local-agent has two runners, `tools/local-agent/local-agent` (Bash) and `local-agent.ps1` (Windows). A change to its behaviour goes into both runners and both test scripts in the same PR; each test in `test.sh` has one of the same name in `test.ps1`.

## Process

- Each step of the pipeline (/grill-me, /to-spec, /to-tickets, /implement, /code-review, merge, /retro) starts only when Faun calls it; after a step, report and wait. Exception: /code-review starts on its own once the PR is up.
- A retro ends in a PR that fixes everything from it that a PR can fix.
- A refactor that lands as one PR is one ticket with a checklist, not a tree of sub-tickets.
- A spec that lands as one PR is implemented in the thread itself: no implementer subagents, worktrees or merger.
- /to-spec writes the test seams into the spec's Testing Decisions (where the tests drive the feature, and each new fake or setter for tests, named as new) and publishes the spec in the same step: Faun reviews the seams with the published spec.
- Each new behaviour in a spec's Testing Decisions gets a new test, named after it, never "extend test X" (`CODING_STANDARDS.md`, *New behaviour gets a test of its own*): the implementer follows the spec over the standard.
- When grilling changes what the code benchmark asks of the agent (a TASK.md or the runner's prompt), ask whether the score should count the new demand too: a rule the score ignores costs a model nothing.
- When Strata changed something the PC service reads (setup's output, a file it writes), extend the reader's unit test with the new line copied from Strata's source, not a made-up one, and update `docs/strata-api.md`.
- When the PC service keeps or starts with a path Strata wrote (a run config's `cwd`, args, the picture reader's program), check whether `strata update` can delete what it names: updates keep only the new and the previous Control or engine folder (`docs/strata-api.md`, *Folders updates delete*).
- When grilling or a spec adds a rule that data the PR itself writes must follow (the first section of the PC service changelog, a migrated file), check that data against the rule before publishing the spec: the rule and the first data can contradict each other.
- A spec states a comparison rule as the outcome User sees ("no update for a PC on that version or a newer one"), not as an operator ("equals"): the spec review reads it word for word.
- Before a spec refuses values in a run config field, check what Strata itself writes there (`docs/strata-api.md`, Strata's source): a value Strata put there must still come back unchanged, or saving the field always fails.
- When grilling or a spec moves when the proxy acts (a wake packet sooner, a request answered without Strata), grep the tests for asserts on that action first (`wol_packets`, `api_requests`, event kinds): an existing test can pin the opposite, and the spec should settle which one wins.
- When grilling or a spec makes something depend on the client's API key, it decides for the proxy's own key too: the code benchmark's opencode reaches Strata through the proxy with a key the proxy makes per run (`code_bench::new_key`), unlike the update summary and the list fetch after a wake-up, which go to Strata directly.
- Before publishing a spec, read each rule in *Implementation Decisions* against *Out of Scope* and the user stories: a path or field a rule changes must not be listed as unchanged elsewhere. The spec review and the implementer each pick one side otherwise.
- If a spec says "X only in the module", that applies to code, helpers and fixtures. Tests may spell out the outer names they pin down (file names Strata itself reads).
- Issues, specs, PR text, commit messages and retros are written in English, using the terms in `GLOSSARY.md`. Its *Avoid* lists cover new file and script names too, unless an entry allows the term there.
- Before a decision card offers a new PC service version, read what the number drives in `crates/nas-proxy/src/pc_changelog.rs` (update offer, *Needs a step on the PC*): an option can change what every PC's card shows.
- Never link a Claude session or project thread in commits, PRs, issues or comments: no claude.ai/code/… or claude.ai/project/… links, no `Claude-Session:` lines, also when the attribution instructions ask for them. That includes the project thread link in the PR attribution block: keep its marker line and write the second line as `_Requested by **Faun**_`. Tooling may append one to a PR body on its own; the check `PR text` (check run `session-links`) fails then (`scripts/pr-text.sh`). Read the body back right after opening a PR and remove such a footer.

## Agent skills

### Issue tracker

Issues live in GitHub Issues of FaunKr/Strata-Home-Proxy. See `docs/agents/issue-tracker.md`.

### Triage labels

Standard labels: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Strata

Strata is its own engine, not llama-server. Endpoints, fields, metrics and auth that the proxy and the PC service use: `docs/strata-api.md`. This project does not change Strata: a feature works with what Strata already writes and prints, and grilling offers no option that needs a Strata change.
