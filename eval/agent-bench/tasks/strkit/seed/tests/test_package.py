import unittest

import strkit
from strkit.errors import StrkitError


class Package(unittest.TestCase):
    def test_imports(self):
        self.assertTrue(strkit.__doc__)
        self.assertTrue(issubclass(StrkitError, ValueError))


if __name__ == "__main__":
    unittest.main()
