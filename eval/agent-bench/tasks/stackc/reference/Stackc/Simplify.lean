import Stackc.Lemmas

namespace Stackc

theorem eval_simplify (env : Env) (e : Expr) : eval env (simplify e) = eval env e := by
  induction e with
  | const n => rfl
  | var x => rfl
  | plus a b iha ihb =>
    simp only [simplify, eval]
    rw [← iha, ← ihb]
    clear iha ihb
    split <;> simp_all [eval]
  | times a b iha ihb =>
    simp only [simplify, eval]
    rw [← iha, ← ihb]
    clear iha ihb
    split <;> simp_all [eval]

theorem simplify_size (e : Expr) : (simplify e).size ≤ e.size := by
  induction e with
  | const n => simp [simplify, Expr.size]
  | var x => simp [simplify, Expr.size]
  | plus a b iha ihb =>
    simp only [simplify]
    split <;> simp_all [Expr.size] <;> omega
  | times a b iha ihb =>
    simp only [simplify]
    split <;> simp_all [Expr.size] <;> omega

end Stackc
