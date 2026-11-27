"""What the proofs came to: each theorem proved, as stated, of the definitions as given, on
the standard axioms only; the README and the changelog say so. Run from the project's root."""
import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STANDARD = {"propext", "Classical.choice", "Quot.sound"}
THEOREMS = {
    "mem_insert": "∀ (x y : Nat) (t : Tree), y ∈ (insert x t).toList ↔ y = x ∨ y ∈ t.toList",
    "insert_bst": "∀ (x : Nat) (t : Tree), BST t → BST (insert x t)",
    "search_correct": "∀ (x : Nat) (t : Tree), BST t → (search x t = true ↔ x ∈ t.toList)",
    "toList_sorted": "∀ (t : Tree), BST t → t.toList.Pairwise (· < ·)",
    "delete_bst": "∀ (x : Nat) (t : Tree), BST t → BST (delete x t)",
    "mem_delete": "∀ (x y : Nat) (t : Tree), BST t → (y ∈ (delete x t).toList ↔ y ≠ x ∧ y ∈ t.toList)",
    "treeSort_sorted": "∀ (xs : List Nat), (treeSort xs).Pairwise (· < ·)",
    "mem_treeSort": "∀ (y : Nat) (xs : List Nat), y ∈ treeSort xs ↔ y ∈ xs",
}
DEFINITIONS = """
#guard (insert 2 (insert 5 (insert 3 .leaf))).toList = [2, 3, 5]
#guard (insert 3 (insert 3 .leaf)).toList = [3]
#guard search 5 (fromList [4, 5, 1]) = true
#guard search 6 (fromList [4, 5, 1]) = false
#guard (delete 4 (fromList [1, 7, 4, 2, 9, 5])).toList = [1, 2, 5, 7, 9]
#guard (delete 8 (fromList [1, 7, 4])).toList = [1, 4, 7]
#guard (removeMin (.node .leaf 2 .leaf) 5 (.node .leaf 8 .leaf)).1 = 2
#guard (join (.node .leaf 1 .leaf) (.node (.node .leaf 6 .leaf) 7 .leaf)).toList = [1, 6, 7]
#guard treeSort [3, 1, 2, 3, 1] = [1, 2, 3]
example : BST (.node (.node .leaf 1 .leaf) 3 (.node .leaf 5 .leaf)) :=
  .node ⟨trivial, by decide, trivial⟩ ⟨trivial, by decide, trivial⟩ (.node trivial trivial .leaf .leaf) (.node trivial trivial .leaf .leaf)
example : ¬ BST (.node (.node .leaf 5 .leaf) 3 .leaf) := by
  intro h
  cases h with
  | node hl _ _ _ => exact absurd hl.2.1 (by decide)
example : ForallT (· < 3) (.node .leaf 1 (.node .leaf 2 .leaf)) := ⟨trivial, by decide, trivial, by decide, trivial⟩
"""

def lean(text):
    check = ROOT / "AcceptCheck.lean"
    check.write_text("import Bst\nnamespace Bst\n" + text + "\nend Bst\n")
    try:
        out = subprocess.run(["lake", "env", "lean", str(check)], cwd=ROOT, capture_output=True, text=True, timeout=300)
    finally:
        check.unlink()
    return out.returncode, out.stdout + out.stderr


class Proofs(unittest.TestCase):
    built = None

    @classmethod
    def setUpClass(cls):
        out = subprocess.run(["lake", "build"], cwd=ROOT, capture_output=True, text=True, timeout=900)
        cls.built = (out.returncode, out.stdout + out.stderr)

    def setUp(self):
        self.assertEqual(self.built[0], 0, "lake build: " + self.built[1][-2000:])

    def check(self, name):
        code, said = lean(f"example : {THEOREMS[name]} := {name}\n#print axioms {name}\n")
        self.assertEqual(code, 0, said)
        self.assertNotIn("error", said, said)
        used = set()
        for m in re.finditer(r"depends on axioms: \[([^\]]*)\]", said):
            used |= {a.strip() for a in m.group(1).split(",") if a.strip()}
        self.assertTrue(used <= STANDARD, f"{name} depends on {sorted(used - STANDARD)}")

    def test_definitions(self):
        code, said = lean(DEFINITIONS)
        self.assertEqual(code, 0, said)


for _name in THEOREMS:
    setattr(Proofs, f"test_{_name}", lambda self, n=_name: self.check(n))


class Shared(unittest.TestCase):
    def test_readme(self):
        text = (ROOT / "README.md").read_text()
        self.assertNotIn("- [ ]", text)
        self.assertNotIn("<<<<<<<", text)
        for name in THEOREMS:
            self.assertEqual(text.count(f"- [x] `{name}`"), 1, name)

    def test_changelog(self):
        text = (ROOT / "CHANGELOG.md").read_text()
        self.assertNotIn("<<<<<<<", text)
        for name in THEOREMS:
            self.assertEqual(text.count(f"- Prove `{name}`."), 1, name)
