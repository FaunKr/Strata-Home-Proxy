import time
import unittest

from lru import LRUCache


class Hidden(unittest.TestCase):
    def test_get_and_default(self):
        cache = LRUCache(2)
        cache.put("a", 1)
        self.assertEqual(cache.get("a"), 1)
        self.assertIsNone(cache.get("x"))
        self.assertEqual(cache.get("x", 7), 7)

    def test_replacing_marks_as_used(self):
        cache = LRUCache(2)
        cache.put("a", 1)
        cache.put("b", 2)
        cache.put("a", 10)
        cache.put("c", 3)
        self.assertEqual(cache.keys(), ["a", "c"])
        self.assertEqual(cache.get("a"), 10)
        self.assertEqual(len(cache), 2)

    def test_contains_does_not_mark_as_used(self):
        cache = LRUCache(2)
        cache.put("a", 1)
        cache.put("b", 2)
        self.assertIn("a", cache)
        cache.put("c", 3)
        self.assertNotIn("a", cache)
        self.assertEqual(cache.evictions, 1)

    def test_bad_capacity(self):
        for n in (0, -3):
            with self.assertRaises(ValueError):
                LRUCache(n)
        with self.assertRaises(ValueError):
            LRUCache(2).resize(0)

    def test_resize_drops_the_oldest(self):
        cache = LRUCache(4)
        for k in "abcd":
            cache.put(k, k)
        cache.get("a")
        cache.resize(2)
        self.assertEqual(cache.keys(), ["d", "a"])
        self.assertEqual(cache.evictions, 2)
        cache.resize(3)
        cache.put("e", 1)
        self.assertEqual(len(cache), 3)

    def test_evictions_is_read_only(self):
        with self.assertRaises(AttributeError):
            LRUCache(1).evictions = 5

    def test_capacity_one(self):
        cache = LRUCache(1)
        cache.put("a", 1)
        cache.put("b", 2)
        self.assertEqual(cache.keys(), ["b"])
        self.assertEqual(cache.get("a"), None)

    def test_constant_time(self):
        cache = LRUCache(100_000)
        started = time.perf_counter()
        for i in range(300_000):
            cache.put(i, i)
            cache.get(i - 50_000)
        self.assertLess(time.perf_counter() - started, 5.0)
        self.assertEqual(len(cache), 100_000)


if __name__ == "__main__":
    unittest.main()
