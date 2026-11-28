"""What the four features share: the package's exports, the README and the changelog."""
import re
import unittest
from pathlib import Path

import strkit

ROOT = Path(__file__).resolve().parent.parent
FUNCTIONS = ["parse_duration", "format_duration", "to_roman", "from_roman", "parse_version", "compare_versions", "bump", "render"]
ERRORS = ["StrkitError", "DurationError", "RomanError", "VersionError", "TemplateError"]


class Shared(unittest.TestCase):
    def test_all(self):
        for name in FUNCTIONS + ERRORS + ["Version"]:
            self.assertIn(name, strkit.__all__, name)
            self.assertTrue(hasattr(strkit, name), name)
        self.assertEqual(len(strkit.__all__), len(set(strkit.__all__)), "a name twice in __all__")

    def test_errors_module(self):
        errors = (ROOT / "strkit" / "errors.py").read_text()
        for name in ERRORS:
            self.assertRegex(errors, rf"class {name}\b", name)

    def test_readme(self):
        text = (ROOT / "README.md").read_text()
        self.assertNotIn("(none yet)", text)
        self.assertNotIn("<<<<<<<", text)
        functions = text.split("## Functions", 1)[1].split("\n## ", 1)[0]
        for name in FUNCTIONS:
            self.assertEqual(len(re.findall(rf"^- strkit\.{name}\(", functions, re.M)), 1, name)
        headings = re.findall(r"^## (.+)$", text, re.M)
        for section in ["Durations", "Roman numerals", "Versions", "Templates"]:
            self.assertEqual(headings.count(section), 1, section)
        self.assertEqual(headings[0], "Functions")
        self.assertEqual(headings[-1], "Errors")

    def test_changelog(self):
        text = (ROOT / "CHANGELOG.md").read_text()
        self.assertNotIn("<<<<<<<", text)
        for name in FUNCTIONS:
            self.assertEqual(text.count(f"- Add strkit.{name}."), 1, name)

    def test_no_markers(self):
        for path in ROOT.rglob("*"):
            if path.is_file() and path.parent != Path(__file__).resolve().parent and ".nv" not in path.parts and ".git" not in path.parts and path.suffix in (".py", ".md"):
                self.assertNotIn("<<<<<<< ", path.read_text(errors="replace"), str(path))
