import os
import tempfile
import unittest

from summary import summarize

HEADER = "date,region,product,quantity,unit_price\n"


def write(text):
    fd, path = tempfile.mkstemp(suffix=".csv")
    with os.fdopen(fd, "w", newline="") as f:
        f.write(text)
    return path


class Hidden(unittest.TestCase):
    def test_two_regions(self):
        path = write(
            HEADER
            + "2024-01-01,North,Tea,2,3.50\n"
            + "2024-01-01,South,Tea,1,3.50\n"
            + "2024-01-02,South,Coffee,3,4\n"
        )
        self.assertEqual(
            summarize(path),
            {
                "North": {"orders": 1, "revenue": 7.0, "top_product": "Tea"},
                "South": {"orders": 2, "revenue": 15.5, "top_product": "Coffee"},
            },
        )

    def test_columns_in_another_order(self):
        path = write("unit_price,quantity,product,region,date\n2.25,4,Bun,East,2024-02-01\n")
        self.assertEqual(
            summarize(path), {"East": {"orders": 1, "revenue": 9.0, "top_product": "Bun"}}
        )

    def test_quoted_fields_with_commas(self):
        path = write(HEADER + '2024-01-01,"West, Coast","Tea, green",1,"2.5"\n')
        self.assertEqual(
            summarize(path),
            {"West, Coast": {"orders": 1, "revenue": 2.5, "top_product": "Tea, green"}},
        )

    def test_regions_ignore_case_and_spaces(self):
        path = write(
            HEADER + "2024-01-01,  North ,Tea,1,1\n" + "2024-01-02,NORTH,Tea,1,1\n"
        )
        self.assertEqual(
            summarize(path), {"North": {"orders": 2, "revenue": 2.0, "top_product": "Tea"}}
        )

    def test_bad_rows_are_skipped(self):
        path = write(
            HEADER
            + "2024-01-01,North,Tea,0,1\n"
            + "2024-01-01,North,Tea,1.5,1\n"
            + "2024-01-01,North,Tea,two,1\n"
            + "2024-01-01,North,Tea,1,-1\n"
            + "2024-01-01,North,Tea,1,abc\n"
            + "2024-01-01,,Tea,1,1\n"
            + "2024-01-01,North,Cake,1,0\n"
        )
        self.assertEqual(
            summarize(path), {"North": {"orders": 1, "revenue": 0.0, "top_product": "Cake"}}
        )

    def test_top_product_tie_goes_to_the_first_alphabetically(self):
        path = write(HEADER + "2024-01-01,North,Tea,1,5\n" + "2024-01-01,North,Cake,1,5\n")
        self.assertEqual(summarize(path)["North"]["top_product"], "Cake")

    def test_revenue_is_rounded(self):
        path = write(HEADER + "2024-01-01,North,Tea,3,0.333\n")
        self.assertEqual(summarize(path)["North"]["revenue"], 1.0)

    def test_empty_file(self):
        self.assertEqual(summarize(write("")), {})


if __name__ == "__main__":
    unittest.main()
