Write `lru.py` in this folder with a class `LRUCache`, a cache that holds at
most `capacity` entries and drops the least recently used one when full.

- `LRUCache(capacity: int)`; a capacity below 1 raises `ValueError`.
- `get(key, default=None)` returns the value and marks the key as used, or
  `default` if the key is missing.
- `put(key, value)` adds or replaces a value and marks the key as used. When
  a new key does not fit, the least recently used entry goes first.
- `len(cache)` is the number of entries; `key in cache` tells whether a key is
  there without marking it as used.
- `evictions` (a read-only property) counts the entries dropped so far.
- `resize(capacity: int)` changes the capacity, dropping the least recently
  used entries that no longer fit (they count as evictions); below 1 raises
  `ValueError`.
- `keys()` returns the keys as a list, least recently used first.

`get` and `put` must take constant time on average, also for a capacity of
100000. Use only the Python standard library. `example_test.py` shows a few
cases; run it with `python -m unittest example_test`.
