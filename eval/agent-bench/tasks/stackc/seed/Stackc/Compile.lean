import Stackc.Machine

/-! Expressions compiled to the stack machine, and two rewritings of them. -/

namespace Stackc

def compile : Expr → List Instr
  | .const n => [.push n]
  | .var x => [.load x]
  | .plus a b => compile a ++ compile b ++ [.add]
  | .times a b => compile a ++ compile b ++ [.mul]

/-- Constant folding: an operation on two constants is the constant it comes to. -/
def fold : Expr → Expr
  | .plus a b =>
    match fold a, fold b with
    | .const m, .const n => .const (m + n)
    | a', b' => .plus a' b'
  | .times a b =>
    match fold a, fold b with
    | .const m, .const n => .const (m * n)
    | a', b' => .times a' b'
  | e => e

/-- Simplification: `0 + e`, `e + 0`, `1 * e` and `e * 1` are `e`; `0 * e` and `e * 0` are `0`. -/
def simplify : Expr → Expr
  | .plus a b =>
    match simplify a, simplify b with
    | .const 0, b' => b'
    | a', .const 0 => a'
    | a', b' => .plus a' b'
  | .times a b =>
    match simplify a, simplify b with
    | .const 0, _ => .const 0
    | _, .const 0 => .const 0
    | .const 1, b' => b'
    | a', .const 1 => a'
    | a', b' => .times a' b'
  | e => e

end Stackc
