# Coding Standards

Regeln für das Review. Was ein Test oder Clippy schon prüft, steht hier nicht.
Verhalten von Strata (Endpunkte, Auth, Metriken): `docs/strata-api.md`.
Schon entschiedene Fragen: `docs/adr/`.

## Weboberfläche und Login

- **Sitzungen enden auf dem Server.** Abmelden, Passwort ändern und das Löschen
  von `admin-password.json` machen bestehende Cookies ungültig, auch kopierte.
  Ein Cookie nur im Browser zu löschen reicht nicht.
- **Tokens sind eindeutig.** Zwei Anmeldungen in derselben Sekunde ergeben
  verschiedene Tokens; ein Widerruf trifft nur die eigene Sitzung.
- **Jede Passwortprüfung zählt zur Sperre**, auch die des aktuellen Passworts
  beim Ändern.
- **Fehler beim Speichern werden gemeldet.** Ein Wert, der nicht in `DATA_DIR`
  landet, gilt nicht als gespeichert, und die Seite sagt das.
- **Kein veralteter Stand überschreibt einen neueren.** Wer eine Kopie unter
  einem Lock zieht und außerhalb schreibt, hält dabei eine Schreibsperre, die
  auch jeder andere Write derselben Datei nimmt.
- **Was Leptos beim Bau des Routers tut, geschieht einmal pro Prozess.** Das
  Erzeugen der Routenliste setzt einen prozessweiten Schalter, der jede
  Ressource anhält, die währenddessen startet (#69). Neuer Code, der das
  bei jedem Router-Bau wiederholt, braucht denselben `OnceLock` wie
  `server::router`.
- **Eine Ressource entsteht nicht im Inhalt eines `Suspend`.** Wird sie dort
  angelegt, hydratisiert die Seite nicht („expected a marker node“). Die
  Komponente legt sie an und zeigt sie neben dem `Suspend`, wie
  `CodeBenchCard` neben `with_page`.
- **Server-Funktionen lesen Kontext ohne Panik** (`backend()?`, `use_context`).
  Der Server zeichnet den Inhalt einer `Transition` pro Anfrage mehrmals; die
  Ressourcen der verworfenen Durchläufe laufen weiter, ohne Kontext. Eine
  Panik dort lässt die Antwort nie enden.

## Proxy und PC

- **`/admin` weckt den PC nie** und setzt die Zeit bis zum Standby nicht
  zurück. Server-Funktionen prüft `no_server_function_wakes_the_pc` von selbst;
  eine eigene Route wie `overview-stream` braucht einen eigenen Test.
- **Timer in Tests mit Abstand.** Muss ein Test erst etwas sehen, bevor ein
  Timer abläuft (Leerlaufzeit, Nachlauf), liegt der Timer mindestens 2 s
  dahinter; der CI-Runner ist langsamer als die Session. Knappe Timer nur,
  wenn der Test danach nur noch wartet.
- **Wartezeiten, die ein Test abwarten müsste, sind einstellbar.** Eine Grenze
  oder Wiederholzeit, von der Verhalten abhängt, kommt aus `Config` (wie
  `idle_timeout`) oder über einen Setter für Tests (wie `Monitor::set_poll`,
  `UpdateSummaries::set_timing`), nicht aus einer festen `const`. Sonst bleibt
  das Verhalten ungetestet.
- **Vorrang der Einstellungen:** vom PC gemeldete MAC, dann Wert von der
  Seite, dann Umgebungsvariable, dann Standardwert. Neue Einstellungen folgen
  derselben Reihenfolge und werden live über `AppState::cfg()` gelesen. Das
  gilt für Werte aus den Einstellungs-Abschnitten der Weboberfläche, nicht für feste Pfade im Image wie
  `LEPTOS_SITE_ROOT` oder `PC_AGENT_EXE`.
- **Strata-Vorgänge führen nur feste Programme aus.** Der PC-Dienst nimmt
  Werte nur aus `catalog.rs` und prüft Modell-IDs gegen den Katalog, bevor sie
  in einen Pfad gehen. Der Strata-Key steht in keiner Antwort an die
  Weboberfläche und in keinem gespeicherten Vorgang.
- **Clients für Dienste auf demselben Rechner setzen ein kurzes
  `connect_timeout`** (wie `STRATA_CONNECT_TIMEOUT` in `pc-agent/src/agent.rs`).
  Windows wiederholt eine abgewiesene Verbindung zu localhost rund 2 s lang;
  ohne Grenze wartet jede Abfrage eines gestoppten Strata so lange.
- **Wird ein Feld optional, geht das Review jeden Leser durch.** Wer bisher
  einen Wert voraussetzte, braucht einen Weg für „fehlt“, der zum Zustand
  passt (fehlendes Startskript heißt nicht „PC schläft“).

## Code-Benchmark (`tools/code-bench`)

- **Jede Änderung am Runner oder an seinem Aufruf durch den Proxy geht durch
  das Bedrohungsmodell** in `tools/code-bench/README.md`: Erreicht fremder Code
  danach etwas aus der Tabelle, oder fällt eine der Schranken weg? Ein neuer
  Zugang (Port, Pfad, Nutzer, Mount) bekommt eine Zeile mit seiner Schranke.

## Installationsskript (`deploy/windows/install.ps1`)

Es läuft in Windows PowerShell 5.1, nicht in PowerShell 7:

- **Dateien für den PC-Dienst ohne BOM schreiben**
  (`[IO.File]::WriteAllLines`). `Set-Content -Encoding UTF8` setzt eins davor.
- **Fehler des NAS kommen als Skript mit Meldung**, nicht als Fehlerstatus:
  `irm` zeigt bei einem Fehlerstatus nur „(403)“ und keinen Text.
- **Was der Einzeiler braucht, setzt der NAS ins Skript ein.** `irm … | iex`
  nimmt keine Parameter.
- **Erst laden, dann den laufenden Dienst beenden**, und alles, was scheitern
  kann, vor dem Verbrauch des Einmalcodes prüfen.
