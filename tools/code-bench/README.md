# Code-Benchmark

Der Knopf **Run code benchmark** unter Strata → Version & Benchmark lässt das
aktive Modell drei feste Programmieraufgaben lösen. Dafür läuft auf dem NAS
neben dem Proxy ein zweiter Container, `code-bench`:

- `runner.py` nimmt vom Proxy einen Lauf an (`POST /runs`), lässt für jede
  Aufgabe `opencode run` in einem frischen Ordner arbeiten und prüft danach
  mit den versteckten Tests, was übrig ist. Der Proxy fragt `GET /runs/current`
  ab, bis der Lauf endet.
- opencode spricht über das Relay mit dem OpenAI-Endpunkt des Proxys (`PROXY_URL`), mit einem
  Key, den der Proxy für den Lauf anlegt und danach widerruft. So weckt der
  Lauf den PC wie jeder Client.
- Die Aufgaben in `tasks/` baut der Proxy ein und schickt sie mit dem Lauf;
  die versteckten Tests liegen erst nach opencode im Ordner.
- Der Agent darf lesen, schreiben und `bash` ausführen, `webfetch` nicht.
  opencode und die versteckten Tests laufen als Nutzer `agent` in diesem
  Container, nicht auf dem NAS. Der Runner (root) setzt beim Start eine
  Firewall nur für `agent`: erlaubt sind ein Relay im Runner (`RELAY_ADDR`,
  127.0.0.1:8097), das nur `/v1` an den Proxy (`PROXY_URL`) weitergibt, nicht
  dessen Weboberfläche, dazu Dockers DNS und das Internet; gesperrt sind der
  Proxy direkt, Loopback (die API des Runners), private,
  Link-Local- und CGNAT-Netze, also NAS und Heimnetz, und IPv6. Dafür braucht
  der Container `NET_ADMIN`; ohne verweigert der Runner Läufe und die Karte
  nennt den Grund.
- Vor jedem Lauf und bei jeder Abfrage von `/health` prüft der Runner, ob
  unter `PROXY_URL` wirklich der Proxy antwortet (dessen Anmeldeseite, die den
  PC nie weckt). Antwortet dort ein anderer Dienst oder keiner, verweigert er
  Läufe und die Karte nennt Adresse und Antwort.

Gemessen wird pro Aufgabe: Gesamtzeit, Schritte des Modells, Ein- und
Ausgabe-Tokens, Ausgabe-Tokens pro Sekunde (über die Zeit der Schritte) und
bestandene Tests. Nach 30 Minuten wird opencode beendet; was es geschrieben
hat, wird trotzdem geprüft.

Tests: `python3 tools/code-bench/test_runner.py` (mit einem nachgebildeten
opencode; `scripts/check.sh` führt sie aus).

## Bedrohungsmodell

Was opencode, die Befehle des Modells und der Code in den Aufgabenordnern
tun, ist fremder Code. Er erreicht nicht:

| Was | Geschützt durch |
| --- | --- |
| die API des Runners (Läufe abbrechen oder starten) | Nutzer `agent`, Firewall sperrt Loopback außer dem Relay und Port 53 von Dockers DNS (127.0.0.11) |
| NAS, Heimnetz, Weboberfläche des Proxys | Firewall sperrt private, Link-Local- und CGNAT-Netze und IPv6; das Relay gibt nur `/v1/*` weiter |
| andere Keys als den des Laufs | der Proxy legt pro Lauf einen Key an und widerruft nur diesen |
| Dateien des Runners und anderer Aufgaben | Nutzer `agent` besitzt nur den eigenen Aufgabenordner |
| die Wertung | versteckte Tests erst nach opencode, `python -I`, eigene Prozessgruppe; Schreiben ohne Symlinks zu folgen (`O_NOFOLLOW`); Marke nur aus stdout |
| Prozesse über das Ende der Aufgabe hinaus | opencodes Prozessgruppe endet mit opencode, die der Tests nach der Wertung |

Bekannte Lücke: Der geprüfte Code läuft im Prozess des Testlaufs und kann
beim Beenden selbst eine Marke ausgeben. Das lässt sich in diesem Prozess
nicht verhindern; die Wertung ist ein Maß, keine Prüfung gegen Betrug.
