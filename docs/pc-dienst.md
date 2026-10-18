# PC-Dienst und Gaming-Schalter

`strata-pc-agent.exe` läuft unsichtbar im Hintergrund, sobald du dich
anmeldest. Er

- legt den PC in Standby, wenn das NAS darum bittet und nichts dagegen
  spricht: Gaming-Modus an, jemand hat in den letzten 10 min Maus oder
  Tastatur benutzt, oder Strata rechnet gerade;
- schaltet den Gaming-Modus: **an** beendet Strata und sperrt das Schlafen,
  **aus** startet Strata wieder. Der Zustand übersteht einen Neustart;
- startet Strata, wenn es nicht läuft, und startet es neu, wenn es 60 s lang
  nicht auf `/health` antwortet (z. B. nach dem Standby). Nach jedem Start
  wartet er 4 min, bis das Modell geladen ist.

## Installieren

1. In der Weboberfläche des NAS die Seite **PC-Dienst** öffnen und
   **Befehl erzeugen** drücken.
2. Den Befehl in eine PowerShell auf dem PC einfügen, z. B.:
   ```powershell
   irm http://192.168.1.10:8090/admin/agent/install.ps1?code=K7F2Q9AB | iex
   ```
   Windows fragt einmal nach Administratorrechten. Das Skript lädt
   `strata-pc-agent.exe` vom NAS nach `%LOCALAPPDATA%\StrataHome`, meldet den PC
   beim NAS an, richtet den Start bei Anmeldung ein, öffnet Port 8091 nur für
   das NAS und legt die Desktop-Verknüpfung **Gaming an-aus** an. Token,
   `PC_AGENT_URL` und den Strata-Key trägt der NAS selbst ein.
3. Startskript und Strata-Programme setzt die Seite **Strata**. Bis dahin
   läuft der Dienst, startet Strata aber nicht selbst.

Der Code im Befehl gilt 15 Minuten und für einen PC; ein neuer Befehl macht
den alten ungültig. Auf einem PC, auf dem der Dienst schon läuft, tauscht ein
neuer Befehl die .exe aus und meldet den PC neu an (neues Token, z. B. nachdem
auf dem NAS Token oder Adresse zurückgesetzt wurden); die übrige Konfiguration
bleibt.

**Entfernen:** Die Seite zeigt nach *Befehl erzeugen* auch den Befehl dazu
(`irm http://<NAS>:<Port>/admin/agent/install.ps1?uninstall | iex`). Er entfernt
Aufgabe, Firewall-Regel, Verknüpfung und Ordner. Auf dem NAS danach unter
*Einstellungen* `PC_AGENT_URL` und `PC_AGENT_TOKEN` zurücksetzen.

### Ohne NAS

1. In GitHub unter *Actions* den letzten grünen CI-Lauf öffnen und unten
   **strata-pc-agent-windows** herunterladen, entpacken.
2. PowerShell im entpackten Ordner:
   ```powershell
   powershell -ExecutionPolicy Bypass -File install.ps1 -NasIp 192.168.1.10
   ```
   Das erzeugt ein Token und zeigt es an; sonst wie oben.
3. `%LOCALAPPDATA%\StrataHome\strata-pc-agent.toml` öffnen und prüfen:
   - `strata_start`: Pfad zum Startskript von Strata
   - `strata_processes`: der Strata-Ordner mit `\` am Ende (Vorgabe
     `C:\Strata\`). Der Dienst beendet dann jedes Programm aus diesem Ordner
     samt allem, was es gestartet hat; nur `strata.exe` zu beenden reicht
     nicht, weil Strata die Engine nach einem Absturz neu startet
   - `strata_api_key`: nur falls `/health` ohne Key nicht antwortet
   Danach in der Aufgabenplanung „Strata PC-Dienst“ beenden und neu starten.
4. Auf dem NAS unter *Einstellungen* `PC_AGENT_URL` (`http://<PC-IP>:8091`) und
   `PC_AGENT_TOKEN` (das Token aus Schritt 2) eintragen.

Entfernen: `powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall`.

Der Dienst schreibt nach `%LOCALAPPDATA%\StrataHome\strata-pc-agent.log`.

## Strata-Ausgabe

Der Dienst startet Strata ohne Konsolenfenster und liest dessen Ausgabe
selbst mit. Die Zeilen des laufenden Strata stehen in
`%LOCALAPPDATA%\StrataHome\strata.log` (bei jedem Start neu), die letzten
500 zeigt die Weboberfläche live auf der Seite „Strata-Ausgabe“ (Knopf auf
der Übersicht). Python-Programme bekommen `PYTHONUNBUFFERED=1`, damit die
Ausgabe sofort kommt und nicht in Schüben. Ein Strata, das du selbst per
Doppelklick startest, läuft in seinem eigenen Fenster; dessen Ausgabe sieht
der Dienst nicht. Startet das Startskript Strata seinerseits mit `start` in
einem eigenen Fenster, gilt das ebenso. Der Strata-Key steht in der Ausgabe
als `***`.

Strata schreibt seine Ausgabe in den Dienst. Wird der Dienst beendet oder neu
gestartet, während Strata läuft, laufen Stratas Ausgaben ins Leere; stürzt
Strata daran ab, startet der neu gestartete Dienst es wieder.

## Verwaltete Werte

Richtet der Dienst Strata selbst ein (Seite „Strata“ der Weboberfläche),
speichert er Startskript, Strata-Ordner und Strata-Key in
`strata-managed.json` neben der TOML. Diese Werte gehen vor `strata_start`,
`strata_args`, `strata_processes` und `strata_api_key` aus der TOML; die TOML
selbst bleibt unverändert. Ohne diese Datei gilt die TOML wie bisher. Wer
wieder von Hand einrichten will, löscht die Datei und startet den Dienst neu.

## Hinweise

- Der Dienst läuft im Benutzerkonto, nicht als Windows-Dienst: nur so sieht
  er Maus und Tastatur und kann Strata in deiner Sitzung starten.
  Nach einem Neustart muss sich daher jemand anmelden (oder die automatische
  Anmeldung von Windows ist eingerichtet).
- Mit der RX 6800 XT hängt die erste Anfrage nach dem Standby, wenn Strata
  geladen bleibt; nach einem Neustart von Strata klappt sie. Darum steht
  `fresh_strata_after_standby = true` in der Vorlage: Strata wird vor dem
  Standby beendet und danach neu gestartet. Aufwachen dauert so 1–3 min.
- Ein Strata, das du selbst gestartet hast, bleibt in Ruhe, solange es antwortet.
