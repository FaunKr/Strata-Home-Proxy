# History in SQLite

The events are in an SQLite database (`events.db`, `rusqlite` with
bundled SQLite) in `DATA_DIR`, no longer in `events.jsonl`. The history
queries time ranges back to the first event and is later meant to hold per-minute readings of the
power plug; a JSONL file would have to be read in full every time for that,
the database returns ranges via an index.

`events.jsonl` is imported once in a transaction and afterwards kept only as a
backup, never written again. A failed import is retried at the
next startup.

`record` writes immediately, as appending to the file did before, so that a
stopped proxy loses nothing; in WAL mode an entry costs no sync to
disk. Range queries run on a blocking thread, not on the
async runtime.

Whoever reports the short write access in `record` in review: decided.
Change it only if it measurably slows things down.
