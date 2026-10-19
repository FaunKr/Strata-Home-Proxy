Write `summary.py` in this folder with a function
`summarize(path: str) -> dict` that reads a CSV file of orders and sums them
up per region.

The file has a header row with the columns `date,region,product,quantity,unit_price`
in any order. Fields may be quoted and contain commas. For each region the
result holds:

- `orders`: the number of valid rows
- `revenue`: the sum of `quantity * unit_price`, rounded to 2 decimals
- `top_product`: the product with the highest revenue in that region; on a
  tie the one that comes first alphabetically

Region names are trimmed of surrounding spaces and compared without regard to
case; the result uses the spelling of the region's first valid row. A row is
skipped when `quantity` is not a whole number of at least 1, when
`unit_price` is not a number of at least 0, or when the region is empty. An
empty file, or one with only the header, gives `{}`.

Example: two rows `2024-01-02,North,Tea,2,3.50` and `2024-01-03,north,Cake,1,10`
give `{"North": {"orders": 2, "revenue": 17.0, "top_product": "Cake"}}`.

Use only the Python standard library. `example_test.py` shows a few cases;
run it with `python -m unittest example_test`.
