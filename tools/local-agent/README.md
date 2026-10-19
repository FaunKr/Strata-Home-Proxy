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
   - Kontext mindestens 64K; darunter fällt jeder Lauf zurück, weil Claude
     Codes eigener Prompt schon rund 25K Tokens braucht. 128K, wenn der
     Speicher reicht.
   - `fit_max_tokens` an, damit Strata `max_tokens` kürzt statt mit 400
     abzulehnen.
   - `"anthropic_thinking": "on_request"`, damit Claude Codes kurze
     Hilfsanfragen nicht erst nachdenken.
4. `local-agent check` prüft Konfiguration, Key, `/health`, Kontext und einen
   Tool-Aufruf über Strata und nennt bei einem Fehler, was zu tun ist.
5. Den Skill speichern: den Inhalt von [SKILL.md](SKILL.md) als persönlichen
   Skill `local-agent` anlegen.

## Kompaktierung prüfen

Claude Code nimmt mit einem Claude-Modellnamen ein Fenster von 200K an (1M ist
abgeschaltet) und wüsste sonst nichts von Stratas kleinerem Kontext. Der Runner
setzt deshalb `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` aus `max_context`: 75 % davon,
bezogen auf 200K (64K ergibt 24, also Kompaktierung bei rund 48K Tokens). Ob
das im Modus `-p` greift, sagt die Doku von Claude Code nicht. Einmal auf
echter Hardware nachweisen, bei 64K Kontext:

```sh
cd <dieses Repo>
local-agent run --read-only --watch "Lies jede Datei unter crates/nas-proxy/src und crates/admin-ui/src vollständig, eine nach der anderen, und fasse jede in einem Satz zusammen."
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
