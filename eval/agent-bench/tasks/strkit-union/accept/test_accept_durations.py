import unittest

import strkit


class Durations(unittest.TestCase):
    def test_parse(self):
        for text, seconds in [("1h30m", 5400), ("2d", 172800), ("45s", 45), ("1h 30m 5s", 5405), (" 1D2H ", 93600), ("0s", 0), ("100m", 6000)]:
            self.assertEqual(strkit.parse_duration(text), seconds, text)

    def test_parse_refuses(self):
        for text in ["", "   ", "10", "h", "1x", "1m1h", "1h1h", "1.5h", "-1h", "1 h"]:
            with self.assertRaises(strkit.DurationError, msg=text):
                strkit.parse_duration(text)

    def test_format(self):
        for seconds, text in [(5400, "1h 30m"), (90061, "1d 1h 1m 1s"), (0, "0s"), (59, "59s"), (3600, "1h"), (86400 * 3 + 60, "3d 1m")]:
            self.assertEqual(strkit.format_duration(seconds), text)

    def test_format_refuses(self):
        with self.assertRaises(strkit.DurationError):
            strkit.format_duration(-1)
        for bad in [1.5, True, "5"]:
            with self.assertRaises(TypeError, msg=repr(bad)):
                strkit.format_duration(bad)

    def test_round_trip(self):
        for n in [1, 61, 3599, 3601, 86399, 1234567]:
            self.assertEqual(strkit.parse_duration(strkit.format_duration(n)), n)

    def test_error(self):
        self.assertTrue(issubclass(strkit.DurationError, strkit.StrkitError))
