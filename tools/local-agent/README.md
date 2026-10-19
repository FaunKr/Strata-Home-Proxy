# local-agent

Gibt Aufgaben aus Claude Code an Strata ab. `local-agent run` startet ein
headless `claude -p`, dessen API der Proxy auf dem NAS ist; Strata führt so die
ganze Agentenschleife aus, in einem eigenen Worktree auf einem neuen Branch.
Der Skill `local-agent` ([SKILL.md](SKILL.md)) sagt Claude, wann und wie.
Entwurf und Entscheidungen: Issue #107.

Läuft auf macOS und Linux, mit Bash, git, jq und curl. Cloud-Sitzungen
erreichen das NAS nicht.

## Einrichten

1. `tools/local-agent/install.sh` kopiert `local-agent` nach `~/.local/bin`
   (oder `PREFIX`) und legt `~/.config/local-agent/env` an, nur für dich lesbar.
2. Im Bereich *Access* der Weboberfläche einen Key für diesen Rechner anlegen
   und mit der Adresse des Proxys eintragen:

   ```sh
   LOCAL_AGENT_URL=http://<NAS-IP>:8080
   LOCAL_AGENT_KEY=<Key aus Access>
   ```

   Optional: `LOCAL_AGENT_MODEL` (ein Claude-Modellname; Strata ignoriert ihn,
   Claude Code verlangt einen bekannten) und `LOCAL_AGENT_COMPACT_SHARE`
   (Anteil von Stratas Kontext in Prozent, ab dem Claude Code kompaktiert;
   Standard 75).
3. In Strata (Modell-Konfiguration auf dem PC):
   - Kontext mindestens 128K; darunter fällt jeder Lauf zurück. Bei 64K
     kompaktierte Claude Code bei Arbeit mit vielen Dateien im Kreis: Nach
     einer Kompaktierung waren schon 20–29K belegt, und eine gelesene Datei
     bringt bis zu 15K dazu.
   - `fit_max_tokens` an, als Sicherheitsnetz: Strata kürzt dann `max_tokens`,
     statt mit 400 abzulehnen. Das hält einen Lauf nicht am Leben, es macht aus
     einem späten 400 nur immer kürzere Antworten; vor dem vollen Kontext
     schützt die Kompaktierung (siehe unten).
   - `"anthropic_thinking": "on_request"`, damit Claude Codes kurze
     Hilfsanfragen nicht erst nachdenken.
4. `local-agent check` prüft Konfiguration, Key, `/health`, Kontext und einen
   Tool-Aufruf über Strata und nennt bei einem Fehler, was zu tun ist.
5. Den Skill speichern: den Inhalt von [SKILL.md](SKILL.md) als persönlichen
   Skill `local-agent` anlegen.

## Kompaktierung prüfen

Claude Code weiß nichts von Stratas Kontext. Ohne eigene Vorgabe kompaktierte
es im Modus `-p` gar nicht (Claude Code 2.1.218, 2026-10-19): Lauf #130 wuchs
auf 253K Tokens und endete an Stratas 400. Auch
`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` allein, bezogen auf die angenommenen 200K,
zeigte keine Wirkung. Der Runner setzt deshalb vor jedem Lauf (und in
`local-agent check`) das Kompaktierungsfenster
`CLAUDE_CODE_AUTO_COMPACT_WINDOW` ausdrücklich, berechnet aus Stratas
`max_context`:

- Ziel = `LOCAL_AGENT_COMPACT_SHARE` (Standard 75) % von `max_context`;
- Fenster = Ziel + 8192 (Ausgabegrenze) + 13000, begrenzt auf 100000–180000.

Gemessen kompaktiert Claude Code bei etwa Fenster − Ausgabegrenze − 13K: bei
einem Fenster von 100K bei 80–89K, bei 150K bei 130K. Bei 256K ergibt das das
Fenster 180000 und Kompaktierung bei rund 159K; bei 128000 das Fenster 117192
und rund 96K. Unter 100K nimmt Claude Code kein Fenster an; nur dann setzt der
Runner zusätzlich `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` (Ziel geteilt durch
1000, also Prozent des 100K-Fensters). Die Obergrenze 180K liegt bewusst unter Claude
Codes 200K-Modellfenster, weil der Bereich darüber nicht getestet ist.
`local-agent check` zeigt das Fenster und den ungefähren Punkt der
Kompaktierung. Während eines Laufs zeigt `local-agent status` (und im
Fehlerfall die Log-Zusammenfassung in `local-agent result`), wie voll der
Kontext ist und wie oft schon kompaktiert wurde. Liegt der Kontext mehr als
20K über dem Punkt der Kompaktierung, ohne dass Claude Code kompaktiert hat,
steht dort eine Warnung; mit `--watch` landet sie einmal in der
Fortschrittsdatei. Messungen: `docs/research/fit-max-tokens-and-autocompact.md`
(Abschnitt „Hardware test").

Einmal auf echter Hardware nachweisen, mit dem Kontext, den Strata fährt:

```sh
cd <dieses Repo>
local-agent run --read-only --watch "Lies jede Datei unter crates/nas-proxy/src und crates/admin-ui/src vollständig, eine nach der anderen, und fasse jede in einem Satz zusammen. Keine Subagenten: lies jede Datei selbst."
```

Bestanden, wenn der Lauf mit Exit 0 endet und die Fortschrittsdatei (oder
`local-agent status`) mindestens einmal `context compacted` zeigt. Steht
stattdessen `request exceeds the context window` im Log
(`~/.local/state/local-agent/runs/<Lauf>/log.jsonl`), kompaktiert Claude Code
zu spät: `LOCAL_AGENT_COMPACT_SHARE` senken und erneut prüfen.

## Tests

`tools/local-agent/test.sh` (läuft in `scripts/check.sh`) startet den echten
Runner gegen einen nachgebildeten Proxy (`testdata/fake-proxy.py`) und ein
nachgebildetes `claude` (`testdata/fake-claude`).
