# bst

Binary search trees of natural numbers, and what is proved of them. Lean 4, nothing but its
core library.

## Proved

Each theorem, ticked once its proof has no `sorry`:

- [ ] `mem_insert`: a key is in a tree after inserting `x` when it is `x` or was in it
- [ ] `insert_bst`: insertion keeps a search tree a search tree
- [ ] `search_correct`: search finds exactly the keys of a search tree
- [ ] `toList_sorted`: the keys of a search tree, in order, are strictly increasing
- [ ] `delete_bst`: deletion keeps a search tree a search tree
- [ ] `mem_delete`: after deleting `x` from a search tree, its keys are the others it had
- [ ] `treeSort_sorted`: a tree sort is strictly increasing
- [ ] `mem_treeSort`: a tree sort holds the values of the list, and no others

## Building

`lake build`
