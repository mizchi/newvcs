import Bst.Insert
import Bst.Search
import Bst.Delete

namespace Bst

theorem mem_delete (x y : Nat) (t : Tree) :
    BST t → (y ∈ (delete x t).toList ↔ y ≠ x ∧ y ∈ t.toList) := by
  sorry

theorem treeSort_sorted (xs : List Nat) : (treeSort xs).Pairwise (· < ·) := by
  sorry

theorem mem_treeSort (y : Nat) (xs : List Nat) : y ∈ treeSort xs ↔ y ∈ xs := by
  sorry

end Bst
