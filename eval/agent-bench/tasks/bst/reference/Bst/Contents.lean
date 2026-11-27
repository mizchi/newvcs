import Bst.Insert
import Bst.Search
import Bst.Delete

namespace Bst

theorem mem_delete (x y : Nat) (t : Tree) :
    BST t → (y ∈ (delete x t).toList ↔ y ≠ x ∧ y ∈ t.toList) := by
  intro h
  induction h with
  | leaf => simp [delete, Tree.toList]
  | @node l k r hl hr bl br ihl ihr =>
    have hl' := (forallT_iff _ _).1 hl
    have hr' := (forallT_iff _ _).1 hr
    by_cases h1 : x < k
    · simp only [delete, h1, ite_true, Tree.toList, List.mem_append, List.mem_cons, ihl]
      constructor
      · rintro (⟨hne, h⟩ | rfl | h)
        · exact ⟨hne, Or.inl h⟩
        · exact ⟨by omega, Or.inr (Or.inl rfl)⟩
        · exact ⟨by have := hr' y h; omega, Or.inr (Or.inr h)⟩
      · rintro ⟨hne, h | h | h⟩
        · exact Or.inl ⟨hne, h⟩
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr h)
    · by_cases h2 : k < x
      · simp only [delete, h1, h2, ite_true, ite_false, Tree.toList, List.mem_append, List.mem_cons, ihr]
        constructor
        · rintro (h | rfl | ⟨hne, h⟩)
          · exact ⟨by have := hl' y h; omega, Or.inl h⟩
          · exact ⟨by omega, Or.inr (Or.inl rfl)⟩
          · exact ⟨hne, Or.inr (Or.inr h)⟩
        · rintro ⟨hne, h | h | h⟩
          · exact Or.inl h
          · exact Or.inr (Or.inl h)
          · exact Or.inr (Or.inr ⟨hne, h⟩)
      · have hk : x = k := by omega
        subst hk
        simp only [delete, h1, ite_false, Tree.toList, List.mem_append, List.mem_cons, mem_join]
        constructor
        · rintro (h | h)
          · exact ⟨by have := hl' y h; omega, Or.inl h⟩
          · exact ⟨by have := hr' y h; omega, Or.inr (Or.inr h)⟩
        · rintro ⟨hne, h | h | h⟩
          · exact Or.inl h
          · exact absurd h hne
          · exact Or.inr h

theorem fromList_bst (xs : List Nat) : BST (fromList xs) := by
  induction xs with
  | nil => exact .leaf
  | cons x xs ih => exact insert_bst x _ ih

theorem treeSort_sorted (xs : List Nat) : (treeSort xs).Pairwise (· < ·) :=
  toList_sorted _ (fromList_bst xs)

theorem mem_treeSort (y : Nat) (xs : List Nat) : y ∈ treeSort xs ↔ y ∈ xs := by
  induction xs with
  | nil => simp [treeSort, fromList, Tree.toList]
  | cons x xs ih =>
    simp only [treeSort, fromList, List.foldr_cons] at ih ⊢
    rw [mem_insert, ih, List.mem_cons]

end Bst
