## Arbeiten im Repo

- PRs gehen gegen `main`.
- Vor dem Push `scripts/check.sh`: dieselben Checks wie der CI-Job `test`.
- Bevor du vorschlägst, einen PR zu mergen: `/code-review` über seinen Diff.
- Neue Proxy-Tests nutzen das Gerüst mit simuliertem PC und Strata in `crates/nas-proxy/tests/common`.
- Wackelnde oder hängende Tests: `scripts/repeat-test.sh` lässt eine Testdatei vielfach laufen und nennt, welcher hängt.

## Agent skills

### Issue tracker

Issues liegen in GitHub Issues von FaunKr/Strata-Home-Proxy. See `docs/agents/issue-tracker.md`.

### Triage labels

Standard-Labels: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: ein `GLOSSARY.md` und `docs/adr/` im Repo-Root. See `docs/agents/domain.md`.

### Strata

Strata ist eine eigene Engine, kein llama-server. Endpunkte, Felder, Metriken und Auth, die Proxy und PC-Dienst nutzen: `docs/strata-api.md`.
