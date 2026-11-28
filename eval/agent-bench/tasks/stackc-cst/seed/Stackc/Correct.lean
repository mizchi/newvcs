import Stackc.Lemmas

namespace Stackc

theorem run_append (env : Env) (p q : List Instr) (s : List Nat) :
    run env (p ++ q) s = run env q (run env p s) := by
  sorry

theorem compile_run (env : Env) (e : Expr) (p : List Instr) (s : List Nat) :
    run env (compile e ++ p) s = run env p (eval env e :: s) := by
  sorry

theorem compile_correct (env : Env) (e : Expr) :
    run env (compile e) [] = [eval env e] := by
  sorry

end Stackc
