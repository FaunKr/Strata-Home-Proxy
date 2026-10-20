# CI und Actions-Minuten

Das Repo ist privat; GitHub Free enthält 3.000 Actions-Minuten im Monat. In
den ersten zwei Tagen gingen davon rund 2.500 weg, deshalb läuft die CI seit
#151 nur noch, was eine Änderung braucht (Spec #146).

## Was Minuten kostet

- Jeder Job rundet auf eine volle Minute auf; auch `changes` und `ci-ok`
  kosten je eine.
- Windows-Runner zählen doppelt. Deshalb läuft der Windows-Job nur von Hand
  (`windows.yml`), die CI prüft den Windows-Code des PC-Dienstes unter Linux.
- Der Actions-Cache fasst 10 GB und verdrängt danach die ältesten Einträge.
  Caches eines Pull Requests sieht nur dieser Pull Request, die von `main`
  sehen alle. Deshalb schreibt nur `main` Caches (rust-cache `save-if`, Docker
  `cache-to` nur im Job `publish`).
- rust-cache hält die Abhängigkeiten; sein Schlüssel enthält Cargo.lock, die
  Manifeste und die rustc-Version. Auf `main` sieht der Job `test` nur nach,
  ob der Cache da ist, und läuft nur dann voll, um ihn neu zu füllen.

## Was ein Lauf braucht

Welche Jobs ein Pull Request startet, entscheidet `scripts/ci-select.sh` nach
den geänderten Pfaden; `scripts/ci-select.test.sh` prüft die Zuordnung gegen
das Repo. Richtwerte nach #151:

| Lauf | Jobs | Minuten |
|---|---|---|
| PR mit Code-Änderung | changes, test, ci-ok | ca. 6–10 |
| PR, der Workflow, Cargo.lock oder scripts/ ändert | alles außer Windows | ca. 18 |
| Push auf `main` | changes, test (nur Nachsehen), publish, ci-ok | ca. 5–10 |

Der Job `publish` läuft nie zweimal gleichzeitig und endet nach 30 Minuten.
Ein neuerer Push auf `main` wartet, bis der laufende fertig ist; so setzt kein
älterer Commit `:latest` zurück.

## What gets published

- `publish` pushes both images to ghcr.io as `:latest` and `:<commit>`. The
  NAS and `deploy/truenas-auto-update.sh` follow `:latest`.
- The version is the crate version from each `Cargo.toml`. The web UI shows
  the proxy's under *System* (`crates/nas-proxy/src/admin.rs`). The PC-Dienst
  reports its own with the build commit appended, e.g. `0.1.0+5e8b9f1`
  (`crates/pc-agent/build.rs`, from `STRATA_BUILD_ID`).
- There are no git tags or GitHub releases.

## Wohin die Minuten gehen

Die schnellste Quelle ist der Nutzungsbericht unter *Settings → Billing →
Usage* (Export als CSV): Minuten je Repo, Workflow und Runner-Betriebssystem.
Aus den Läufen selbst lässt es sich nur über die Job-Zeiten jedes einzelnen
Laufs rekonstruieren, ein Aufruf pro Lauf.
