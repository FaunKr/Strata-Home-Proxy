# Schnittstelle zwischen NAS-Proxy und PC-Dienst

Der PC-Dienst bietet diese Endpunkte an, der Proxy nutzt sie. Was jeder
Endpunkt nimmt und antwortet, steht als Typen im Crate `strata-pc-protocol`
(`crates/pc-protocol`); dieses Dokument beschreibt, was dahinter passiert.
Gespeicherte Antworten des heutigen PC-Dienstes liegen in
`crates/pc-protocol/tests/samples`.

## Ältere PC-Dienste

Der PC-Dienst aktualisiert sich vom NAS, also spricht oft ein älterer
PC-Dienst mit einem neueren Proxy. Deshalb:

- Felder kommen nur dazu; keins wird umbenannt oder entfernt.
- Ein Antwortfeld, das nach der ersten Version dazukam, hat einen
  Standardwert, damit die Antwort eines älteren PC-Dienstes lesbar bleibt.
- Ein optionales Anfragefeld fehlt, solange es nicht gesetzt ist: Der
  PC-Dienst lehnt Anfragefelder ab, die er nicht kennt. Arten ohne Felder
  (`update`, `benchmark`, `compare_models`) ignorieren zusätzliche Felder.

Die Tests des Crates lesen die gespeicherten Antworten und dazu Antworten,
denen die neueren Felder fehlen. Ein Test des PC-Dienstes prüft, dass die
Beispiele jedes Feld zeigen, das er sendet; ein neues Feld kommt also mit
seinem Beispiel. `scripts/fake-pc.js` baut auf denselben Beispielen auf.

Alle Anfragen tragen `Authorization: Bearer <PC_AGENT_TOKEN>`; der Dienst
verlangt das Token immer. Er nimmt Anfragen nur von `allowed_clients` (dem
NAS) und von 127.0.0.1 an.

## `GET /status`

Antwort: `PcStatus`.

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
30 s Ruhe startet die Messung und bekommt nur `null`-Werte (`Hardware`).

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
Die Antwort (`Answer`) kommt zuerst, Standby folgt 2 s später.

- `200`, angenommen: Standby wird ausgelöst.
- `409`, abgelehnt mit Grund, z. B. „Benutzer aktiv“. Der Proxy fragt nach
  `SLEEP_RETRY_SECS` erneut.

## `POST /update` (für die Weboberfläche)

Lädt `strata-pc-agent.exe` von `nas_url` aus der Konfiguration
(`/admin/agent/strata-pc-agent.exe`), benennt die laufende .exe in
`strata-pc-agent.exe.alt` um, legt die neue an ihre Stelle und startet sie
mit denselben Argumenten. Die Antwort kommt danach; 1 s später beendet sich
der alte Dienst, der neue wartet so lange auf den Port und löscht dann die
`.alt`.

- `202` (`Answer`): die neue .exe läuft gleich.
  Ab dem Download bis zum Ende lehnt der Dienst Strata-Vorgänge und Schlafen
  ab.
- `409` (`Refused` mit Grund): nicht jetzt (Strata bearbeitet eine Anfrage, ein
  Strata-Vorgang oder ein Update läuft) oder ohne `nas_url` eingerichtet.
- `500` (`Refused`): Download oder Austausch gescheitert; der alte Dienst
  läuft weiter, nichts ist verändert.

Der Proxy schickt das nur bei wachem PC und weckt ihn dafür nie. Ein Dienst
ohne diesen Endpunkt antwortet 404; der Bereich *PC* sagt dann, ihn einmal
mit dem Befehl von dort neu einzurichten.

## `POST /gaming` (für die Weboberfläche)

Anfrage: `GamingRequest`, Antwort: `PcStatus`. An: Strata beenden, Schlafen
sperren. Aus: Strata starten, Schlafen erlauben.

## `GET /strata-log` (für den Reiter *Output* im Bereich *Strata*)

Die Strata-Ausgabe als Server-Sent Events: was ein vom Dienst gestartetes
Strata auf seine Konsole schreibt. Zuerst kommen die gemerkten Zeilen (höchstens
500, Trennzeilen eingeschlossen), dann jede neue, dazu jeder Wechsel des
Hinweises. Jede Nachricht ist eine `StrataOutputMessage` als JSON in
`data:`; `replace` ersetzt die letzte Zeile (Fortschritt, der in der Konsole
dieselbe Zeile mit `\r` überschreibt), `notice` sagt, ob es Ausgabe gibt
und warum nicht. `not_started_by_agent`: Strata antwortet, wurde aber nicht
vom Dienst gestartet, also kann er seine Ausgabe nicht lesen.

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

Antwort: `StrataReport`, also `StrataInfo` und daneben `update`. Was
installiert ist, Stratas Katalog mit Empfehlung für den RAM des PCs,
freier Platz, letzter Benchmark und der laufende oder letzte Vorgang.
Enthält nie den Strata-Key. Fragt nebenbei höchstens einmal pro Stunde Git,
ob es eine neuere Strata-Version gibt (`update_available`).

Kommen Commits herein, steht in `update` (`Incoming`), was sie bringen; ohne
Update, vor der ersten Prüfung und bei einem älteren PC-Dienst fehlt das
Feld. `commits` sind die neuesten zuerst, höchstens 200 (`total` zählt alle).
`files` kommt aus `git diff --numstat HEAD..@{u}`; Binärdateien zählen 0
Zeilen. Der Proxy behält das alles auf dem NAS; die Weboberfläche bekommt nur
Kurz-Hash, Betreff und Datum.

Jedes installierte Modell sagt mit `speed_projection`, ob seine Konfiguration
einen Steuervektor lädt (`--control-vector-scaled`, Stratas oder ein eigener).
Ein älterer PC-Dienst lässt das Feld weg.

`context` ist der Wert von `--max-context` in seiner Konfiguration (Tokens),
`null`, wenn das Flag fehlt oder sein Wert keine Zahl ist. Ein älterer
PC-Dienst lässt das Feld weg.

`loop_recovery` ist `reasoning_loop_recovery` aus seiner Konfiguration, wie
Strata es liest: `recover` (auch `true`), `stop` oder `off` (fehlt der Wert
oder ist er unbekannt). Ein älterer PC-Dienst lässt das Feld weg.

`sampling` sagt, was eine Anfrage ohne Sampling-Felder bekommt: `support`
ist `model_card`, `config` oder `none` (dieses Strata liest keins und ist
greedy), `thinking` und `instruct` sind Listen von `{"name", "value",
"source"}` mit `source` `model_card`, `config` oder `chat_settings`, in der
Reihenfolge temperature, top_p, top_k, min_p, presence_penalty,
repetition_penalty, frequency_penalty. Eine leere Liste ist greedy. Aus den
Chat-Einstellungen (`strata-<id>.shared-settings.json`, Stratas Web-App)
zählen temperature, top_p und top_k; der PC-Dienst liest die Datei nur und
nimmt eine, die kein gültiges JSON ist, wie Strata als leer. `sampling` ist
`null`, solange `serve/server.py` nicht lesbar ist. Ein älterer PC-Dienst
lässt das Feld weg.

`benchmarks` hält den letzten Lauf jedes Modells: `decode_tps` (`chat decode
tok/s`), `prompt_tps` (`4K prompt tok/s`, fehlt, wenn der Benchmark die Zeile
nicht druckt), dazu `context`, `speed_projection` und `version` (Strata-Commit)
zur Zeit des Laufs. Ein Modell, das im Vergleich nicht anlief oder nichts
meldete, hat statt der Werte `error`. `benchmark` bleibt der letzte
erfolgreiche Lauf, für Proxys, die nur einen kennen. Ein älterer PC-Dienst
lässt `benchmarks` weg.

### Was der PC-Dienst in den Modell-Konfigurationen hält

Beim Start des PC-Dienstes, nach `install`, `add_model` und `update` setzt er
in jeder `strata-<id>.json` `"reasoning_loop_recovery": "recover"` (siehe
[strata-api.md](strata-api.md)) und überschreibt einen anderen Wert. Den
Block `sampling` setzt er auf den Denk-Satz der Modellkarte, wenn das
installierte Strata nur den Block kennt; kennt es die Modellkarte oder den
Block nicht, nimmt er ihn heraus. Ein Block von Hand wird dabei ersetzt.
Ist `serve/server.py` nicht lesbar, bleibt der Block, wie er ist. Seine
Änderungen macht er beim Start, bevor er Vorgänge annimmt. Alles
andere in der Konfiguration bleibt, auch ihre Änderungszeit. Eine
Konfiguration, die er nicht lesen oder schreiben kann, steht im Log und
bleibt, wie sie ist. Ändert sich die Konfiguration des aktiven Modells,
während Strata läuft, startet er Strata neu, sobald es keine Anfrage
bearbeitet; nach `install` und `update` startet Strata ohnehin mit ihr.

### `POST /strata/jobs`

Anfrage: `StrataJobRequest`, ein Fall pro Art. Antwort `202` mit dem
Vorgang (`StrataJob`). `400` (`Refused`) bei Werten außerhalb des Katalogs,
`409`, wenn es gerade nicht geht (Gaming an, ein Vorgang läuft, zu wenig
Platz, aktives Modell löschen).

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
angehakt ist (siehe „Ältere PC-Dienste“).

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
trainierten Länge
(`MAX_SWITCHED_CONTEXT`): darüber braucht das Modell Rope-Scaling, das nur Setup
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

Der laufende oder letzte Vorgang (`StrataJob`, `null` ohne einen): `state` (`running`, `done`, `failed`,
`cancelled`, `interrupted` nach einem Neustart), Schritt, Fortschritt mit
Bytes, Tempo und Restzeit, die letzten 20 Ausgabezeilen, `error`, `result`.

### `POST /strata/job/cancel`

Bricht den laufenden Vorgang ab (`202` mit dem Vorgang), sonst `409`
(`Refused`).
