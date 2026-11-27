import Stackc.Expr

/-! A stack machine. -/

namespace Stackc

inductive Instr where
  | push (n : Nat)
  | load (x : String)
  | add
  | mul
deriving Repr, DecidableEq

def step (env : Env) : Instr → List Nat → List Nat
  | .push n, s => n :: s
  | .load x, s => env x :: s
  | .add, b :: a :: s => (a + b) :: s
  | .mul, b :: a :: s => (a * b) :: s
  | _, s => s

def run (env : Env) : List Instr → List Nat → List Nat
  | [], s => s
  | i :: p, s => run env p (step env i s)

end Stackc
