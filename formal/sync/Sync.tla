---------------------------- MODULE Sync ----------------------------
(***************************************************************************)
(* Writers pushing to and syncing with one ref, as nv does it.             *)
(*                                                                         *)
(* The server is `server/src/repo.ts` `push`: one push at a time; a push   *)
(* sent before gets the answer it got; a push on the tip lands as it is;   *)
(* one on an older commit is checked, path by path, against everything     *)
(* the ref took since that commit, and lands rebased or is refused.        *)
(*                                                                         *)
(* A workspace is `src/remote.rs` `push`, `sync` and `arrive_from`, and    *)
(* `src/workspace.rs` for going back: what it published and has not pushed *)
(* (`chain`) and its files (`disk`), on the position of the ref it last    *)
(* saw (`seen`). A sync recognises its own commits that the ref took       *)
(* without it hearing so by what they say and what they change, not by    *)
(* id, as `arrive_from` does.                                              *)
(*                                                                         *)
(* A file is two lines, merged line by line, as Git's three-way merge does *)
(* for lines far apart. A line is a content and a write: nv compares       *)
(* contents (a file is its bytes), the invariants follow writes. Every     *)
(* write remembers the writes its writer saw it replace (`wrote`). A line  *)
(* can be changed back to what it was: the same content, a new write.     *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  W,          \* the workspaces
  P,          \* the files
  MayWrite,   \* the lines each workspace may write: {<<w, p, l>>}
  MaxEdits,   \* [W -> lines a workspace writes, at most]
  MaxCommits, \* [W -> commits a workspace makes (amends included), at most]
  MaxChain,   \* commits published and not pushed, at most (2 at most: see Begin)
  MaxHist,    \* commits the ref takes, at most
  SameKeys,   \* TRUE: every commit says the same (one author, one message, one second)
  GoingBack,  \* which of these may happen too: {"amend", "aside" (`restore pushed`), "before-sync"}
  Fixed,      \* fixes of bugs this model found: {"landed-base", "unheard-guard"} (as nv has them),
              \* and {"amend-own", "push-forgets"} (others that were tried)
  Known       \* bugs found and not fixed in the model, whose counterexamples are let pass: {"aside"}

L == {1, 2}
None == <<"none">>
V0 == [c |-> <<"i", 0>>, n |-> <<"i", 0>>]
Mark(o, t) == [c |-> <<"M", o.c, t.c>>, n |-> <<"M", o.n, t.n>>]
IsMark(v) == v.c[1] = "M"
Plain(v) == IF IsMark(v) THEN {[c |-> v.c[2], n |-> v.n[2]], [c |-> v.c[3], n |-> v.n[3]]} ELSE {v}

\* Files are compared by what they hold, as nv compares them by the id of their bytes.
Same(f, g) == \A l \in L : f[l].c = g[l].c
SameCh(a, b) == DOMAIN a = DOMAIN b /\ \A p \in DOMAIN a : Same(a[p], b[p])

VARIABLES
  hist,      \* the ref: <<[key, ch, tree]>>; hist[1] is the commit everyone started on
  landed,    \* the server's `landed` table: {<<sent, position>>}
  seen,      \* [W -> position of the ref the workspace last saw]
  chain,     \* [W -> <<[key, ch, tree]>>]: published here, not on the ref as far as it knows
  disk,      \* [W -> [P -> [L -> value]]]: its files
  unsettled, \* [W -> files a sync left with both versions in]
  edits,     \* [W -> lines written so far]
  commits,   \* [W -> commits made so far]
  waiting,   \* [W -> the push it waits to hear about, or None]
  net,       \* pushes sent and not yet taken by the server
  replies,   \* the server's answers, to workspaces that still wait for them
  wrote,     \* {[n, over]}: each write, and the writes its writer saw it replace
  committed, \* {<<writer, p, l, value>>}: what was in a commit when it was made
  kept,      \* [W -> values recorded and set aside by going back, under an id nv names]
  standing,  \* [W -> what `before-sync` goes back to, or None]
  unheard,   \* [W -> how many commits a push not heard back from carried; 0 for none]
  ar         \* [W -> an arrival under way, or None]

vars == <<hist, landed, seen, chain, disk, unsettled, edits, commits, waiting, net, replies, wrote, committed, kept, standing, unheard, ar>>

Start == [p \in P |-> [l \in L |-> V0]]
NoCh == [p \in {} |-> Start[p]]
Nothing == [p \in P |-> None]

Apply(t, ch) == [p \in P |-> IF p \in DOMAIN ch THEN ch[p] ELSE t[p]]
Diff(a, b) == [p \in {q \in P : ~Same(a[q], b[q])} |-> b[p]]
Pairs(f) == {<<p, f[p]>> : p \in DOMAIN f}
Vals(t) == {t[p][l] : p \in DOMAIN t, l \in L}
Marked(f) == \E l \in L : IsMark(f[l])

\* Git's three-way merge, line by line, of contents: a line both changed differently holds both.
Merge3(was, o, t) ==
  [l \in L |-> IF o[l].c = t[l].c THEN o[l]
               ELSE IF o[l].c = was[l].c THEN t[l]
               ELSE IF t[l].c = was[l].c THEN o[l]
               ELSE Mark(o[l], t[l])]

Tip == Len(hist)
BaseRoot(w) == IF chain[w] = <<>> THEN hist[seen[w]].tree ELSE chain[w][Len(chain[w])].tree
KeyEq(a, b) == a.key = b.key /\ SameCh(a.ch, b.ch)

\* Whether a commit of the ref is `c`, as `arrive_from` tells: what it says and what it changes. With
\* "amend-own", also a commit `c` was made in place of (an amend), which is this workspace's too.
Is(r, c) == KeyEq(r, c) \/ ("amend-own" \in Fixed /\ \E x \in c.replaced : KeyEq(r, x))

\* The tree the commit of the ref that is `c` left, as this workspace made it.
LeftBy(r, c) == IF KeyEq(r, c) THEN c.tree ELSE (CHOOSE x \in c.replaced : KeyEq(r, x)).tree

\* The first commit of `refs` from position `from` on that is `c`; 0 for none.
FirstFrom(refs, c, from) ==
  IF \E j \in from..Len(refs) : Is(refs[j], c)
  THEN CHOOSE j \in from..Len(refs) : Is(refs[j], c) /\ \A i \in from..(j - 1) : ~Is(refs[i], c)
  ELSE 0

TypeOK ==
  /\ seen \in [W -> 1..Len(hist)]
  /\ \A w \in W : Len(chain[w]) <= MaxChain

---------------------------------------------------------------------------
(* What `arrive_from` makes of a workspace arriving at position `tip`.     *)
(* In four steps, each from what the one before stored in `ar`, so that    *)
(* nothing is worked out twice. Nothing else happens meanwhile (`Calm`):   *)
(* an arrival reads its own workspace and a part of the ref that does not  *)
(* change, so no other order could make it come out differently.          *)

Calm == \A w \in W : ar[w] = None

\* Its own commits the ref has, in order (`own_landed`, `own_on_ref`), and the trees it stands on.
Begin(w, tip) ==
  LET common == seen[w]
      mine == chain[w]
      refs == SubSeq(hist, common + 1, tip)
      i1 == IF Len(mine) >= 1 THEN FirstFrom(refs, mine[1], 1) ELSE 0
      i2 == IF i1 > 0 /\ Len(mine) >= 2 THEN FirstFrom(refs, mine[2], i1 + 1) ELSE 0
      count == IF i1 = 0 THEN 0 ELSE IF i2 = 0 THEN 1 ELSE 2
      ct == hist[common].tree
  IN ar' = [ar EXCEPT ![w] = [stage |-> 2, tip |-> tip, count |-> count,
                              own |-> UNION {Pairs(refs[IF k = 1 THEN i1 ELSE i2].ch) : k \in 1..count},
                              ct |-> ct, tt |-> hist[tip].tree, base |-> BaseRoot(w),
                              pf |-> IF count = 0 THEN ct ELSE LeftBy(refs[IF count = 1 THEN i1 ELSE i2], mine[count]),
                              key |-> IF mine = <<>> THEN None ELSE mine[Len(mine)].key,
                              \* What `before-sync` would go back to: the workspace as it is now.
                              was |-> [seen |-> seen[w], chain |-> chain[w], disk |-> disk[w]],
                              th |-> Nothing, pub |-> Nothing, uns |-> {}, pub2 |-> Nothing, moved |-> Nothing]]

\* Theirs and what was published here, without what both have alike.
Sides(w) ==
  LET a == ar[w]
      th1(p) == IF Same(a.ct[p], a.tt[p]) THEN None
                ELSE IF a.count > 0 /\ \E x \in a.own : x[1] = p /\ Same(x[2], a.tt[p]) THEN None
                ELSE a.tt[p]
      pb0(p) == IF Same(a.pf[p], a.base[p]) THEN None ELSE a.base[p]
      alike(p) == pb0(p) # None /\ th1(p) # None /\ Same(pb0(p), th1(p))
  IN ar' = [ar EXCEPT ![w] = [a EXCEPT !.stage = 3,
                                       !.th = [p \in P |-> IF alike(p) THEN None ELSE th1(p)],
                                       !.pub = [p \in P |-> IF alike(p) THEN None ELSE pb0(p)]]]

\* What a commit published here is merged on: nv takes what the old common point had
\* (`was = at(from, path)`), even where its own commits landed since and changed the file.
MergeBase(a, p) == IF "landed-base" \in Fixed THEN a.pf[p] ELSE a.ct[p]

\* Commit against commit.
Commits(w) ==
  LET a == ar[w]
      mg == [p \in P |-> IF a.pub[p] # None /\ a.th[p] # None THEN Merge3(MergeBase(a, p), a.pub[p], a.th[p]) ELSE None]
      uns == {p \in P : mg[p] # None /\ Marked(mg[p])}
      mine(p) == IF mg[p] # None THEN mg[p] ELSE a.pub[p]
  IN ar' = [ar EXCEPT ![w] = [a EXCEPT !.stage = 4,
                                       !.uns = uns,
                                       !.pub2 = [p \in P |-> IF a.pub[p] = None \/ p \in uns THEN None
                                                              ELSE IF a.th[p] # None /\ Same(a.th[p], mine(p)) THEN None
                                                              ELSE mine(p)],
                                       !.moved = [p \in P |-> IF mg[p] # None /\ ~Marked(mg[p]) THEN mg[p] ELSE a.th[p]]]]

\* Then the files onto that, and the workspace on the tip.
Files(w) ==
  LET a == ar[w]
      d == disk[w]
      changed(p) == ~Same(d[p], a.base[p])
      m(p) == Merge3(a.base[p], d[p], a.moved[p])
      merging(p) == p \notin a.uns /\ changed(p) /\ a.moved[p] # None /\ ~Same(d[p], a.moved[p])
      bothP == a.uns \cup {p \in P : merging(p) /\ Marked(m(p))}
      ch == [p \in {q \in P : a.pub2[q] # None} |-> a.pub2[p]]
  IN /\ disk' = [disk EXCEPT ![w] =
          [p \in P |-> IF p \in a.uns THEN Merge3(MergeBase(a, p), d[p], a.moved[p])
                       ELSE IF merging(p) THEN m(p)
                       ELSE IF changed(p) THEN d[p]
                       ELSE IF a.moved[p] # None THEN a.moved[p]
                       ELSE d[p]]]
     /\ chain' = [chain EXCEPT ![w] = IF DOMAIN ch = {} THEN <<>>
                                      ELSE <<[key |-> a.key, ch |-> ch, tree |-> Apply(a.tt, ch), replaced |-> {}]>>]
     /\ unsettled' = [unsettled EXCEPT ![w] = bothP]
     /\ seen' = [seen EXCEPT ![w] = a.tip]
     /\ standing' = [standing EXCEPT ![w] = [was |-> a.was, left |-> chain'[w]]]
     /\ unheard' = [unheard EXCEPT ![w] = IF @ > 0 /\ a.count >= @ THEN 0 ELSE @]
     /\ ar' = [ar EXCEPT ![w] = None]

Arriving(w) ==
  /\ ar[w] # None
  /\ \/ ar[w].stage = 2 /\ Sides(w) /\ UNCHANGED <<seen, chain, disk, unsettled, standing, unheard>>
     \/ ar[w].stage = 3 /\ Commits(w) /\ UNCHANGED <<seen, chain, disk, unsettled, standing, unheard>>
     \/ ar[w].stage = 4 /\ Files(w)
  /\ UNCHANGED <<hist, landed, edits, commits, waiting, net, replies, wrote, committed, kept>>

---------------------------------------------------------------------------

Init ==
  /\ hist = <<[key |-> <<"init">>, ch |-> NoCh, tree |-> Start]>>
  /\ landed = {}
  /\ seen = [w \in W |-> 1]
  /\ chain = [w \in W |-> <<>>]
  /\ disk = [w \in W |-> Start]
  /\ unsettled = [w \in W |-> {}]
  /\ edits = [w \in W |-> 0]
  /\ commits = [w \in W |-> 0]
  /\ waiting = [w \in W |-> None]
  /\ net = {}
  /\ replies = {}
  /\ wrote = {}
  /\ committed = {}
  /\ kept = [w \in W |-> {}]
  /\ standing = [w \in W |-> None]
  /\ unheard = [w \in W |-> 0]
  /\ ar = [w \in W |-> None]

Idle(w) == waiting[w] = None /\ Calm

\* With "unheard-guard": nothing goes back past a push that may have landed unheard, nor makes its commits again.
Heard(w) == "unheard-guard" \notin Fixed \/ unheard[w] = 0

\* The writes the writer of write `n` saw it replace, closed: each write notes those of what it replaced.
Over(n) == UNION {x.over : x \in {y \in wrote : y.n = n}}

Write(w, p, l, content) ==
  LET n == <<w, edits[w] + 1>>
      old == disk[w][p][l]
      f == [disk[w][p] EXCEPT ![l] = [c |-> content, n |-> n]]
  IN /\ disk' = [disk EXCEPT ![w][p] = f]
     /\ wrote' = wrote \cup {[n |-> n, over |-> {x.n : x \in Plain(old)} \cup UNION {Over(x.n) : x \in Plain(old)}]}
     /\ unsettled' = [unsettled EXCEPT ![w] = IF Marked(f) THEN @ ELSE @ \ {p}]
     /\ edits' = [edits EXCEPT ![w] = @ + 1]

\* A writer writes one line. A line holding both versions is settled by writing it.
Edit(w, p, l) ==
  /\ <<w, p, l>> \in MayWrite
  /\ Idle(w)
  /\ edits[w] < MaxEdits[w]
  /\ Write(w, p, l, <<w, edits[w] + 1>>)
  /\ UNCHANGED <<hist, landed, seen, chain, commits, waiting, net, replies, committed, kept, standing, ar, unheard>>

\* A writer changes a line back to what the ref had where the workspace last saw it.
ChangeBack(w, p, l) ==
  /\ <<w, p, l>> \in MayWrite
  /\ Idle(w)
  /\ edits[w] < MaxEdits[w]
  /\ ~IsMark(disk[w][p][l])
  /\ disk[w][p][l].c # hist[seen[w]].tree[p][l].c
  /\ Write(w, p, l, hist[seen[w]].tree[p][l].c)
  /\ UNCHANGED <<hist, landed, seen, chain, commits, waiting, net, replies, committed, kept, standing, ar, unheard>>

Record(w, ch) == {<<w, p, l, ch[p][l]>> : p \in DOMAIN ch, l \in L}

\* `nv commit`: refused while a file holds a sync's markers.
Commit(w) ==
  /\ Idle(w)
  /\ unsettled[w] = {}
  /\ commits[w] < MaxCommits[w]
  /\ Len(chain[w]) < MaxChain
  /\ LET ch == Diff(BaseRoot(w), disk[w])
         key == IF SameKeys THEN <<"m">> ELSE <<w, commits[w] + 1>>
     IN /\ DOMAIN ch # {}
        /\ \A p \in DOMAIN ch : ~Marked(ch[p])
        /\ chain' = [chain EXCEPT ![w] = Append(@, [key |-> key, ch |-> ch, tree |-> disk[w], replaced |-> {}])]
        /\ committed' = committed \cup Record(w, ch)
  /\ commits' = [commits EXCEPT ![w] = @ + 1]
  /\ UNCHANGED <<hist, landed, seen, disk, unsettled, edits, waiting, net, replies, wrote, kept, standing, ar, unheard>>

\* `nv git commit --amend`: the last commit published here and not pushed, made again with what
\* the files are now. Refused where there is none (on the ref's own commit after a sync).
Amend(w) ==
  /\ "amend" \in GoingBack
  /\ Heard(w)
  /\ Idle(w)
  /\ unsettled[w] = {}
  /\ commits[w] < MaxCommits[w]
  /\ chain[w] # <<>>
  /\ LET n == Len(chain[w])
         below == IF n = 1 THEN hist[seen[w]].tree ELSE chain[w][n - 1].tree
         ch == Diff(below, disk[w])
         key == IF SameKeys THEN <<"m">> ELSE <<w, commits[w] + 1>>
     IN /\ DOMAIN ch # {}
        /\ ~SameCh(ch, chain[w][n].ch)
        /\ \A p \in DOMAIN ch : ~Marked(ch[p])
        /\ chain' = [chain EXCEPT ![w][n] = [key |-> key, ch |-> ch, tree |-> disk[w],
                                             replaced |-> chain[w][n].replaced \cup
                                                          {[key |-> chain[w][n].key, ch |-> chain[w][n].ch, tree |-> chain[w][n].tree]}]]
        /\ committed' = committed \cup Record(w, ch)
  /\ commits' = [commits EXCEPT ![w] = @ + 1]
  /\ UNCHANGED <<hist, landed, seen, disk, unsettled, edits, waiting, net, replies, wrote, kept, standing, ar, unheard>>

\* `nv restore <dir> pushed`: to the ref as last seen, everything else recorded under an id.
SetAside(w) ==
  /\ "aside" \in GoingBack
  /\ Heard(w)
  /\ Idle(w)
  /\ chain[w] # <<>> \/ disk[w] # hist[seen[w]].tree
  /\ kept' = [kept EXCEPT ![w] = @ \cup UNION {Plain(v) : v \in Vals(disk[w])}
                                     \cup UNION {Vals(chain[w][k].tree) : k \in 1..Len(chain[w])}]
  /\ chain' = [chain EXCEPT ![w] = <<>>]
  /\ disk' = [disk EXCEPT ![w] = hist[seen[w]].tree]
  /\ unsettled' = [unsettled EXCEPT ![w] = {}]
  /\ UNCHANGED <<hist, landed, seen, edits, commits, waiting, net, replies, wrote, committed, standing, ar, unheard>>

\* `nv restore <dir> before-sync`: what is on disk recorded, then the workspace as it was before
\* the last sync, refused if it committed since. A push forgets it.
BeforeSync(w) ==
  /\ "before-sync" \in GoingBack
  /\ Heard(w)
  /\ Idle(w)
  /\ standing[w] # None
  /\ chain[w] = standing[w].left
  /\ kept' = [kept EXCEPT ![w] = @ \cup UNION {Plain(v) : v \in Vals(disk[w])}]
  /\ seen' = [seen EXCEPT ![w] = standing[w].was.seen]
  /\ chain' = [chain EXCEPT ![w] = standing[w].was.chain]
  /\ disk' = [disk EXCEPT ![w] = standing[w].was.disk]
  /\ unsettled' = [unsettled EXCEPT ![w] = {}]
  /\ UNCHANGED <<hist, landed, edits, commits, waiting, net, replies, wrote, committed, standing, ar, unheard>>

\* `nv push`: the chain and where the workspace last saw the ref, in one request.
Push(w) ==
  /\ Idle(w)
  /\ unsettled[w] = {}
  /\ chain[w] # <<>>
  /\ LET sent == <<chain[w], seen[w]>>
     IN /\ net' = net \cup {[w |-> w, chain |-> chain[w], base |-> seen[w], sent |-> sent]}
        /\ waiting' = [waiting EXCEPT ![w] = sent]
  \* With "push-forgets", what a sync made is not undone once a push of it is sent: it may land unheard.
  /\ standing' = IF "push-forgets" \in Fixed THEN [standing EXCEPT ![w] = None] ELSE standing
  /\ unheard' = IF "unheard-guard" \in Fixed THEN [unheard EXCEPT ![w] = Len(chain[w])] ELSE unheard
  /\ UNCHANGED <<hist, landed, seen, chain, disk, unsettled, edits, commits, replies, wrote, committed, kept, ar>>

\* A workspace killed while it waits: what it sent may still land, and it never hears so.
Kill(w) ==
  /\ Calm
  /\ waiting[w] # None
  /\ waiting' = [waiting EXCEPT ![w] = None]
  /\ replies' = {r \in replies : r.w # w}
  /\ UNCHANGED <<hist, landed, seen, chain, disk, unsettled, edits, commits, net, wrote, committed, kept, standing, ar, unheard>>

Answer(r, status, pos) ==
  replies' = IF waiting[r.w] = r.sent THEN replies \cup {[w |-> r.w, sent |-> r.sent, status |-> status, pos |-> pos]} ELSE replies

\* The server takes one push.
Serve(r) ==
  /\ Calm
  /\ r \in net
  /\ net' = net \ {r}
  /\ IF \E x \in landed : x[1] = r.sent
     THEN \* Sent again: the answer it got.
          /\ Answer(r, "ok", (CHOOSE x \in landed : x[1] = r.sent)[2])
          /\ UNCHANGED <<hist, landed>>
     ELSE LET clash == {p \in UNION {DOMAIN r.chain[k].ch : k \in 1..Len(r.chain)} :
                          \E i \in (r.base + 1)..Tip : p \in DOMAIN hist[i].ch}
          IN IF r.base # Tip /\ clash # {}
             THEN /\ Answer(r, "conflict", Tip)
                  /\ UNCHANGED <<hist, landed>>
             ELSE \* On the tip as it is, or rebased onto it: the same changes.
                  LET n == Len(r.chain)
                      t1 == Apply(hist[Tip].tree, r.chain[1].ch)
                      c1 == [key |-> r.chain[1].key, ch |-> r.chain[1].ch, tree |-> t1]
                      new == IF n = 1 THEN <<c1>>
                             ELSE <<c1, [key |-> r.chain[2].key, ch |-> r.chain[2].ch, tree |-> Apply(t1, r.chain[2].ch)]>>
                  IN /\ Len(hist) + n <= MaxHist
                     /\ hist' = hist \o new
                     /\ landed' = landed \cup {<<r.sent, Len(hist) + n>>}
                     /\ Answer(r, "ok", Len(hist) + n)
  /\ UNCHANGED <<seen, chain, disk, unsettled, edits, commits, waiting, wrote, committed, kept, standing, ar, unheard>>

\* The answer to a push: landed, and the workspace arrives where it landed, and forgets what it
\* could have undone; or refused, and nothing changes.
Hear(w) ==
  /\ Calm
  /\ \E r \in replies :
       /\ r.w = w /\ r.sent = waiting[w]
       /\ replies' = replies \ {r}
       /\ waiting' = [waiting EXCEPT ![w] = None]
       /\ IF r.status = "ok"
          THEN Begin(w, r.pos) /\ standing' = [standing EXCEPT ![w] = None]
          ELSE UNCHANGED <<ar, standing>>
       /\ unheard' = [unheard EXCEPT ![w] = 0]
  /\ UNCHANGED <<hist, landed, seen, chain, disk, unsettled, edits, commits, net, wrote, committed, kept>>

\* `nv sync`: refused while a file holds a sync's markers.
Sync(w) ==
  /\ Idle(w)
  /\ unsettled[w] = {}
  /\ Tip > seen[w]
  /\ Begin(w, Tip)
  /\ UNCHANGED <<hist, landed, seen, chain, disk, unsettled, edits, commits, waiting, net, replies, wrote, committed, kept, standing, unheard>>

Next ==
  \/ \E w \in W, p \in P, l \in L : Edit(w, p, l) \/ ChangeBack(w, p, l)
  \/ \E w \in W : Commit(w) \/ Amend(w) \/ SetAside(w) \/ BeforeSync(w)
  \/ \E w \in W : Push(w) \/ Kill(w) \/ Hear(w) \/ Sync(w) \/ Arriving(w)
  \/ \E r \in net : Serve(r)

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
(* What must hold.                                                         *)

\* A line the ref changes is changed by someone who saw what it replaces.
NoLostUpdate ==
  \A i \in 2..Len(hist) : \A p \in P, l \in L :
    LET old == hist[i - 1].tree[p][l]
        new == hist[i].tree[p][l]
    IN old.c # new.c => old.n \in Over(new.n)

\* Nothing reaches the ref that was not committed, and no markers.
OnlyCommitted ==
  \A p \in P, l \in L :
    LET v == hist[Tip].tree[p][l]
    IN v = V0 \/ \E x \in committed : x[4].n = v.n

\* A committed write wins over what its writer saw it replace: the ref does not end up holding a
\* line that a commit replaced, unless that commit is still with its writer (in its files, in
\* what it has not pushed, or set aside under an id) or was replaced in turn by someone who saw it.
CommittedWins ==
  \A p \in P, l \in L : \A x \in committed :
    LET v == hist[Tip].tree[p][l]
        w == x[1]  u == x[4]
        holds(f) == \E y \in Plain(f[l]) : y.c = u.c
    IN (x[2] = p /\ x[3] = l /\ u.c # v.c /\ v.n \in Over(u.n)) =>
         \/ holds(disk[w][p])
         \/ \E k \in 1..Len(chain[w]) : holds(chain[w][k].tree[p])
         \/ \E y \in kept[w] : y.n = u.n
         \/ \E y \in wrote : u.n \in y.over

\* A sync never leaves a writer its own two versions of a line to choose between. (With "aside"
\* known: one that it set aside, after a push of it landed unheard, is still left so.)
NoConflictWithSelf ==
  \A w \in W, p \in P, l \in L :
    LET v == disk[w][p][l]
        theirs == [c |-> v.c[3], n |-> v.n[3]]
    IN IsMark(v) => v.n[2][1] # v.n[3][1] \/ ("aside" \in Known /\ theirs \in kept[w])

\* A push lands once.
LandsOnce ==
  \A i, j \in 2..Len(hist) : i < j => ~KeyEq(hist[i], hist[j])
=============================================================================
