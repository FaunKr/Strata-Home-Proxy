# Strata Home Proxy

Der Strata-PC zuhause geht in Standby, wenn keine Anfragen kommen, und wacht
per Wake on LAN auf, sobald eine eintrifft. Im Gaming-Modus passiert beides
nicht. Hintergrund und Entscheidungen stehen im Handoff im Projekt.

Alle Clients sprechen nur mit dem NAS. Der Proxy dort weckt den PC bei Bedarf
und bittet ihn nach Leerlauf, schlafen zu gehen.

## Stand

| Schritt | Stand |
| --- | --- |
| 1. Einmalige Einrichtung | Anleitung: [docs/einrichtung.md](docs/einrichtung.md) |
| 2. NAS-Proxy | gebaut, gegen nachgebautes Strata getestet |
| 3. PC-Dienst und Gaming-Schalter | offen; Schnittstelle: [docs/pc-agent-api.md](docs/pc-agent-api.md) |
| 4. Weboberfläche | offen |
| 5. Abnahme auf echter Hardware | offen |

## Was der NAS-Proxy tut

- Leitet alle Pfade an Strata weiter (OpenAI- und Anthropic-Format), Antworten
  werden live durchgereicht.
- **PC schläft:** Weckpaket senden (alle 10 s wiederholt), warten bis Strata
  auf `/health` antwortet, dann weiterleiten. Mehrere gleichzeitige Anfragen
  teilen sich ein Aufwachen.
- **Gestreamte Anfragen** bekommen sofort eine Antwort und alle 15 s ein
  Ping-Ereignis, auch während Strata lange still rechnet (Claude Code bricht
  nach 300 s ohne Daten ab). Anthropic: `event: ping`, OpenAI: SSE-Kommentar.
- **Gaming-Modus:** sofort `503` mit Header `x-strata-proxy-status: gaming`.
  Wacht der PC nicht rechtzeitig auf: `504` mit `wake_timeout`. Daran kann der
  Skill `local-agent` einen Lauf zurückstellen.
- **Leerlauf:** 15 min ohne Anfrage und ohne offene Antwort, dann Schlafen-
  Anfrage an den PC-Dienst. Lehnt er ab, fragt der Proxy nach 1 min erneut.
- **Verlauf:** Weck- und Schlafereignisse in `/data/events.jsonl`.

## Auf dem NAS starten

```sh
git clone https://github.com/FaunKr/Strata-Home-Proxy.git
cd Strata-Home-Proxy/deploy
cp env.example .env      # Werte eintragen
docker compose up -d --build
docker compose logs -f
```

Der Container braucht `network_mode: host`, sonst kommt das Weckpaket nicht
ins Heimnetz. Alle Einstellungen stehen mit Erklärung in
[deploy/env.example](deploy/env.example) und
[crates/nas-proxy/src/config.rs](crates/nas-proxy/src/config.rs).

Nur ein Weckpaket senden, zum Testen: `docker compose run --rm strata-proxy wake`

## Entwickeln

```sh
cargo test      # startet ein nachgebautes Strata, PC-Dienst und WoL-Empfänger
cargo clippy --all-targets -- -D warnings
```
