import unittest

from roman import from_roman, to_roman


class Examples(unittest.TestCase):
    def test_some_numbers(self):
        self.assertEqual(to_roman(4), "IV")
        self.assertEqual(to_roman(1994), "MCMXCIV")
        self.assertEqual(from_roman("XLII"), 42)

    def test_out_of_range(self):
        with self.assertRaises(ValueError):
            to_roman(0)


if __name__ == "__main__":
    unittest.main()
