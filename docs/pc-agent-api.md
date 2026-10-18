# Schnittstelle zwischen NAS-Proxy und PC-Dienst

Der Dienst auf dem PC (Schritt 3) bietet diese Endpunkte an. Der Proxy nutzt
sie schon jetzt; die Tests bilden den Dienst nach.

Alle Anfragen tragen `Authorization: Bearer <PC_AGENT_TOKEN>`; der Dienst
verlangt das Token immer. Er nimmt Anfragen nur von `allowed_clients` (dem
NAS) und von 127.0.0.1 an.

## `GET /status`

```json
{ "gaming_mode": false, "strata_running": true, "user_idle_secs": 1840, "mac": "AA:BB:CC:DD:EE:FF", "strata_job": false, "version": "0.1.0+558db97" }
```

`strata_job` ist `true`, solange ein Strata-Vorgang läuft (siehe unten).

`version` ist die Crate-Version plus der Commit des Baus (`STRATA_BUILD_ID`
beim Bauen, sonst `dev`). Dieselbe Angabe steht als Text
`strata-pc-agent-version:<version>` in der .exe; so liest der Proxy die
Version der .exe in seinem Image, ohne sie auszuführen. Weicht sie ab oder
fehlt `version` (Dienst von vor dem Update-Knopf), zeigt die Übersicht
„Update verfügbar“.

`mac` ist die MAC-Adresse der Netzwerkkarte, über die die Anfrage kam (also
die, die das Weckpaket empfangen muss), oder der Wert `mac` aus der
Konfiguration. Ist sie unbekannt, fehlt das Feld. Der Proxy merkt sich die
gemeldete MAC in `DATA_DIR` und weckt damit; sie hat Vorrang vor `PC_MAC` und
der Einstellungsseite.

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
- `409 {"reason": …}`: nicht jetzt (Strata bearbeitet eine Anfrage, ein
  Strata-Vorgang oder ein Update läuft) oder ohne `nas_url` eingerichtet.
- `500 {"reason": …}`: Download oder Austausch gescheitert; der alte Dienst
  läuft weiter, nichts ist verändert.

Der Proxy schickt das nur bei wachem PC und weckt ihn dafür nie. Ein Dienst
ohne diesen Endpunkt antwortet 404; die Übersicht sagt dann, ihn einmal mit
dem Befehl von der Seite PC-Dienst neu einzurichten.

## `POST /gaming` (für die Weboberfläche)

```json
{ "enabled": true }
```

An: Strata beenden, Schlafen sperren. Aus: Strata starten, Schlafen erlauben.

## Strata verwalten (für die Seite „Strata“)

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

### `POST /strata/jobs`

Startet einen Vorgang, Antwort `202` mit dem Vorgang. `400 {"reason": …}`
bei Werten außerhalb des Katalogs, `409 {"reason": …}`, wenn es gerade nicht
geht (Gaming an, ein Vorgang läuft, zu wenig Platz, aktives Modell löschen).

| `kind` | Felder | Was passiert |
| --- | --- | --- |
| `install` | `family`, `size`, `context`, `api_key` | Git (per winget, falls nötig), `git clone`, Setup ohne Fragen, startet Strata |
| `add_model` | `family`, `size`, `context`, optional `api_key` | Setup mit `--setup` und dem Key des Proxys (sonst dem eigenen), das aktive Modell bleibt aktiv |
| `activate` | `model` | wartet, bis Strata keine Anfrage bearbeitet, startet mit dem anderen Startskript neu |
| `delete` | `model` | löscht Modellordner, Pack, Konfiguration, Startskript und Log dieses Modells |
| `update` | – | `git pull --ff-only`, dann `START-HERE.bat --update` bei beendetem Strata, startet neu |
| `benchmark` | – | `BENCH.bat --config <aktives Modell>`, behält Tokens/s mit Modell und Datum |
| `set_key` | `api_key` | trägt den Key in alle Modell-Konfigurationen ein und startet Strata neu |

Den Strata-Key erzeugt der Proxy. Er gilt erst, wenn der Vorgang fertig ist
(auch über einen Neustart des Proxys); scheitert er, bleibt der alte. In der
Ausgabe des Vorgangs steht statt des Keys `***`.

### `GET /strata/job`

Der laufende oder letzte Vorgang: `state` (`running`, `done`, `failed`,
`cancelled`, `interrupted` nach einem Neustart), Schritt, Fortschritt mit
Bytes, Tempo und Restzeit, die letzten 20 Ausgabezeilen, `error`, `result`.

### `POST /strata/job/cancel`

Bricht den laufenden Vorgang ab (`202`), sonst `409`.
