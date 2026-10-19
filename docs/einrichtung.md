# Einmalige Einrichtung

Zum Abhaken, bevor der Proxy auf dem NAS läuft. Reihenfolge so lassen: erst
prüfen, ob Standby und Wecken überhaupt klappen, dann Strata, dann das Netz.

## 1. BIOS (ASRock B550 Phantom Gaming 4)

Die Menünamen können je nach BIOS-Version leicht abweichen.

- [ ] **Advanced → ACPI Configuration → PCIE Devices Power On** auf *Enabled* (das ist Wake on LAN für die Onboard-Netzwerkkarte)
- [ ] **Advanced → ACPI Configuration → Suspend to RAM** auf *Enabled* bzw. *Auto* (Standby S3)
- [ ] **Advanced → PCI Configuration → Above 4G Decoding** und **Re-Size BAR Support** auf *Enabled* (Resizable BAR für Spiele)
- [ ] **Boot → Fast Boot** auf *Disabled*, falls Wecken später nicht klappt (bei manchen Boards bleibt die Netzwerkkarte sonst aus)

## 2. Windows

In einer PowerShell als Administrator:

```powershell
# Ruhezustand aus, damit "Schlafen" wirklich Standby ist (spart ~64 GB auf der NVMe)
powercfg -h off

# Prüfen, welche Schlafzustände das System kann: "Standby (S3)" muss dabei sein
powercfg /a

# Netzwerkkarte, Name und MAC-Adresse anzeigen
Get-NetAdapter | Where-Object Status -eq Up | Format-Table Name, InterfaceDescription, MacAddress, LinkSpeed

# Wake on Magic Packet einschalten (Name aus der Zeile oben, meist "Ethernet")
Set-NetAdapterPowerManagement -Name "Ethernet" -WakeOnMagicPacket Enabled
Get-NetAdapterAdvancedProperty -Name "Ethernet" | Where-Object DisplayName -match "Wake|Magic|Energy|EEE"
```

- [ ] `powercfg /a` zeigt **Standby (S3)** als verfügbar. Steht dort nur *Modern Standby (S0)*: melden, dann muss der Plan angepasst werden.
- [ ] In den erweiterten Eigenschaften der Netzwerkkarte: *Wake on Magic Packet* = Enabled, *Energy Efficient Ethernet* bzw. *Green Ethernet* = Disabled (verhindert sonst oft das Wecken)
- [ ] Geräte-Manager → Netzwerkkarte → Energieverwaltung: „Gerät kann den Computer aus dem Ruhezustand aktivieren“ und „Nur Magic Packet …“ angehakt
- [ ] Systemsteuerung → Energieoptionen → „Auswählen, was beim Drücken des Netzschalters geschehen soll“ → **Schnellstart aus**
- [ ] Energieoptionen: automatisches Standby von Windows auf *Nie* (das übernimmt später der Proxy)
- [ ] MAC-Adresse notiert: `__-__-__-__-__-__`

## 3. Netz

- [ ] PC per **LAN-Kabel** am Router oder Switch (WLAN weckt nicht zuverlässig)
- [ ] Im Router eine **feste IP** für den PC vergeben (DHCP-Reservierung): `192.168.__.__`
- [ ] NAS und PC im **selben Netz** (gleicher Adressbereich, kein Gast-WLAN/VLAN dazwischen)
- [ ] Broadcast-Adresse des Netzes notiert, meist die IP mit `.255` am Ende: `192.168.__.255`

## 4. Wecken von Hand testen

Bevor irgendetwas anderes gebaut wird, muss das hier klappen:

1. PC über Start → Energie → *Energie sparen* in Standby schicken. Lüfter aus, Power-LED blinkt oder ist aus.
2. Vom NAS aus ein Weckpaket schicken. Dafür reicht das Image des Proxys,
   Strata muss noch nicht laufen. Per SSH auf dem NAS (MAC und Broadcast
   anpassen):
   ```sh
   docker run --rm --network host \
     -e PC_MAC=AA-BB-CC-DD-EE-FF -e WOL_ADDR=192.168.1.255:9 \
     ghcr.io/faunkr/strata-home-proxy:latest wake
   ```
3. Zeit stoppen, bis der Bildschirm wieder da ist.

- [ ] PC wacht auf, Zeit: ____ s
- [ ] Fünfmal hintereinander klappt es

Klappt es nicht: BIOS-Punkte aus Schritt 1 und *Energy Efficient Ethernet* prüfen. Hilft das nicht, mit `powercfg -lastwake` nachsehen, was Windows meldet.

## 5. Strata

Einfacher Weg, sobald Proxy und PC-Dienst laufen: Weboberfläche, Bereich
**Strata**. Der Knopf weckt den PC, holt Strata, richtet es mit dem
empfohlenen Modell ein und trägt den Strata-Key in Strata und Proxy ein.
Die Empfehlung folgt Stratas eigenem Setup: ab 60 GB RAM `IQ3_XXS`, ab 44 GB
`Q2_0`, ab 28 GB `coder-IQ1_M`; Kontext 64K für eine 16-GB-Karte. Der Skill
`local-agent` braucht mehr Kontext (siehe sein README).

- [ ] Bereich *Strata*: „Set up Strata“ läuft durch, das Modell lädt auf der RX 6800 XT
- [ ] „Run benchmark“ (Reiter *Version & benchmark*) liefert Tokens/s: ____
- [ ] `curl http://<PC-IP>:8080/health` vom NAS aus liefert `200`
- [ ] Standby-Test: PC schlafen legen, wecken, dann noch einmal `/health` abfragen. Antwortet Strata danach nicht mehr, notieren (dafür ist der Watchdog im PC-Dienst gedacht)

Von Hand geht es weiter so: `START-HERE.bat` mit `--host 0.0.0.0`, Port
**8080** und **API-Key**, den Key dann in der Weboberfläche unter *Strata → Connection* eintragen.

## 6. Firewall auf dem PC

Port 8080 nur für das NAS freigeben (IP anpassen):

```powershell
New-NetFirewallRule -DisplayName "Strata nur vom NAS" -Direction Inbound -Protocol TCP -LocalPort 8080 -RemoteAddress 192.168.1.10 -Action Allow
```

- [ ] Regel angelegt, ggf. ältere, offenere Regeln für 8080 entfernt
- [ ] Vom NAS aus erreichbar, von einem anderen Rechner aus nicht mehr

## 7. Clients umstellen

Erst wenn der Proxy auf dem NAS läuft (siehe README):

- [ ] `http://<NAS-IP>:8080/admin/` öffnen, mit `admin` (oder `UI_PASSWORD`) anmelden und ein eigenes Passwort festlegen
- [ ] Bereich *PC*: *Create install command* und den Befehl in eine PowerShell auf dem PC einfügen ([docs/pc-dienst.md](pc-dienst.md))
- [ ] Die Checkliste *Finish setup* auf der *Overview* ist verschwunden (Strata-Adresse, MAC, PC-Dienst, API-Key und `DATA_DIR` sind da)
- [ ] Unter *Strata → Connection* den API-Key von Strata eintragen, damit die Übersicht laufende und wartende Anfragen und Token/s zeigt
- [ ] Die *Overview* zeigt PC und Strata richtig an; *Sleep now* und *Wake PC* wirken

Was die Weboberfläche speichert, liegt in `/data` und gilt sofort. Es geht vor
den Umgebungsvariablen, die wiederum vor den eingebauten Standardwerten gelten.
Einzige Ausnahme ist die MAC-Adresse: Meldet der PC-Dienst eine, gilt immer
diese. Mit *Zurücksetzen* neben einem Feld gilt wieder Umgebung oder Standard.

Passwort vergessen: `/data/admin-password.json` löschen und den Container neu
starten. Danach gilt wieder `admin` (bzw. `UI_PASSWORD`), alle alten
Anmeldungen sind ungültig und beim ersten Login wird ein neues Passwort verlangt.

- [ ] Unter *Strata → Connection* den API-Key von Strata als `STRATA_API_KEY` eintragen (den Wert von `--api-key` aus Schritt 5)
- [ ] Unter *Access* für jeden Client einen eigenen Key anlegen und sofort kopieren; er wird nicht wieder angezeigt
- [ ] Skill `local-agent` einrichten ([tools/local-agent/README.md](../tools/local-agent/README.md)): eigener Key aus *Access* und die Strata-Einstellungen aus dem README; `local-agent check` ist grün
- [ ] Kompaktierung des Skills auf echter Hardware nachgewiesen: der lange Testlauf aus dem README zeigt `context compacted` und endet ohne 400
- [ ] Claude Code direkt gegen Strata (falls genutzt): `ANTHROPIC_BASE_URL=http://<NAS-IP>:8080` und `ANTHROPIC_API_KEY=<Key aus der Weboberfläche>` (geht als `x-api-key` raus)
- [ ] Open WebUI (falls genutzt): *Admin-Einstellungen → Verbindungen → OpenAI API*, URL `http://<NAS-IP>:8080/v1`, als Key den Key aus der Weboberfläche

Ohne gültigen Key antwortet der Proxy mit `401` und weckt den PC nicht. Der
Strata-Key bleibt auf dem NAS; Clients brauchen ihn nicht mehr.
