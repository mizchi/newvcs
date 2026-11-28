import unittest

import strkit


class Versions(unittest.TestCase):
    def test_parse(self):
        v = strkit.parse_version("1.2.3")
        self.assertEqual((v.major, v.minor, v.patch, v.pre), (1, 2, 3, ()))
        self.assertIsInstance(v, strkit.Version)
        self.assertEqual(strkit.parse_version("1.2.3-alpha.1").pre, ("alpha", 1))
        self.assertEqual(strkit.parse_version("0.0.0-x-y.0").pre, ("x-y", 0))

    def test_parse_refuses(self):
        for text in ["", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "1.2.3-01", "1.2.3-", "1.2.3-a..b", "1.2.3+build", "1.2.3-a_b", "v1.2.3", "1.2.x"]:
            with self.assertRaises(strkit.VersionError, msg=text):
                strkit.parse_version(text)

    def test_order(self):
        chain = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.1.0", "2.0.0", "10.0.0"]
        for a, b in zip(chain, chain[1:]):
            self.assertEqual(strkit.compare_versions(a, b), -1, (a, b))
            self.assertEqual(strkit.compare_versions(b, a), 1, (b, a))
        self.assertEqual(strkit.compare_versions("1.2.3-rc.1", "1.2.3-rc.1"), 0)

    def test_bump(self):
        self.assertEqual(strkit.bump("1.2.3", "patch"), "1.2.4")
        self.assertEqual(strkit.bump("1.2.3", "minor"), "1.3.0")
        self.assertEqual(strkit.bump("1.2.3", "major"), "2.0.0")
        self.assertEqual(strkit.bump("1.2.3-rc.1", "minor"), "1.3.0")
        with self.assertRaises(strkit.VersionError):
            strkit.bump("1.2.3", "build")

    def test_error(self):
        self.assertTrue(issubclass(strkit.VersionError, strkit.StrkitError))
