/-! Arithmetic expressions over named variables, and what they evaluate to. -/
namespace Stackc

abbrev Env := String → Nat

inductive Expr where
  | const (n : Nat)
  | var (x : String)
  | plus (a b : Expr)
  | times (a b : Expr)
deriving Repr, DecidableEq

def eval (env : Env) : Expr → Nat
  | .const n => n
  | .var x => env x
  | .plus a b => eval env a + eval env b
  | .times a b => eval env a * eval env b

def Expr.size : Expr → Nat
  | .const _ => 1
  | .var _ => 1
  | .plus a b => a.size + b.size + 1
  | .times a b => a.size + b.size + 1

end Stackc
