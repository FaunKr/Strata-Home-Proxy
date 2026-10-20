## Arbeiten im Repo

- PRs gehen gegen `main`.
- Vor dem Push `scripts/check.sh`: dieselben Checks wie der CI-Job `test`. Zum Iterieren prüft `scripts/check.sh <crate>…` nur diese Crates.
- Bevor du vorschlägst, einen PR zu mergen: `/code-review` über seinen Diff.
- UI-Änderungen im Browser prüfen: `scripts/ui-check.sh` (`SHOTS=1` für Screenshots).
- Neue Proxy-Tests nutzen das Gerüst mit simuliertem PC und Strata in `crates/nas-proxy/tests/common`.
- Was der PC-Dienst über HTTP nimmt und antwortet, steht als Typen in `crates/pc-protocol`; Felder kommen nur dazu, Regeln in `docs/pc-agent-api.md`.
- Wackelnde oder hängende Tests: `scripts/repeat-test.sh` lässt eine Testdatei vielfach laufen und nennt, welcher hängt.
- CI-Kosten, Actions-Minuten, welche Jobs ein PR startet: `docs/ci.md`.

## Ablauf

- Jeder Schritt der Pipeline (/grill-me, /to-spec, /to-tickets, /implement, /code-review, Merge, /retro) startet erst, wenn Faun ihn aufruft; nach einem Schritt berichten und warten.
- Ein Refactor, der als ein PR landet, ist ein Ticket mit Checkliste, kein Baum aus Unter-Tickets.
- Schreibt ein Spec „X nur im Modul“, gilt das für Code, Helfer und Fixtures. Tests dürfen die äußeren Namen ausschreiben, die sie festnageln (Dateinamen, die Strata selbst liest).

## Agent skills

### Issue tracker

Issues liegen in GitHub Issues von FaunKr/Strata-Home-Proxy. See `docs/agents/issue-tracker.md`.

### Triage labels

Standard-Labels: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: ein `GLOSSARY.md` und `docs/adr/` im Repo-Root. See `docs/agents/domain.md`.

### Strata

Strata ist eine eigene Engine, kein llama-server. Endpunkte, Felder, Metriken und Auth, die Proxy und PC-Dienst nutzen: `docs/strata-api.md`.
