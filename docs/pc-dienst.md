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

1. In GitHub unter *Actions* den letzten grünen CI-Lauf öffnen und unten
   **strata-pc-agent-windows** herunterladen, entpacken.
2. PowerShell **als Administrator** im entpackten Ordner:
   ```powershell
   powershell -ExecutionPolicy Bypass -File install.ps1 -NasIp 192.168.1.10
   ```
   Das legt `%LOCALAPPDATA%\StrataHome` an, erzeugt ein Token und zeigt es an,
   richtet den Start bei Anmeldung ein, öffnet Port 8091 nur für das NAS und
   legt die Desktop-Verknüpfung **Gaming an-aus** an.
3. `%LOCALAPPDATA%\StrataHome\strata-pc-agent.toml` öffnen und prüfen:
   - `strata_start`: Pfad zu `START-HERE.bat`
   - `strata_processes`: im Task-Manager unter *Details* nachsehen, welche
     Prozesse zu Strata gehören, während es läuft
   - `strata_api_key`: nur falls `/health` ohne Key nicht antwortet
   Danach in der Aufgabenplanung „Strata PC-Dienst“ beenden und neu starten.
4. Auf dem NAS in der App `PC_AGENT_URL` (`http://<PC-IP>:8091`) und
   `PC_AGENT_TOKEN` (das Token aus Schritt 2) eintragen.

Der Dienst schreibt nach `%LOCALAPPDATA%\StrataHome\strata-pc-agent.log`.

## Hinweise

- Der Dienst läuft im Benutzerkonto, nicht als Windows-Dienst: nur so sieht
  er Maus und Tastatur und kann Strata in einem normalen Fenster starten.
  Nach einem Neustart muss sich daher jemand anmelden (oder die automatische
  Anmeldung von Windows ist eingerichtet).
- Mit der RX 6800 XT hängt die erste Anfrage nach dem Standby, wenn Strata
  geladen bleibt; nach einem Neustart von Strata klappt sie. Darum steht
  `fresh_strata_after_standby = true` in der Vorlage: Strata wird vor dem
  Standby beendet und danach neu gestartet. Aufwachen dauert so 1–3 min.
- Ein Strata, das du selbst gestartet hast, bleibt in Ruhe, solange es antwortet.
