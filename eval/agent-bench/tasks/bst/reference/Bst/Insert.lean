import Bst.Lemmas

namespace Bst

theorem mem_insert (x y : Nat) (t : Tree) :
    y ∈ (insert x t).toList ↔ y = x ∨ y ∈ t.toList := by
  induction t with
  | leaf => simp [insert, Tree.toList]
  | node l k r ihl ihr =>
    by_cases h1 : x < k
    · simp only [insert, h1, ite_true, Tree.toList, List.mem_append, List.mem_cons, ihl]
      constructor
      · rintro ((h | h) | h | h) <;> simp_all
      · rintro (h | h | h | h) <;> simp_all
    · by_cases h2 : k < x
      · simp only [insert, h1, h2, ite_true, ite_false, Tree.toList, List.mem_append, List.mem_cons, ihr]
        constructor
        · rintro (h | h | h | h) <;> simp_all
        · rintro (h | h | h | h) <;> simp_all
      · have : x = k := by omega
        subst this
        simp only [insert, h1, ite_false, Tree.toList, List.mem_append, List.mem_cons]
        constructor
        · intro h; exact Or.inr h
        · rintro (rfl | h)
          · exact Or.inr (Or.inl rfl)
          · exact h

theorem insert_bst (x : Nat) (t : Tree) : BST t → BST (insert x t) := by
  intro h
  induction h with
  | leaf => exact .node trivial trivial .leaf .leaf
  | @node l k r hl hr bl br ihl ihr =>
    by_cases h1 : x < k
    · simp only [insert, h1, ite_true]
      refine .node ?_ hr ihl br
      rw [forallT_iff] at hl ⊢
      intro y hy
      rcases (mem_insert x y l).1 hy with rfl | hy
      · exact h1
      · exact hl y hy
    · by_cases h2 : k < x
      · simp only [insert, h1, h2, ite_true, ite_false]
        refine .node hl ?_ bl ihr
        rw [forallT_iff] at hr ⊢
        intro y hy
        rcases (mem_insert x y r).1 hy with rfl | hy
        · exact h2
        · exact hr y hy
      · simp only [insert, h1, h2, ite_false]
        exact .node hl hr bl br

end Bst
