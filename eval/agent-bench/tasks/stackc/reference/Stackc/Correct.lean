import Stackc.Lemmas

namespace Stackc

theorem run_append (env : Env) (p q : List Instr) (s : List Nat) :
    run env (p ++ q) s = run env q (run env p s) := by
  induction p generalizing s with
  | nil => rfl
  | cons i p ih => simp [run, ih]

theorem compile_run (env : Env) (e : Expr) (p : List Instr) (s : List Nat) :
    run env (compile e ++ p) s = run env p (eval env e :: s) := by
  induction e generalizing p s with
  | const n => simp [compile, run, step, eval]
  | var x => simp [compile, run, step, eval]
  | plus a b iha ihb =>
    simp only [compile, List.append_assoc, iha, ihb]
    simp [run, step, eval]
  | times a b iha ihb =>
    simp only [compile, List.append_assoc, iha, ihb]
    simp [run, step, eval]

theorem compile_correct (env : Env) (e : Expr) :
    run env (compile e) [] = [eval env e] := by
  have h := compile_run env e [] []
  simpa [run] using h

end Stackc
