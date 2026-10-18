## Arbeiten im Repo

- PRs gehen gegen `main`.
- Vor dem Push dieselben Befehle wie der CI-Job `test` (`.github/workflows/ci.yml`), auch die wasm-Zeile für die Weboberfläche.
- Neue Proxy-Tests nutzen das Gerüst mit simuliertem PC und Strata in `crates/nas-proxy/tests/common`.

## Agent skills

### Issue tracker

Issues liegen in GitHub Issues von FaunKr/Strata-Home-Proxy. See `docs/agents/issue-tracker.md`.

### Triage labels

Standard-Labels: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: ein `GLOSSARY.md` und `docs/adr/` im Repo-Root. See `docs/agents/domain.md`.

### Strata

Strata ist eine eigene Engine, kein llama-server. Endpunkte, Felder, Metriken und Auth, die Proxy und PC-Dienst nutzen: `docs/strata-api.md`.
