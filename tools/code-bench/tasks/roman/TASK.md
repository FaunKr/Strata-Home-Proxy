Write `roman.py` in this folder with two functions:

- `to_roman(n: int) -> str` turns 1 to 3999 into a Roman numeral in the usual
  subtractive form (4 is `IV`, 1994 is `MCMXCIV`). Any other value, also a
  non-integer such as `2.5` or `True`, raises `ValueError`.
- `from_roman(s: str) -> int` reads such a numeral back. Lowercase letters
  are accepted. A string that `to_roman` would not produce (`IIII`, `VX`,
  `MMMM`, an empty string, other characters) raises `ValueError`.

Use only the Python standard library. `example_test.py` shows a few cases;
run it with `python -m unittest example_test`.
