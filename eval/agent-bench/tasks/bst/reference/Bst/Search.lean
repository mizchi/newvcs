import Bst.Lemmas

namespace Bst

theorem search_correct (x : Nat) (t : Tree) : BST t → (search x t = true ↔ x ∈ t.toList) := by
  intro h
  induction h with
  | leaf => simp [search, Tree.toList]
  | @node l k r hl hr bl br ihl ihr =>
    rw [forallT_iff] at hl hr
    simp only [Tree.toList, List.mem_append, List.mem_cons]
    by_cases h1 : x < k
    · simp only [search, h1, ite_true, ihl]
      constructor
      · intro h; exact Or.inl h
      · rintro (h | rfl | h)
        · exact h
        · omega
        · have := hr x h; omega
    · by_cases h2 : k < x
      · simp only [search, h1, h2, ite_true, ite_false, ihr]
        constructor
        · intro h; exact Or.inr (Or.inr h)
        · rintro (h | rfl | h)
          · have := hl x h; omega
          · omega
          · exact h
      · simp only [search, h1, h2, ite_false]
        simp only [true_iff]
        exact Or.inr (Or.inl (by omega))

theorem toList_sorted (t : Tree) : BST t → t.toList.Pairwise (· < ·) := by
  intro h
  induction h with
  | leaf => simp [Tree.toList]
  | @node l k r hl hr bl br ihl ihr =>
    rw [forallT_iff] at hl hr
    simp only [Tree.toList, List.pairwise_append, List.pairwise_cons, List.mem_cons]
    refine ⟨ihl, ⟨hr, ihr⟩, ?_⟩
    rintro a ha b (rfl | hb)
    · exact hl a ha
    · have := hl a ha; have := hr b hb; omega

end Bst
