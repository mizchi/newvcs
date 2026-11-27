"""Tests of merge_cst.py, run as Git runs a driver: three files, the result left in the second.

    python3 -m unittest discover -s tools/merge-cst
"""
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
DRIVER = os.path.join(HERE, "merge_cst.py")


def merge(path, base, ours, theirs):
    with tempfile.TemporaryDirectory() as d:
        files = []
        for name, text in (("O", base), ("A", ours), ("B", theirs)):
            f = os.path.join(d, name)
            with open(f, "w") as h:
                h.write(text)
            files.append(f)
        code = subprocess.run([sys.executable, DRIVER, files[0], files[1], files[2], path, "yours", "theirs", "7"]).returncode
        with open(files[1]) as h:
            return code, h.read()


ERRORS = '"""The errors."""\n\n\nclass StrkitError(ValueError):\n    """Base."""\n'
README = "# strkit\n\nSmall.\n\n## Functions\n\n- (none yet)\n\n## Errors\n\nAll are StrkitError.\n"
CHANGELOG = "# Changelog\n\n## Unreleased\n\n"


class Python(unittest.TestCase):
    def test_two_classes_added_at_the_end(self):
        ours = ERRORS + "\n\nclass DurationError(StrkitError):\n    pass\n"
        theirs = ERRORS + "\n\nclass RomanError(StrkitError):\n    pass\n"
        code, out = merge("strkit/errors.py", ERRORS, ours, theirs)
        self.assertEqual(code, 0)
        self.assertEqual(out, ERRORS + "\n\nclass DurationError(StrkitError):\n    pass\n\n\nclass RomanError(StrkitError):\n    pass\n")
        compile(out, "errors.py", "exec")

    def test_one_side_changes_a_function_the_other_adds_one(self):
        base = "def a():\n    return 1\n\n\ndef b():\n    return 2\n"
        ours = "def a():\n    return 10\n\n\ndef b():\n    return 2\n"
        theirs = base + "\n\ndef c():\n    return 3\n"
        code, out = merge("m.py", base, ours, theirs)
        self.assertEqual(code, 0)
        self.assertEqual(out, "def a():\n    return 10\n\n\ndef b():\n    return 2\n\n\ndef c():\n    return 3\n")

    def test_both_change_one_function_is_a_conflict_with_markers(self):
        base = "def a():\n    return 1\n"
        code, out = merge("m.py", base, "def a():\n    return 2\n", "def a():\n    return 3\n")
        self.assertEqual(code, 1)
        self.assertIn("<<<<<<< yours", out)
        self.assertIn(">>>>>>> theirs", out)

    def test_a_version_that_does_not_parse_is_merged_as_text(self):
        base = "x = 1\n"
        code, out = merge("m.py", base, "x = 1\ny = (\n", "x = 1\nz = 2\n")
        self.assertEqual(code, 1)
        self.assertIn("<<<<<<< yours", out)

    def test_one_removes_what_the_other_left(self):
        base = "def a():\n    pass\n\n\ndef b():\n    pass\n"
        ours = "def b():\n    pass\n"
        theirs = base + "\n\ndef c():\n    pass\n"
        code, out = merge("m.py", base, ours, theirs)
        self.assertEqual(code, 0)
        self.assertNotIn("def a", out)
        self.assertIn("def c", out)


class Markdown(unittest.TestCase):
    def test_lines_listed_and_sections_added_by_both(self):
        ours = README.replace("- (none yet)\n", "- strkit.parse_duration(text): seconds\n- strkit.format_duration(s): text\n").replace(
            "## Errors", "## Durations\n\nAn example.\n\n## Errors"
        )
        theirs = README.replace("- (none yet)\n", "- strkit.to_roman(n): a numeral\n").replace("## Errors", "## Roman numerals\n\nAnother.\n\n## Errors")
        code, out = merge("README.md", README, ours, theirs)
        self.assertEqual(code, 0, out)
        self.assertNotIn("(none yet)", out)
        self.assertNotIn("<<<<<<<", out)
        functions = out.split("## Functions", 1)[1].split("\n## ", 1)[0]
        self.assertEqual(
            [line for line in functions.splitlines() if line.startswith("- ")],
            ["- strkit.parse_duration(text): seconds", "- strkit.format_duration(s): text", "- strkit.to_roman(n): a numeral"],
        )
        self.assertIn("- strkit.format_duration(s): text\n- strkit.to_roman(n): a numeral\n\n## Durations", out, "no blank line between items")
        headings = [line for line in out.splitlines() if line.startswith("#")]
        self.assertEqual(headings, ["# strkit", "## Functions", "## Durations", "## Roman numerals", "## Errors"])

    def test_changelog_lines_from_both(self):
        ours = CHANGELOG + "- Add strkit.parse_duration.\n- Add strkit.format_duration.\n"
        theirs = CHANGELOG + "- Add strkit.render.\n"
        code, out = merge("CHANGELOG.md", CHANGELOG, ours, theirs)
        self.assertEqual(code, 0, out)
        self.assertEqual(out, CHANGELOG + "- Add strkit.parse_duration.\n- Add strkit.format_duration.\n- Add strkit.render.\n")

    def test_one_paragraph_changed_two_ways_is_a_conflict(self):
        base = "# t\n\nOne line.\n"
        code, out = merge("x.md", base, "# t\n\nOne line, ours.\n", "# t\n\nOne line, theirs.\n")
        self.assertEqual(code, 1)
        self.assertIn("<<<<<<< yours", out)


class Others(unittest.TestCase):
    def test_a_language_it_does_not_know_is_merged_as_text(self):
        code, out = merge("x.txt", "a\nb\n", "a\nb\nours\n", "a\nb\ntheirs\n")
        self.assertEqual(code, 1)
        self.assertIn("<<<<<<< yours", out)
        code, out = merge("x.txt", "a\nb\nc\n", "a\nours\nc\n", "a\nb\nc\ntheirs\n")
        self.assertEqual(code, 0)
        self.assertEqual(out, "a\nours\nc\ntheirs\n")


if __name__ == "__main__":
    unittest.main()
