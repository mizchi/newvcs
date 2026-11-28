/-! Binary search trees of natural numbers: insertion, search, deletion, and a sort by them. -/
namespace Bst

inductive Tree where
  | leaf
  | node (l : Tree) (k : Nat) (r : Tree)
deriving Repr, DecidableEq

/-- The keys of a tree, in order. -/
def Tree.toList : Tree → List Nat
  | .leaf => []
  | .node l k r => l.toList ++ k :: r.toList

/-- Every key of the tree has the property. -/
def ForallT (p : Nat → Prop) : Tree → Prop
  | .leaf => True
  | .node l k r => ForallT p l ∧ p k ∧ ForallT p r

/-- A search tree: every key on the left of a node is below it, every key on its right above. -/
inductive BST : Tree → Prop
  | leaf : BST .leaf
  | node {l : Tree} {k : Nat} {r : Tree} :
      ForallT (· < k) l → ForallT (k < ·) r → BST l → BST r → BST (.node l k r)

def insert (x : Nat) : Tree → Tree
  | .leaf => .node .leaf x .leaf
  | .node l k r =>
    if x < k then .node (insert x l) k r
    else if k < x then .node l k (insert x r)
    else .node l k r

def search (x : Nat) : Tree → Bool
  | .leaf => false
  | .node l k r =>
    if x < k then search x l
    else if k < x then search x r
    else true

/-- The smallest key of `node l k r`, and the tree without it. -/
def removeMin : Tree → Nat → Tree → Nat × Tree
  | .leaf, k, r => (k, r)
  | .node ll lk lr, k, r => ((removeMin ll lk lr).1, .node (removeMin ll lk lr).2 k r)

/-- Two trees as one, every key of the first below every key of the second. -/
def join : Tree → Tree → Tree
  | l, .leaf => l
  | l, .node rl rk rr => .node l (removeMin rl rk rr).1 (removeMin rl rk rr).2

def delete (x : Nat) : Tree → Tree
  | .leaf => .leaf
  | .node l k r =>
    if x < k then .node (delete x l) k r
    else if k < x then .node l k (delete x r)
    else join l r

def fromList (xs : List Nat) : Tree := xs.foldr insert .leaf

/-- A list sorted, each value once: the keys of the tree its values are inserted into. -/
def treeSort (xs : List Nat) : List Nat := (fromList xs).toList

end Bst
