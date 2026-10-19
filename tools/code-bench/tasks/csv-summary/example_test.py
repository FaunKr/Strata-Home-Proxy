import os
import tempfile
import unittest

from summary import summarize


def write(text):
    fd, path = tempfile.mkstemp(suffix=".csv")
    with os.fdopen(fd, "w", newline="") as f:
        f.write(text)
    return path


class Examples(unittest.TestCase):
    def test_the_example(self):
        path = write(
            "date,region,product,quantity,unit_price\n"
            "2024-01-02,North,Tea,2,3.50\n"
            "2024-01-03,north,Cake,1,10\n"
        )
        self.assertEqual(
            summarize(path),
            {"North": {"orders": 2, "revenue": 17.0, "top_product": "Cake"}},
        )

    def test_only_a_header(self):
        self.assertEqual(summarize(write("date,region,product,quantity,unit_price\n")), {})


if __name__ == "__main__":
    unittest.main()
