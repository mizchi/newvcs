---------------------------- MODULE MC ----------------------------
(* The lines each configuration lets each writer write. *)
EXTENDS Sync
\* `a` writes and changes back line 1; `b` writes either line.
AOneBBoth == {<<"a", "f", 1>>, <<"b", "f", 1>>, <<"b", "f", 2>>}
\* `a` writes line 1 and `b` line 2: going back, with another writer elsewhere in the file.
AOneBTwo == {<<"a", "f", 1>>, <<"b", "f", 2>>}
\* Both write either line of either file.
Everything == {<<w, p, l>> : w \in {"a", "b"}, p \in {"f", "g"}, l \in {1, 2}}
\* How much each writer does.
TwoEach == [w \in {"a", "b"} |-> 2]
TwoAndOne == [w \in {"a", "b"} |-> IF w = "a" THEN 2 ELSE 1]
=============================================================================
