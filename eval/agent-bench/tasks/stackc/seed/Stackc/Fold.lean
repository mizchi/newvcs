import Stackc.Lemmas

namespace Stackc

theorem eval_fold (env : Env) (e : Expr) : eval env (fold e) = eval env e := by
  sorry

theorem fold_size (e : Expr) : (fold e).size ≤ e.size := by
  sorry

end Stackc
