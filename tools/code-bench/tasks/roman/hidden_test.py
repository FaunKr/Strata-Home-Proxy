import unittest

from roman import from_roman, to_roman


class Hidden(unittest.TestCase):
    def test_small_numbers(self):
        expected = ["I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X"]
        self.assertEqual([to_roman(n) for n in range(1, 11)], expected)

    def test_larger_numbers(self):
        self.assertEqual(to_roman(40), "XL")
        self.assertEqual(to_roman(90), "XC")
        self.assertEqual(to_roman(400), "CD")
        self.assertEqual(to_roman(944), "CMXLIV")
        self.assertEqual(to_roman(3999), "MMMCMXCIX")

    def test_round_trip(self):
        for n in range(1, 4000):
            self.assertEqual(from_roman(to_roman(n)), n)

    def test_lowercase(self):
        self.assertEqual(from_roman("mcmxciv"), 1994)

    def test_out_of_range(self):
        for n in (0, -1, 4000):
            with self.assertRaises(ValueError):
                to_roman(n)

    def test_not_an_integer(self):
        for n in (2.5, "5", True, None):
            with self.assertRaises(ValueError):
                to_roman(n)

    def test_bad_numerals(self):
        for s in ("", "IIII", "VX", "MMMM", "IC", "XM", "VV", "ABC", "X I"):
            with self.assertRaises(ValueError, msg=s):
                from_roman(s)

    def test_only_canonical_forms(self):
        for s in ("IIX", "XXXX", "CCCC", "DD", "LL"):
            with self.assertRaises(ValueError, msg=s):
                from_roman(s)


if __name__ == "__main__":
    unittest.main()
