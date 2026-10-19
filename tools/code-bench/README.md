# Code-Benchmark

Der Knopf **Run code benchmark** unter Strata → Version & Benchmark lässt das
aktive Modell drei feste Programmieraufgaben lösen. Dafür läuft auf dem NAS
neben dem Proxy ein zweiter Container, `code-bench`:

- `runner.py` nimmt vom Proxy einen Lauf an (`POST /runs`), lässt für jede
  Aufgabe `opencode run` in einem frischen Ordner arbeiten und prüft danach
  mit den versteckten Tests, was übrig ist. Der Proxy fragt `GET /runs/current`
  ab, bis der Lauf endet.
- opencode spricht mit dem OpenAI-Endpunkt des Proxys (`PROXY_URL`), mit einem
  Key, den der Proxy für den Lauf anlegt und danach widerruft. So weckt der
  Lauf den PC wie jeder Client.
- Die Aufgaben in `tasks/` baut der Proxy ein und schickt sie mit dem Lauf;
  die versteckten Tests liegen erst nach opencode im Ordner.
- Der Agent darf lesen, schreiben und `bash` ausführen, `webfetch` nicht. Er
  läuft als Nutzer `bench` in diesem Container, nicht auf dem NAS.

Gemessen wird pro Aufgabe: Gesamtzeit, Schritte des Modells, Ein- und
Ausgabe-Tokens, Ausgabe-Tokens pro Sekunde (über die Zeit der Schritte) und
bestandene Tests. Nach 30 Minuten wird opencode beendet; was es geschrieben
hat, wird trotzdem geprüft.

Tests: `python3 tools/code-bench/test_runner.py` (mit einem nachgebildeten
opencode; `scripts/check.sh` führt sie aus).
