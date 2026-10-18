# Coding Standards

Regeln für das Review. Was ein Test oder Clippy schon prüft, steht hier nicht.

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
- **Vorrang der Einstellungen:** vom PC gemeldete MAC, dann Wert von der
  Seite, dann Umgebungsvariable, dann Standardwert. Neue Einstellungen folgen
  derselben Reihenfolge und werden live über `AppState::cfg()` gelesen.
