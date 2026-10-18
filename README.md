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
| 3. PC-Dienst und Gaming-Schalter | gebaut, gegen nachgebauten PC getestet: [docs/pc-dienst.md](docs/pc-dienst.md) |
| 4. Weboberfläche | Anmeldung, Einstellungen und Übersicht gebaut; Verlauf mit Stromschätzung offen |
| 5. Abnahme auf echter Hardware | offen |

## Was der NAS-Proxy tut

- Leitet alle Pfade an Strata weiter (OpenAI- und Anthropic-Format), Antworten
  werden live durchgereicht.
- **API-Keys:** Jeder Client braucht einen eigenen Key aus der Weboberfläche
  (Seite *API-Keys*), als `Authorization: Bearer …` oder `x-api-key`. Ohne
  gültigen Key kommt sofort `401` mit `x-strata-proxy-status: unauthorized`;
  der PC wird dafür nicht geweckt. Strata bekommt nie den Key des Clients,
  sondern den Strata-Key (`STRATA_API_KEY`), den nur der Proxy kennt.
- **PC schläft:** Weckpaket senden (alle 10 s wiederholt), warten bis Strata
  auf `/health` antwortet, dann weiterleiten. Mehrere gleichzeitige Anfragen
  teilen sich ein Aufwachen.
- **Gestreamte Anfragen** bekommen sofort eine Antwort und alle 15 s ein
  Ping-Ereignis, auch während Strata lange still rechnet (Claude Code bricht
  nach 300 s ohne Daten ab). Anthropic: `event: ping`, OpenAI: SSE-Kommentar.
- **Gaming-Modus:** sofort `503` mit Header `x-strata-proxy-status: gaming`.
  Wacht der PC nicht rechtzeitig auf: `504` mit `wake_timeout`. Daran kann der
  Skill `local-agent` einen Lauf zurückstellen.
- **Nicht eingerichtet:** Ohne Strata-Adresse startet der Proxy trotzdem und
  antwortet mit `503` und `not_configured`; ohne MAC-Adresse scheitert das
  Wecken mit `503` und `no_mac`.
- **Leerlauf:** 15 min ohne Anfrage und ohne offene Antwort, dann Schlafen-
  Anfrage an den PC-Dienst. Lehnt er ab, fragt der Proxy nach 1 min erneut.
- **MAC-Adresse:** Der PC-Dienst meldet sie selbst; der Proxy merkt sie sich in
  `/data/pc-mac` und weckt damit. `PC_MAC` braucht es nur für den allerersten
  Start, bevor der PC-Dienst je geantwortet hat.
- **Verlauf:** Weck- und Schlafereignisse in `/data/events.jsonl`.

## Auf dem NAS starten

**TrueNAS SCALE:** [deploy/truenas-app.yaml](deploy/truenas-app.yaml) als eigene App über *Install via YAML*. Anleitung steht oben in der Datei.

**Andere NAS mit Docker Compose:**

Das Image baut die CI bei jedem Push auf main: `ghcr.io/faunkr/strata-home-proxy:latest`.

```sh
mkdir -p strata-proxy && cd strata-proxy
curl -fsSLO https://raw.githubusercontent.com/FaunKr/Strata-Home-Proxy/main/deploy/docker-compose.yml
curl -fsSL -o .env https://raw.githubusercontent.com/FaunKr/Strata-Home-Proxy/main/deploy/env.example
nano .env                # Werte eintragen
docker compose pull
docker compose up -d
docker compose logs -f
```

Der Container braucht `network_mode: host`, sonst kommt das Weckpaket nicht
ins Heimnetz. Alle Einstellungen stehen mit Erklärung in
[deploy/env.example](deploy/env.example) und
[crates/nas-proxy/src/config.rs](crates/nas-proxy/src/config.rs).

Nur ein Weckpaket senden, zum Testen: `docker compose run --rm strata-proxy wake`

## Weboberfläche

Unter `http://<NAS-IP>:8080/admin/`, also auf demselben Port wie die API.
Aufrufe dort wecken den PC nicht und halten ihn nicht wach.

- **Übersicht:** zeigt live, ob der PC schläft, aufwacht, wach oder im
  Gaming-Modus ist, ob Strata bereit ist, lädt, Anfragen bearbeitet (mit
  wartenden Anfragen und Token/s) oder offline ist, die letzte Anfrage, einen
  Countdown bis zum Schlafen (oder warum nicht geschlafen wird) und die letzten
  zehn PC-Ereignisse. Solange eine Übersicht offen ist, fragt der Proxy alle
  5 s den PC-Dienst, `/health` und die Metriken von Strata ab; das
  weckt den PC nicht und hält ihn nicht wach. Für die Metriken braucht der
  Proxy den API-Key von Strata (`STRATA_API_KEY`). Welche Endpunkte von
  Strata der Proxy nutzt: [docs/strata-api.md](docs/strata-api.md).
- **Knöpfe:** *Jetzt wecken* (die 15 min Leerlauf zählen ab dem Aufwachen),
  *Jetzt schlafen legen* (lehnt der PC-Dienst ab, steht der Grund da),
  *Gaming an/aus* (weckt einen schlafenden PC vorher; nach *aus* zählt der
  Leerlauf neu) und *Wach halten* für 1 h, 4 h, 8 h oder bis morgen 8 Uhr.
  Wach halten übersteht einen Neustart (`/data/keep-awake.json`). „Morgen
  8 Uhr“ richtet sich nach der Zeitzone in `TZ` (z. B. `Europe/Berlin`), ohne
  `TZ` nach UTC. Läuft gerade eine Anfrage, fragen *schlafen legen* und
  *Gaming an* vorher nach. Jeder Knopf landet im Verlauf mit „über
  Weboberfläche“.
- **Erste Anmeldung:** Passwort `admin`, oder der Wert von `UI_PASSWORD`, falls
  gesetzt. Danach muss ein eigenes Passwort (mindestens 8 Zeichen) festgelegt
  werden, vorher geht nichts anderes. Es liegt als Hash in
  `/data/admin-password.json`.
- **Angemeldet bleiben:** 30 Tage, auch über einen Neustart. Ein Passwortwechsel
  meldet alle anderen Geräte ab.
- **Passwort vergessen:** `/data/admin-password.json` löschen und den Container
  neu starten, dann gilt wieder das Standardpasswort.
- **Einstellungen:** Jeder Wert aus [deploy/env.example](deploy/env.example)
  außer `DATA_DIR` lässt sich dort ändern und gilt sofort, nur die Adresse des
  Proxys (`LISTEN_ADDR`) erst nach einem Neustart. Es gilt der Wert von der
  Seite, sonst die Umgebungsvariable, sonst die Vorgabe; „Zurücksetzen“ verwirft
  den Wert von der Seite. Bei der MAC hat die vom PC-Dienst gemeldete Vorrang.
  Gespeichert wird in `/data/settings.json`; ohne `DATA_DIR` warnt die Seite,
  dass Änderungen beim Neustart verloren gehen. Das Token des PC-Dienstes wird
  nie angezeigt.
- **API-Keys:** Einen Key pro Client anlegen. Er wird nur direkt nach dem
  Anlegen vollständig angezeigt und nur als Hash in `/data/api-keys.json`
  gespeichert. Die Liste zeigt Anfang, Erstellung und letzte Nutzung; ein
  widerrufener Key gilt sofort nicht mehr. Solange kein Key existiert, bekommt
  jeder Client `401`.
- Nach 5 Fehlversuchen von derselben Adresse ist die Anmeldung 1 min gesperrt.
  `/admin/` ist überall erreichbar, wo die API erreichbar ist; das Passwort ist
  der einzige Schutz.

## Entwickeln

```sh
cargo test      # startet ein nachgebautes Strata, PC-Dienst und WoL-Empfänger
cargo clippy --all-targets -- -D warnings
```

Die Weboberfläche (Leptos, [crates/admin-ui](crates/admin-ui)) braucht für den
Browser-Teil `rustup target add wasm32-unknown-unknown` und
`cargo install cargo-leptos --locked`. Dann baut `cargo leptos build` Proxy und
Oberfläche, `cargo leptos watch` startet beides und baut bei Änderungen neu.
