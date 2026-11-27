import Stackc.Correct
import Stackc.Fold
import Stackc.Simplify

namespace Stackc

theorem compile_length (e : Expr) : (compile e).length = e.size := by
  sorry

theorem compile_fold_correct (env : Env) (e : Expr) :
    run env (compile (fold e)) [] = [eval env e] := by
  sorry

theorem compile_simplify_length (e : Expr) :
    (compile (simplify e)).length ≤ (compile e).length := by
  sorry

end Stackc
