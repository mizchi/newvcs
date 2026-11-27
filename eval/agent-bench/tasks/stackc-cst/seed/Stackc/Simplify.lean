import Stackc.Lemmas

namespace Stackc

theorem eval_simplify (env : Env) (e : Expr) : eval env (simplify e) = eval env e := by
  sorry

theorem simplify_size (e : Expr) : (simplify e).size ≤ e.size := by
  sorry

end Stackc
