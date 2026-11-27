import Bst.Lemmas

namespace Bst

theorem search_correct (x : Nat) (t : Tree) : BST t → (search x t = true ↔ x ∈ t.toList) := by
  sorry

theorem toList_sorted (t : Tree) : BST t → t.toList.Pairwise (· < ·) := by
  sorry

end Bst
