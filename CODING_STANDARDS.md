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

## Proxy und PC

- **`/admin` weckt den PC nie** und setzt die Zeit bis zum Standby nicht
  zurück. Server-Funktionen prüft `no_server_function_wakes_the_pc` von selbst;
  eine eigene Route wie `overview-stream` braucht einen eigenen Test.
- **Timer in Tests mit Abstand.** Muss ein Test erst etwas sehen, bevor ein
  Timer abläuft (Leerlaufzeit, Nachlauf), liegt der Timer mindestens 2 s
  dahinter; der CI-Runner ist langsamer als die Session. Knappe Timer nur,
  wenn der Test danach nur noch wartet.
- **Vorrang der Einstellungen:** vom PC gemeldete MAC, dann Wert von der
  Seite, dann Umgebungsvariable, dann Standardwert. Neue Einstellungen folgen
  derselben Reihenfolge und werden live über `AppState::cfg()` gelesen.
- **Strata-Vorgänge führen nur feste Programme aus.** Der PC-Dienst nimmt
  Werte nur aus `catalog.rs` und prüft Modell-IDs gegen den Katalog, bevor sie
  in einen Pfad gehen. Der Strata-Key steht in keiner Antwort an die
  Weboberfläche und in keinem gespeicherten Vorgang.
