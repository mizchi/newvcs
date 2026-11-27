"""Tests of merge_cst.py, run as Git runs a driver: three files, the result left in the second.

    python3 -m unittest discover -s tools/merge-cst
"""
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
try:
    import tree_sitter  # noqa: F401

    HAVE_TREE_SITTER = True
except ImportError:
    HAVE_TREE_SITTER = False
needs_tree_sitter = unittest.skipUnless(HAVE_TREE_SITTER, "tree-sitter is not installed: Python and Markdown are merged as text")
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


@needs_tree_sitter
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


@needs_tree_sitter
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

    def test_items_ticked_by_both_stay_where_they_were(self):
        base = "# p\n\n## Proved\n\n" + "".join(f"- [ ] `t{i}`\n" for i in range(1, 7)) + "\n## Building\n"
        ours = base.replace("- [ ] `t5`", "- [x] `t5`").replace("- [ ] `t6`", "- [x] `t6`")
        theirs = base.replace("- [ ] `t3`", "- [x] `t3`").replace("- [ ] `t4`", "- [x] `t4`")
        want = base
        for i in (3, 4, 5, 6):
            want = want.replace(f"- [ ] `t{i}`", f"- [x] `t{i}`")
        code, out = merge("README.md", base, ours, theirs)
        self.assertEqual(code, 0, out)
        self.assertEqual(out, want)
        # One step at a time, as a sync puts commits on a tip one by one, comes to the same.
        first = base.replace("- [ ] `t5`", "- [x] `t5`")
        code, step = merge("README.md", base, first, theirs)
        code, step = merge("README.md", first, ours, step)
        self.assertEqual(step, want)

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


LEAN = """namespace Stack

def size : List Nat → Nat
  | [] => 0
  | _ :: xs => 1 + size xs

theorem size_nil : size [] = 0 := rfl

end Stack
"""


class Lean(unittest.TestCase):
    def test_proofs_filled_in_by_both(self):
        base = LEAN.replace("end Stack", "theorem a : 1 = 1 := by sorry\n\ntheorem b : 2 = 2 := by sorry\n\nend Stack")
        ours = base.replace("theorem a : 1 = 1 := by sorry", "theorem a : 1 = 1 := by\n  rfl")
        theirs = base.replace("theorem b : 2 = 2 := by sorry", "/-- Two is two. -/\ntheorem b : 2 = 2 := by\n  decide")
        code, out = merge("Stack.lean", base, ours, theirs)
        self.assertEqual(code, 0, out)
        self.assertIn("theorem a : 1 = 1 := by\n  rfl", out)
        self.assertIn("/-- Two is two. -/\ntheorem b : 2 = 2 := by\n  decide", out)
        self.assertNotIn("sorry", out)
        self.assertTrue(out.endswith("end Stack\n"), out)

    def test_lemmas_added_by_both_before_the_end(self):
        ours = LEAN.replace("end Stack", "theorem x : 0 = 0 := rfl\n\nend Stack")
        theirs = LEAN.replace("end Stack", "theorem y : 1 = 1 := rfl\n\nend Stack")
        code, out = merge("Stack.lean", LEAN, ours, theirs)
        self.assertEqual(code, 0, out)
        self.assertLess(out.index("theorem x"), out.index("theorem y"))
        self.assertLess(out.index("theorem y"), out.index("end Stack"))

    def test_one_proof_written_two_ways_is_a_conflict(self):
        base = LEAN.replace("end Stack", "theorem a : 1 = 1 := by sorry\n\nend Stack")
        code, out = merge("Stack.lean", base, base.replace("by sorry", "by rfl"), base.replace("by sorry", "by decide"))
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
