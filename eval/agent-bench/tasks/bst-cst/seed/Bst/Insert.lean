import Bst.Lemmas

namespace Bst

theorem mem_insert (x y : Nat) (t : Tree) :
    y ∈ (insert x t).toList ↔ y = x ∨ y ∈ t.toList := by
  sorry

theorem insert_bst (x : Nat) (t : Tree) : BST t → BST (insert x t) := by
  sorry

end Bst
