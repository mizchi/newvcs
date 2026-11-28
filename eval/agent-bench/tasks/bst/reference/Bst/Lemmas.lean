import Bst.Tree

/-! Lemmas more than one proof uses. -/

namespace Bst

theorem forallT_iff (p : Nat → Prop) (t : Tree) : ForallT p t ↔ ∀ x ∈ t.toList, p x := by
  induction t with
  | leaf => simp [ForallT, Tree.toList]
  | node l k r ihl ihr =>
    simp only [ForallT, Tree.toList, ihl, ihr, List.mem_append, List.mem_cons]
    constructor
    · rintro ⟨hl, hk, hr⟩ x (hx | rfl | hx)
      · exact hl x hx
      · exact hk
      · exact hr x hx
    · intro h
      exact ⟨fun x hx => h x (Or.inl hx), h k (Or.inr (Or.inl rfl)), fun x hx => h x (Or.inr (Or.inr hx))⟩

end Bst
