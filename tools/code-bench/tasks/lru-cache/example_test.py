import unittest

from lru import LRUCache


class Examples(unittest.TestCase):
    def test_drops_the_least_recently_used(self):
        cache = LRUCache(2)
        cache.put("a", 1)
        cache.put("b", 2)
        cache.get("a")
        cache.put("c", 3)
        self.assertEqual(cache.keys(), ["a", "c"])
        self.assertEqual(cache.evictions, 1)


if __name__ == "__main__":
    unittest.main()
