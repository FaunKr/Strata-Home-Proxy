# Verlauf in SQLite

Die Ereignisse liegen in einer SQLite-Datenbank (`events.db`, `rusqlite` mit
gebündeltem SQLite) in `DATA_DIR`, nicht mehr in `events.jsonl`. Der Verlauf
fragt Zeitbereiche bis zum ersten Ereignis ab und soll später Minutenwerte der
Messsteckdose halten; eine JSONL-Datei müsste dafür jedes Mal ganz gelesen
werden, die Datenbank liefert Bereiche über einen Index.

`events.jsonl` wird einmal in einer Transaktion übernommen und danach nur als
Sicherung behalten, nie wieder geschrieben. Ein gescheiterter Import wird beim
nächsten Start wiederholt.

`record` schreibt sofort, wie vorher das Anhängen an die Datei, damit ein
beendeter Proxy nichts verliert; im WAL-Modus kostet ein Eintrag kein Sync auf
die Platte. Bereichsabfragen laufen auf einem Blocking-Thread, nicht auf dem
async-Runtime.

Wer im Review den kurzen Schreibzugriff in `record` meldet: entschieden.
Ändern nur, wenn er messbar bremst.
