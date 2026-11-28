"""What the proofs came to: each theorem proved, as stated, of the definitions as given, on
the standard axioms only; the README and the changelog say so. Run from the project's root."""
import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STANDARD = {"propext", "Classical.choice", "Quot.sound"}
THEOREMS = {
    "run_append": "∀ (env : Env) (p q : List Instr) (s : List Nat), run env (p ++ q) s = run env q (run env p s)",
    "compile_run": "∀ (env : Env) (e : Expr) (p : List Instr) (s : List Nat), run env (compile e ++ p) s = run env p (eval env e :: s)",
    "compile_correct": "∀ (env : Env) (e : Expr), run env (compile e) [] = [eval env e]",
    "eval_fold": "∀ (env : Env) (e : Expr), eval env (fold e) = eval env e",
    "fold_size": "∀ (e : Expr), (fold e).size ≤ e.size",
    "eval_simplify": "∀ (env : Env) (e : Expr), eval env (simplify e) = eval env e",
    "simplify_size": "∀ (e : Expr), (simplify e).size ≤ e.size",
    "compile_length": "∀ (e : Expr), (compile e).length = e.size",
    "compile_fold_correct": "∀ (env : Env) (e : Expr), run env (compile (fold e)) [] = [eval env e]",
    "compile_simplify_length": "∀ (e : Expr), (compile (simplify e)).length ≤ (compile e).length",
}
DEFINITIONS = """
#guard eval (fun _ => 3) (.plus (.const 2) (.times (.var "x") (.const 4))) = 14
#guard Expr.size (.plus (.const 1) (.times (.var "x") (.const 2))) = 5
#guard compile (.plus (.const 1) (.var "y")) = [.push 1, .load "y", .add]
#guard run (fun _ => 7) [.push 2, .load "z", .mul, .push 1, .add] [] = [15]
#guard run (fun _ => 0) [.add] [4] = [4]
#guard fold (.plus (.const 2) (.const 3)) = .const 5
#guard fold (.times (.var "x") (.plus (.const 1) (.const 1))) = .times (.var "x") (.const 2)
#guard fold (.plus (.var "x") (.const 0)) = .plus (.var "x") (.const 0)
#guard simplify (.plus (.const 0) (.var "x")) = .var "x"
#guard simplify (.times (.var "x") (.const 0)) = .const 0
#guard simplify (.times (.const 1) (.plus (.var "x") (.const 0))) = .var "x"
#guard simplify (.plus (.const 2) (.const 3)) = .plus (.const 2) (.const 3)
"""


def lean(text):
    check = ROOT / "AcceptCheck.lean"
    check.write_text("import Stackc\nopen Stackc\n" + text)
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
