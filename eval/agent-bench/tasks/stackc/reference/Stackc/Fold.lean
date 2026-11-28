import Stackc.Lemmas

namespace Stackc

theorem eval_fold (env : Env) (e : Expr) : eval env (fold e) = eval env e := by
  induction e with
  | const n => rfl
  | var x => rfl
  | plus a b iha ihb =>
    simp only [fold]
    split <;> simp_all [eval]
  | times a b iha ihb =>
    simp only [fold]
    split <;> simp_all [eval]

theorem fold_size (e : Expr) : (fold e).size ≤ e.size := by
  induction e with
  | const n => simp [fold, Expr.size]
  | var x => simp [fold, Expr.size]
  | plus a b iha ihb =>
    simp only [fold]
    split <;> simp_all [Expr.size] <;> omega
  | times a b iha ihb =>
    simp only [fold]
    split <;> simp_all [Expr.size] <;> omega

end Stackc
