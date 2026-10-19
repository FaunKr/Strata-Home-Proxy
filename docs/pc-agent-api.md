# Schnittstelle zwischen NAS-Proxy und PC-Dienst

Der Dienst auf dem PC (Schritt 3) bietet diese Endpunkte an. Der Proxy nutzt
sie schon jetzt; die Tests bilden den Dienst nach.

Alle Anfragen tragen `Authorization: Bearer <PC_AGENT_TOKEN>`; der Dienst
verlangt das Token immer. Er nimmt Anfragen nur von `allowed_clients` (dem
NAS) und von 127.0.0.1 an.

## `GET /status`

```json
{ "gaming_mode": false, "strata_running": true, "user_idle_secs": 1840, "mac": "AA:BB:CC:DD:EE:FF", "strata_job": false, "version": "0.1.0+558db97", "hardware": { … } }
```

`strata_job` ist `true`, solange ein Strata-Vorgang läuft (siehe unten).

`version` ist die Crate-Version plus der Commit des Baus (`STRATA_BUILD_ID`
beim Bauen, sonst `dev`). Dieselbe Angabe steht als Text
`strata-pc-agent-version:<version>` in der .exe; so liest der Proxy die
Version der .exe in seinem Image, ohne sie auszuführen. Weicht sie ab oder
fehlt `version` (Dienst von vor dem Update-Knopf), zeigt der Bereich *PC*
„Update available“.

`mac` ist die MAC-Adresse der Netzwerkkarte, über die die Anfrage kam (also
die, die das Weckpaket empfangen muss), oder der Wert `mac` aus der
Konfiguration. Ist sie unbekannt, fehlt das Feld. Der Proxy merkt sich die
gemeldete MAC in `DATA_DIR` und weckt damit; sie hat Vorrang vor `PC_MAC` und
den Werten aus der Weboberfläche.

`hardware` sind die Hardwarewerte für die Übersicht. Der Dienst liest sie aus
Windows selbst, ohne Treiber: Solange `/status` gefragt wird, misst er alle 2 s
im Hintergrund und antwortet mit der letzten Messung. Die erste Anfrage nach
30 s Ruhe startet die Messung und bekommt nur `null`-Werte.

```json
"hardware": {
  "gpu": { "load_pct": 37.0, "temp_c": 61.5, "vram_used_bytes": 12670000000,
           "vram_total_bytes": 17163091968, "fan_rpm": 0, "power_pct": 41.0 },
  "cpu": { "load_pct": 12.0, "ram_used_bytes": 22980000000, "ram_total_bytes": 34359738368 }
}
```

- `gpu` ist die Grafikkarte mit dem meisten VRAM, `null` ohne Grafikkarte.
- `power_pct` ist die Leistungsaufnahme als Anteil am Leistungslimit der
  Karte, keine Watt.
- `fan_rpm: 0` heißt, der Lüfter steht. Meldet der Treiber 0 und keine
  Höchstdrehzahl, kennt er den Lüfter nicht, und der Wert ist `null`.
- Temperatur und Leistung meldet der Treiber als 0, wenn er sie nicht kennt;
  der Dienst macht daraus `null`.
- Jeder Wert, den der Dienst nicht lesen kann, ist `null`; die anderen bleiben.
- Die CPU-Last gilt seit der vorigen Messung und ist in der ersten `null`.

Fehlt `hardware`, ist der Dienst älter; die Übersicht bittet dann um ein Update.

Antwortet der Dienst nicht, geht der Proxy davon aus, dass der PC schläft,
und weckt ihn.

## `POST /sleep`

Der Proxy schickt das nach der Leerlaufzeit. Der Dienst prüft Gaming-Modus,
laufende Strata-Anfragen (über `/slots`, falls Strata das anbietet) und
Benutzeraktivität (letzte Tastatur- oder Mauseingabe, Vorgabe 10 min).
Die Antwort kommt zuerst, Standby folgt 2 s später.

- `200 {"accepted": true}`: Standby wird ausgelöst.
- `409 {"accepted": false, "reason": "Benutzer aktiv"}`: abgelehnt. Der
  Proxy fragt nach `SLEEP_RETRY_SECS` erneut.

## `POST /update` (für die Weboberfläche)

Lädt `strata-pc-agent.exe` von `nas_url` aus der Konfiguration
(`/admin/agent/strata-pc-agent.exe`), benennt die laufende .exe in
`strata-pc-agent.exe.alt` um, legt die neue an ihre Stelle und startet sie
mit denselben Argumenten. Die Antwort kommt danach; 1 s später beendet sich
der alte Dienst, der neue wartet so lange auf den Port und löscht dann die
`.alt`.

- `202 {"accepted": true}`: die neue .exe läuft gleich.
  Ab dem Download bis zum Ende lehnt der Dienst Strata-Vorgänge und Schlafen
  ab.
- `409 {"reason": …}`: nicht jetzt (Strata bearbeitet eine Anfrage, ein
  Strata-Vorgang oder ein Update läuft) oder ohne `nas_url` eingerichtet.
- `500 {"reason": …}`: Download oder Austausch gescheitert; der alte Dienst
  läuft weiter, nichts ist verändert.

Der Proxy schickt das nur bei wachem PC und weckt ihn dafür nie. Ein Dienst
ohne diesen Endpunkt antwortet 404; der Bereich *PC* sagt dann, ihn einmal
mit dem Befehl von dort neu einzurichten.

## `POST /gaming` (für die Weboberfläche)

```json
{ "enabled": true }
```

An: Strata beenden, Schlafen sperren. Aus: Strata starten, Schlafen erlauben.

## `GET /strata-log` (für den Reiter *Output* im Bereich *Strata*)

Die Strata-Ausgabe als Server-Sent Events: was ein vom Dienst gestartetes
Strata auf seine Konsole schreibt. Zuerst kommen die gemerkten Zeilen (höchstens
500, Trennzeilen eingeschlossen), dann jede neue, dazu jeder Wechsel des
Hinweises. Jede Nachricht ist ein JSON-Objekt in `data:`:

| `kind` | Felder | Bedeutung |
| --- | --- | --- |
| `started` | `at` (Unix-Sekunden) | Der Dienst hat Strata gestartet; die Zeilen davor bleiben |
| `line` | `text` | eine neue Zeile |
| `replace` | `text` | ersetzt die letzte Zeile (Fortschritt, der in der Konsole dieselbe Zeile mit `\r` überschreibt) |
| `notice` | `reason`: `running`, `gaming`, `not_running`, `not_started_by_agent` | ob es Ausgabe gibt und warum nicht; `not_started_by_agent`: Strata antwortet, wurde aber nicht vom Dienst gestartet |

Farbcodes und andere Steuerzeichen sind entfernt. Kommt der Abrufer nicht
hinterher, endet der Strom; beim nächsten Verbinden kommen die gemerkten
Zeilen neu. Der Proxy öffnet den Strom nur, solange jemand die Seite offen hat
und der PC ohnehin wach ist.

## Strata verwalten (für den Bereich *Strata*)

Der Dienst richtet Strata selbst ein und verwaltet es. Jeder Vorgang ruft
feste Programme mit Werten aus Stratas eigenen Tabellen auf; eine Anfrage
enthält nie Befehle oder Pfade. Es läuft höchstens ein Vorgang; solange er
läuft, lehnt der Dienst Schlafen ab und der Watchdog hält still. Gaming an
bricht ihn ab, ein begonnener Download macht beim nächsten Mal weiter.

### `GET /strata`

Was installiert ist, Stratas Katalog mit Empfehlung für den RAM des PCs,
freier Platz, letzter Benchmark und der laufende oder letzte Vorgang.
Enthält nie den Strata-Key. Fragt nebenbei höchstens einmal pro Stunde Git,
ob es eine neuere Strata-Version gibt (`update_available`).

Kommen Commits herein, steht in `update`, was sie bringen; ohne Update, vor
der ersten Prüfung und bei einem älteren PC-Dienst fehlt das Feld:

```json
{"upstream": "fedcba98…", "total": 14, "remote": "https://github.com/Niko1221/Strata.git",
 "commits": [{"hash": "fedcba98…", "time": 1760000000, "subject": "…", "body": "…"}],
 "files": [{"path": "serve/server.py", "added": 120, "removed": 30}]}
```

`commits` sind die neuesten zuerst, höchstens 200 (`total` zählt alle).
`files` kommt aus `git diff --numstat HEAD..@{u}`; Binärdateien zählen 0
Zeilen. Der Proxy behält das alles auf dem NAS; die Weboberfläche bekommt nur
Kurz-Hash, Betreff und Datum.

Jedes installierte Modell sagt mit `speed_projection`, ob seine Konfiguration
einen Steuervektor lädt (`--control-vector-scaled`, Stratas oder ein eigener).
Ein älterer PC-Dienst lässt das Feld weg.

`context` ist der Wert von `--max-context` in seiner Konfiguration (Tokens),
`null`, wenn das Flag fehlt oder sein Wert keine Zahl ist. Ein älterer
PC-Dienst lässt das Feld weg.

`benchmarks` hält den letzten Lauf jedes Modells: `decode_tps` (`chat decode
tok/s`), `prompt_tps` (`4K prompt tok/s`, fehlt, wenn der Benchmark die Zeile
nicht druckt), dazu `context`, `speed_projection` und `version` (Strata-Commit)
zur Zeit des Laufs. Ein Modell, das im Vergleich nicht anlief oder nichts
meldete, hat statt der Werte `error`. `benchmark` bleibt der letzte
erfolgreiche Lauf, für Proxys, die nur einen kennen. Ein älterer PC-Dienst
lässt `benchmarks` weg.

### `POST /strata/jobs`

Startet einen Vorgang, Antwort `202` mit dem Vorgang. `400 {"reason": …}`
bei Werten außerhalb des Katalogs, `409 {"reason": …}`, wenn es gerade nicht
geht (Gaming an, ein Vorgang läuft, zu wenig Platz, aktives Modell löschen).

| `kind` | Felder | Was passiert |
| --- | --- | --- |
| `install` | `family`, `size`, `context`, `api_key`, optional `experimental_speed_projection` | Git (per winget, falls nötig), `git clone`, Setup ohne Fragen, startet Strata |
| `add_model` | `family`, `size`, `context`, optional `api_key` und `experimental_speed_projection` | Setup mit `--setup` und dem Key des Proxys (sonst dem eigenen), das aktive Modell bleibt aktiv |
| `activate` | `model` | wartet, bis Strata keine Anfrage bearbeitet, startet mit dem anderen Startskript neu |
| `delete` | `model` | löscht Modellordner, Pack, Konfiguration, Startskript und Log dieses Modells |
| `update` | – | `git pull --ff-only`, dann `START-HERE.bat --update` bei beendetem Strata, startet neu |
| `benchmark` | – | `BENCH.bat --config <aktives Modell>`, behält Tokens/s mit Modell und Datum |
| `compare_models` | – | `benchmark` für jedes installierte Modell nacheinander, jedes wie es eingerichtet ist; aktiviert dafür das jeweilige Modell und am Ende (auch nach Abbrechen) wieder das vorher aktive |
| `set_key` | `api_key` | trägt den Key in alle Modell-Konfigurationen ein und startet Strata neu |
| `set_speed_projection` | `model`, `on` | schaltet die Speed-Projection in der Konfiguration dieses Modells ein oder aus, ohne Setup; beim aktiven Modell wartet er, bis Strata keine Anfrage bearbeitet, und startet es neu |
| `set_context` | `model`, `context` | setzt den Kontext in der Konfiguration dieses Modells, ohne Setup; beim aktiven Modell wartet er, bis Strata keine Anfrage bearbeitet, und startet es neu |

Den Strata-Key erzeugt der Proxy. Er gilt erst, wenn der Vorgang fertig ist
(auch über einen Neustart des Proxys); scheitert er, bleibt der alte. In der
Ausgabe des Vorgangs steht statt des Keys `***`.

`experimental_speed_projection` (ohne Angabe `false`) gibt Setup bei Qwen und
Coder `--experimental-speed-projection on` statt `off` mit. Bei anderen
Familien ist `true` ein `400`. Der Proxy schickt das Feld nur, wenn es
angehakt ist, denn ein älterer PC-Dienst lehnt unbekannte Felder ab.

`set_speed_projection` ändert nur `args` in `strata-<id>.json`. `on: true`
nimmt Setups Flags heraus und hängt sie neu an, mit Stratas Vektor aus dem
Strata-Ordner (siehe [strata-api.md](strata-api.md)); fehlt der Vektor,
scheitert der Vorgang und die Konfiguration bleibt. `on: false` nimmt die
vier Flags mit ihren Werten heraus, auch einen eigenen Vektor. Andere
Familien als Qwen und Coder sind ein `400`, ein Modell aus dem Katalog, das
nicht installiert ist, wie bei `activate` ein `409`. Die Änderungszeit der
Konfiguration bleibt, denn ohne eigenes Startskript startet Strata die
zuletzt geänderte.

`set_context` setzt nur den Wert von `--max-context` in `args` (fehlt das
Flag, hängt er es an) und nimmt `--rope-scaling` und `--rope-scale` mit ihren
Werten heraus. Er nimmt nur Kontexte aus dem Katalog bis 262144, der
trainierten Länge: darüber braucht das Modell Rope-Scaling, das nur Setup
einrichtet (`400`). Was Setup aus dem alten Kontext abgeleitet hat
(KV-Streaming mit `--kv-resident`, `--kv`, parallele Slots, Kalibrierung),
bleibt. Ein Modell aus dem Katalog, das nicht installiert ist, ist ein
`409`. Die Änderungszeit bleibt wie bei `set_speed_projection`.

Beim aktiven Modell wartet der Vorgang nach dem Neustart, bis Strata auf
`/health` antwortet. Endet Strata vorher oder antwortet es nicht innerhalb
von `context_start_secs` (Standard 600), schreibt er die alten `args` zurück,
startet Strata damit neu und scheitert; die letzten Zeilen der
Strata-Ausgabe stehen dann in `lines` des Vorgangs. Wird das Warten
abgebrochen, kommen die alten `args` ebenso zurück. Im Gaming-Modus startet
Strata nicht neu, und es wird nicht gewartet.

### `GET /strata/job`

Der laufende oder letzte Vorgang: `state` (`running`, `done`, `failed`,
`cancelled`, `interrupted` nach einem Neustart), Schritt, Fortschritt mit
Bytes, Tempo und Restzeit, die letzten 20 Ausgabezeilen, `error`, `result`.

### `POST /strata/job/cancel`

Bricht den laufenden Vorgang ab (`202`), sonst `409`.
