import Stackc.Correct
import Stackc.Fold
import Stackc.Simplify

namespace Stackc

theorem compile_length (e : Expr) : (compile e).length = e.size := by
  induction e with
  | const n => rfl
  | var x => rfl
  | plus a b iha ihb => simp [compile, Expr.size, iha, ihb]; omega
  | times a b iha ihb => simp [compile, Expr.size, iha, ihb]; omega

theorem compile_fold_correct (env : Env) (e : Expr) :
    run env (compile (fold e)) [] = [eval env e] := by
  rw [compile_correct, eval_fold]

theorem compile_simplify_length (e : Expr) :
    (compile (simplify e)).length ≤ (compile e).length := by
  rw [compile_length, compile_length]
  exact simplify_size e

end Stackc
