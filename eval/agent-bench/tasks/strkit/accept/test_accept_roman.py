import unittest

import strkit


class Roman(unittest.TestCase):
    def test_to(self):
        for n, s in [(1, "I"), (4, "IV"), (9, "IX"), (14, "XIV"), (40, "XL"), (90, "XC"), (400, "CD"), (1994, "MCMXCIV"), (3999, "MMMCMXCIX")]:
            self.assertEqual(strkit.to_roman(n), s)

    def test_to_refuses(self):
        for n in [0, -1, 4000]:
            with self.assertRaises(strkit.RomanError):
                strkit.to_roman(n)
        for bad in [1.0, True, "1"]:
            with self.assertRaises(TypeError, msg=repr(bad)):
                strkit.to_roman(bad)

    def test_from(self):
        self.assertEqual(strkit.from_roman("MCMXCIV"), 1994)
        self.assertEqual(strkit.from_roman("mcmxciv"), 1994)
        self.assertEqual(strkit.from_roman("xLiI"), 42)

    def test_from_refuses(self):
        for s in ["", "IIII", "IC", "VX", "MMMM", "IIV", "XM", "VV", "ABC", "I I"]:
            with self.assertRaises(strkit.RomanError, msg=s):
                strkit.from_roman(s)

    def test_round_trip(self):
        for n in range(1, 4000):
            self.assertEqual(strkit.from_roman(strkit.to_roman(n)), n)

    def test_error(self):
        self.assertTrue(issubclass(strkit.RomanError, strkit.StrkitError))
