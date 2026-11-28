import Bst.Lemmas

namespace Bst

theorem mem_removeMin (l : Tree) (k : Nat) (r : Tree) (y : Nat) :
    y ∈ (Tree.node l k r).toList ↔ y = (removeMin l k r).1 ∨ y ∈ (removeMin l k r).2.toList := by
  induction l generalizing k r with
  | leaf => simp [removeMin, Tree.toList]
  | node ll lk lr ih _ =>
    have h := ih lk lr
    simp only [Tree.toList, List.mem_append, List.mem_cons] at h
    simp only [removeMin, Tree.toList, List.mem_append, List.mem_cons]
    rw [h, or_assoc]

theorem removeMin_bst (l : Tree) (k : Nat) (r : Tree) : BST (.node l k r) →
    BST (removeMin l k r).2 ∧ ForallT ((removeMin l k r).1 < ·) (removeMin l k r).2 := by
  induction l generalizing k r with
  | leaf =>
    intro h
    cases h with
    | node _ hr _ br => exact ⟨br, hr⟩
  | node ll lk lr ih _ =>
    intro h
    cases h with
    | node hl hr bl br =>
      obtain ⟨b', f'⟩ := ih lk lr bl
      have hl' := (forallT_iff _ _).1 hl
      have hr' := (forallT_iff _ _).1 hr
      have sub : ∀ y, y ∈ (removeMin ll lk lr).2.toList → y < k := fun y hy =>
        hl' y ((mem_removeMin ll lk lr y).2 (Or.inr hy))
      have mk : (removeMin ll lk lr).1 < k := hl' _ ((mem_removeMin ll lk lr _).2 (Or.inl rfl))
      simp only [removeMin]
      refine ⟨.node ((forallT_iff _ _).2 sub) hr b' br, ⟨f', mk, ?_⟩⟩
      exact (forallT_iff _ _).2 fun y hy => by have := hr' y hy; omega

theorem mem_join (l r : Tree) (y : Nat) : y ∈ (join l r).toList ↔ y ∈ l.toList ∨ y ∈ r.toList := by
  cases r with
  | leaf => simp [join, Tree.toList]
  | node rl rk rr =>
    have h := mem_removeMin rl rk rr y
    simp only [Tree.toList, List.mem_append, List.mem_cons] at h
    simp only [join, Tree.toList, List.mem_append, List.mem_cons]
    rw [h]

theorem join_bst (l r : Tree) : BST l → BST r → (∀ a ∈ l.toList, ∀ b ∈ r.toList, a < b) →
    BST (join l r) := by
  intro bl br cross
  cases r with
  | leaf => simpa [join] using bl
  | node rl rk rr =>
    obtain ⟨b', f'⟩ := removeMin_bst rl rk rr br
    simp only [join]
    refine .node ?_ f' bl b'
    exact (forallT_iff _ _).2 fun a ha => cross a ha _ ((mem_removeMin rl rk rr _).2 (Or.inl rfl))

theorem mem_delete_of (x y : Nat) (t : Tree) : y ∈ (delete x t).toList → y ∈ t.toList := by
  induction t with
  | leaf => simp [delete, Tree.toList]
  | node l k r ihl ihr =>
    by_cases h1 : x < k
    · simp only [delete, h1, ite_true, Tree.toList, List.mem_append, List.mem_cons]
      rintro (h | h | h)
      · exact Or.inl (ihl h)
      · exact Or.inr (Or.inl h)
      · exact Or.inr (Or.inr h)
    · by_cases h2 : k < x
      · simp only [delete, h1, h2, ite_true, ite_false, Tree.toList, List.mem_append, List.mem_cons]
        rintro (h | h | h)
        · exact Or.inl h
        · exact Or.inr (Or.inl h)
        · exact Or.inr (Or.inr (ihr h))
      · simp only [delete, h1, h2, ite_false, Tree.toList, List.mem_append, List.mem_cons, mem_join]
        rintro (h | h)
        · exact Or.inl h
        · exact Or.inr (Or.inr h)

theorem delete_bst (x : Nat) (t : Tree) : BST t → BST (delete x t) := by
  intro h
  induction h with
  | leaf => exact .leaf
  | @node l k r hl hr bl br ihl ihr =>
    by_cases h1 : x < k
    · simp only [delete, h1, ite_true]
      refine .node ?_ hr ihl br
      exact (forallT_iff _ _).2 fun y hy => (forallT_iff _ _).1 hl y (mem_delete_of x y l hy)
    · by_cases h2 : k < x
      · simp only [delete, h1, h2, ite_true, ite_false]
        refine .node hl ?_ bl ihr
        exact (forallT_iff _ _).2 fun y hy => (forallT_iff _ _).1 hr y (mem_delete_of x y r hy)
      · simp only [delete, h1, h2, ite_false]
        refine join_bst l r bl br fun a ha b hb => ?_
        have := (forallT_iff _ _).1 hl a ha
        have := (forallT_iff _ _).1 hr b hb
        omega

end Bst
