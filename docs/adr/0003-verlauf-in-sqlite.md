# Verlauf in SQLite

Die Ereignisse liegen in einer SQLite-Datenbank (`events.db`) in `DATA_DIR`,
nicht mehr in `events.jsonl`. Der Proxy nutzt `rusqlite` mit gebündeltem
SQLite. Schreiben und Bereichsabfragen laufen auf einem eigenen Thread, nie auf
den Threads des async-Runtimes: `record` stellt nur in die Warteschlange, `range`
wartet asynchron. Die letzten 1000 Ereignisse für die Übersicht hält der Log
zusätzlich im Speicher. Ohne `DATA_DIR` liegt der Store im Speicher.

Der Verlauf soll ab dem nächsten Ticket Bereiche abfragen (Tage, Wochen) und
später Minutensamples bilden. Ein JSONL-File müsste dafür jedes Mal vollständig
gelesen werden; die Datenbank liefert Zeitbereiche über einen Index und zählt
ohne Lesen ganzer Dateien.

Ein vorhandenes `events.jsonl` wird beim ersten Start in eine leere Datenbank
in einer Transaktion importiert; unlesbare Zeilen und unbekannte Arten
überspringt der Import wie zuvor. Die Datei bleibt danach unverändert und wird
nur noch als Sicherung behalten, nie wieder geschrieben.

Die Schnittstelle des Logs (`record`, `recent`, `latest_pc`) bleibt unverändert;
dazu kommt das Lesen von Zeitbereichen. Die Übersicht und alle Aufrufer verhalten
sich wie bisher.
