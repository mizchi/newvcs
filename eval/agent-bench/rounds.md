# Rounds of the agent bench

Each row a median over rounds of the `strkit` task (four agents, one to a part, all pushing to
one ref), on the local Worker, 2026-11-27, every agent on the CLI's default model. Tokens are
every agent's, input cached or not and output; cost is what the CLI reported. Every round of
every pattern reached all 28 acceptance tests.

| | git | nv, before | nv, v2 | nv, v3 | nv, v4 | supervisor |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| rounds | 4 | 1 | 2 | 2 | 3 | 1 |
| last agent done, s | 114.5 | 146.0 | 156.6 | 146.6 | **100.0** | 331.1 |
| tokens | 2.16 M | 2.39 M | 2.40 M | 2.42 M | **1.92 M** | 3.31 M |
| cost, USD | 1.58 | 1.90 | 1.91 | 1.88 | **1.54** | 2.23 |
| tool calls | 44.5 | 46 | 46 | 47 | **38** | 74 |
| pushes refused | 5 | 5 | 5 | 5 | 5 | 2 |
| commits on the ref | 8 | 9 | 8 | 7.5 | 8 | 6 |
| of them passing the tests | 8 | 7 | 8 | 7.5 | 8 | 6 |
| work set aside and made again by hand | | 4 | 4, 4 | 2, 4 | **0, 0, 0** | |

The binaries, each the build of the commit named:

- **before** (3d45fb4): a settling given a message is a commit of its own.
- **v2** (eed22aa): `nv commit <dir>` given no message completes the commit a settling belongs
  to, under its message, and the sync says so.
- **v3** (36a1bfb): the commits before that one, which changed the file, are given their own
  version of it with what the settling brought in.
- **v4** (d5fc3bf): `nv pending` lists what each commit is held out of until the settling, and
  it and the sync say what each commit will get.

## What the transcripts showed

- **Before**, a writer that settled a clash with `nv commit <dir> <message>` made the settling a
  commit of its own, and its two commits below it were on the ref without `errors.py`, failing
  on import (2 of 9 commits). Another, not knowing where its settling would go, set its work
  aside (`nv restore`), and made its commits again by hand from files it had saved: three times,
  each time refused again because others pushed meanwhile.
- **v2** fixed the failing commits, not the redoing: the settling went into the last commit
  that changed the file, the earlier one lost its change to it (`DurationError` again), and the
  writer saw it and redid its commits.
- **v3** gave the earlier commits their versions, but `nv pending` still listed them without the
  files and said the settling would go into the last: writers believed it and redid their
  commits before trying.
- **v4**: no writer redid anything. Each settled, ran `nv commit <dir>`, and pushed; 38 tool
  calls a round against Git's 44.5.
- **The supervisor** is the slowest by far in its one round: the developers were done in 75 s,
  and it took them one by one for four minutes more, five to eight seconds of thinking a step
  (and 120 s lost to a heredoc of its own that waited on stdin).

## The supervisor in parallel (pattern `parallel`)

The supervisor's work split in two: a planner that tells the developers how to keep apart and
writes how to settle each shared file (`plan.md`), then an integrator for each developer, all at
once, each doing for its developer what the supervisor did for all of them. `strkit`, nv v4;
each a round, the supervisor's own rounds beside them.

| | supervisor | parallel, plan first | parallel, `ready` first | parallel, one push at a time |
| --- | ---: | ---: | ---: | ---: |
| last agent done, s | 171.1, 139.1 | 184.1, 195.1 | 153.1, 156.1 | 170.2, 170.1 |
| developers started at, s | 20, 23 | 59, 68 | 38, 35 | 27, 26 |
| cost, USD | 1.90, 1.66 | 2.90, 3.06 | 2.71, 2.68 | 2.75, 2.70 |
| tool calls | 63, 43 | 80, 82 | 66, 62 | 76, 70 |
| pushes refused | 2, 3 | 6, 6 | 5, 5 | 0, 0 |

- **The supervisor, rerun on v4, took half its first round's time** (331 s before v2): fewer
  refusals, no work redone, no heredoc left waiting.
- **Planning held everyone back**: writing the rules of settling before `ready` cost 40 s of
  every agent's time. Written after `ready`, while the developers work, the developers start
  as early as the supervisor's do.
- **Integrators at once collide**: every push touches the same four shared files, so each is
  refused over the last one that landed, and each sync, settle and push is 20 to 30 s of an
  agent's thinking. Taking turns at the ref (a lock directory) ends the refusals and not the
  wait: four settlings one after another, each by an agent that has to read the rules first,
  take longer than one supervisor settling four times with all of it in mind (17 s each).
- What settling costs is an agent's turn, and the parallel pattern pays it once for every
  integrator, a planner on top. While every part meets every other in files a clash needs
  judgment in, one supervisor is the cheaper and quicker of the two.

## With nothing to settle (`strkit-union`)

The same task with `merge=union` for every file the parts meet in, so that a clash never needs
settling: the server merges such paths itself, Git's pull merges them. One round each, nv v4.

| | git | nv | supervisor | parallel |
| --- | ---: | ---: | ---: | ---: |
| last agent done, s | 103.1 | 99.1 | 220.1 | 289.2, 179.1 |
| cost, USD | 1.75 | 1.76 | 2.24 | 3.74, 2.87 |
| pushes refused | 8 | 0 | 0 | 0, 0 |
| commits on the ref | 8 | 14 | 11 | 14, 10 |

- **nv's pushes were never refused**: the server merged what the writers had in common. Git's
  were refused eight times, each a pull that merged without a word, and the time came out the
  same.
- **The supervisor patterns were slower with nothing to settle than with clashes.** The
  planner's rules said what order the shared lines go in; a union keeps both sides' lines in
  the order they landed, so the integrators rewrote the shared files to the rules' order and
  pushed that. Two such rewrites pushed at once are merged by union too, and a union of two
  rewrites of the same lines has each line twice: in the slower round the ref took six commits
  of "settle", "put back in order" and "remove the repeated lines" after the four parts had
  landed. That is what `merge=union` is, in nv as in Git (both sides' lines, where both changed
  the same ones), and it is safe for lines added, not for lines moved or removed.

## Merged by the syntax tree (`strkit-cst`)

`strkit` with `errors.py`, the README and the changelog given `merge=cst`, the driver of
`tools/merge-cst` (tree-sitter, unit by unit, text with markers where it cannot decide), defined
in every workspace and clone; `__init__.py` a union as before. nv at aaada63 (merge drivers),
Git 2.43 running the same driver on its pulls.

| | git | nv | supervisor | parallel |
| --- | ---: | ---: | ---: | ---: |
| rounds | 3 | 3 | 1 | 2 |
| last agent done, s | 97.0, 98.0, 119.1 | **74.1, 78.0, 84.1** | 170.1 | 161.1, 168.1 |
| tokens | 2.12 to 2.28 M | **1.60 M** | 2.76 M | 3.00, 3.11 M |
| cost, USD | 1.54 to 1.65 | **1.36 to 1.38** | 1.93 | 2.61, 2.68 |
| tool calls | 44 to 48 | **32** | 65 | 68, 63 |
| pushes refused | 5 to 7 | 3 | 3 | 5, 4 |
| files left with both versions | 0 | 0 | 0 | 0 |
| commits, all passing their tests | 8 to 10 | 8 | 12 | 5, 7 |

- **No clash in any round needed settling**: every file both sides changed was merged by the
  driver, and every round reached all 28 acceptance tests. Against `strkit` (text merges), the
  nv pattern is 22% quicker (100 to 78 s, medians) and 11% cheaper, with 16% fewer tool calls.
- **nv against Git, with the same driver, no longer overlaps**: the slowest nv round (84 s) is
  quicker than the quickest Git round (97 s), at fewer tokens and calls in every round. A push
  refused costs either a sync or a pull and a second push; Git's writers made more of them, and
  read more around them.
- **The supervisor patterns gained nothing**: their time is the planner's and each agent's own
  turns, not settling. The parallel one still has its integrators refused over each other
  (the server runs no driver, and a refusal is a sync and a push of an agent's), and the planner
  still holds everyone for 35 to 40 s.

## Proofs in Lean (`stackc`, `stackc-cst`)

Ten theorems of a stack-machine compiler and two rewritings of expressions, in Lean 4.36's core
library, stated with `sorry` in four files, one to a developer, meeting in a file of shared
lemmas, the README's checklist of what is proved and the changelog. The acceptance tests (13)
check each theorem against its statement and that it depends on the standard axioms only, the
definitions' behaviour, and the checklist and changelog. Every round of every pattern reached
13 of 13; the proofs took a developer 40 to 90 s, so what is measured here is mostly the work
around them. `stackc-cst` merges the shared files by `tools/merge-cst` (Lean by its commands).

| | rounds | last agent done, s | cost, USD | tool calls | refused | clashes | redone by hand |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `stackc`, git | 3 | 92.1, 113.1, 114.2 | 1.29 to 1.34 | 45 to 49 | 4 to 6 | 6 to 8 | |
| `stackc`, nv | 3 | 94.1, 105.5, 112.2 | 1.35 to 1.43 | 40 to 44 | 4 to 5 | 4 to 5 | |
| `stackc-cst`, git | 3 | 71.1, 75.1, 98.1 | 1.01 to 1.19 | 31 to 37 | 3 to 5 | 0 | 0 |
| `stackc-cst`, nv, driver as first written | 6 | 94.3 to 205.2 | 1.15 to 1.50 | 35 to 44 | 3 to 6 | 0 | 0 to 1 a round |
| `stackc-cst`, nv, driver in the base's order | 3 | 79.1, 83.1, 85.1 | 1.23 to 1.32 | 38 to 40 | 3 to 5 | 0 | 0 |
| `stackc-cst`, nv, and `nv help` short | 3 | **67.0, 70.1, 73.0** | 1.12, 1.12, 1.13 | 33 to 36 | 3 to 5 | 0 | 0 |

(The Git rows of `stackc-cst` are of the driver in the base's order. One parallel and one
supervisor round of `stackc-cst`, before either change: 120.3 s and 165.1 s, $2.08 and $1.74.)

- **With text merges nv and Git came out even**: the same clashes on the checklist and the
  changelog, settled by the same kind of turn.
- **The driver as first written put a side's ticks out of place** in the checklist, when the
  other had ticked the lines before them: a writer put the order back by hand and pushed again,
  and since a sync putting the commits on the tip one by one did not come to what merging them
  at once did, it folded them, and the writer set its work aside and made the commits again.
  Placing every unit by the base's order fixed both (a4a2e16, a test of each).
- **`nv help` was 8 KB**, read by every writer and carried through each of its turns after: of
  what nv's writers read of version control, 39 to 48 thousand characters a round against Git's
  26 to 28, most was that. Cut to the everyday commands (897 bytes, `nv help all` for the rest),
  a round read 20 to 29 thousand, and nv's rounds became the quickest of any pattern here, at
  Git's cost.

## A larger proof (`bst-cst`)

Binary search trees in Lean 4's core library: insertion, search, deletion through `removeMin`
and `join`, and a tree sort; eight theorems in four files, about 250 lines of proof in the
reference, meeting in the shared lemmas, the README's checklist and the changelog, all merged
by `tools/merge-cst`. 11 acceptance tests. nv at 3c58494 (short `nv help`).

| | rounds | last agent done, s | cost, USD | tool calls | refused | clashes | commits folded |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| git | 3 | 111.1, 113.1, 113.3 | 1.29 to 1.42 | 37 to 42 | 3 to 4 | 1 each | 0 |
| nv | 3 | 121.1, 127.2, 134.1 | 1.50 to 1.56 | 43 to 45 | 4 each | 1 each | 1 each |
| nv, a commit of clashed files alone kept (0a48480) | 3 | **105.1, 110.1, 117.1** | 1.45 to 1.70 | 40 to 50 | 3 to 7 | 0 to 1 | **0** |
| nv, Git's verbs toward the ref (99416be) | 3 | **91.0, 91.1, 103.1** | 1.38 to 1.54 | 37 to 45 | 3 to 6 | 0 | 0 |
| nv-git: the Git prompt, `git` being nv's (b929317) | 3 | 88.0, 95.0, 101.1 | 1.37 to 1.49 | 40 to 46 | 2 to 5 | 0 | 0 |
| supervisor | 1 | 200.2 | 1.89 | 53 | 3 | 0 | 0 |
| parallel | 1 | 190.3 | 2.55 | 60 | 4 | 3 | 1 |

Every round reached 11 of 11. It was not as large to the writers as to whoever wrote the
reference: the longest a developer took was 134 s.

- **Every round met one clash nothing merges**: two developers wrote a lemma of one name in
  `Bst/Lemmas.lean`, each its own way (Git's rebase stopped on it as nv's sync did).
- **In every nv round that clash folded the writer's commits into one**, and the sync said both
  that each commit would get its own version of the file and that they were now one commit. One
  of the commits changed nothing but the shared lemmas, which is a case the sync folds (README,
  "Not done"); Git's rebase stops at that commit instead, and it is settled there. Nobody redid
  anything by hand, but the history is coarser than Git's (7 to 9 commits against 10 to 11),
  and each nv round took a little longer and cost a little more.
- **Kept through the sync** (0a48480: such a commit is on the tip with nothing in it until the
  settling gives it its version), no round folded, and nv's rounds took 105 to 117 s against
  Git's 111 to 113, at a little more cost (median 1.47 against 1.40) and with 8 or 9 commits
  against 10 or 11: the writers made fewer. One writer, finding its shared lemma already on the
  ref under the same name, set its work aside and made its commits again without it: what a
  Git writer amends.
- **What the extra cost was**: against Git's rounds, nv's made 4.7 more model calls a round (47.7
  against 43), each reading the whole context again: 260 thousand more tokens read from the
  cache, most of the difference. 3.7 of those calls a round did nothing but look nv up (`nv help`,
  `nv help all`, `nv help commit`); Git's writers know Git. The rest was a round with more pushes
  refused (7, the order pushes arrive in) and one writer's own remaking of its commits.
- **Git's verbs toward the ref** (99416be: `nv git push` is `nv push`, `nv git pull --rebase` is
  `nv sync`, where they had gone to the repository the workspace was cloned from; the writer's
  prompt says so in a clause): no writer used them. Every one still pushed and synced with nv's
  own commands, as the prompt shows them, and looked nv up 3 to 5 times a round. The rounds were
  quicker (91 to 103 s, median 91, against Git's 111 to 113) and a little cheaper (mean 1.47
  against 1.54 before, Git 1.37), with 10 or 11 commits as Git's; none of it is from the verbs,
  and three rounds do not separate it from the order pushes arrived in.
- **Given the Git prompt, with `git` on the path being nv's** (pattern `nv-git`, the Git
  writers' prompt but for a line saying whose `git` it is), no writer looked nv up: 0 lookups,
  against 3 to 5 a round. They committed, pushed and pulled with `--rebase` as Git's writers do
  (`git rebase --continue`, made to finish a stopped pull, went unused: each settled by a commit).
  The turns went instead to Git commands nv does not speak, each refused and tried again
  otherwise: `commit` with a second `-m` (7 times in three rounds), `commit -F` from a file or
  standard input (6), `diff` of a path named without `--` and `diff --diff-filter=U` (5), and
  `fetch` toward the ref (4, refused by design). The rounds were as quick as nv's (median 95 s,
  Git's 113) at what nv's cost (mean 1.45, nv's 1.47, Git's 1.37).

## Counting Git's refusals

Until 952150c, a refusal and a clash were counted by the first lines Git prints of them
(`[rejected]`, `CONFLICT (`). Git's writers cut its output with `tail`, which keeps the hints
and drops those lines, so Git's were undercounted: now its hints count too (`Updates were
rejected`, `Could not apply`). The tables above are counted again from the transcripts; nv's
were counted right, and of Git's only the refusals of `strkit-union` (5 to 8) and
`strkit-cst` (4 to 6, now 5 to 7) changed, and one round's clashes in `strkit` (4 to 5).

## Not yet known

- Four rounds of Git and three of v4 are few, and their ranges overlap: makespan 100 to 131 s
  against 98 to 123 s, tokens 1.96 to 2.58 M against 1.81 to 2.11 M, tool calls 40 to 54
  against 36 to 41. What does not overlap is the work redone by hand: 2 to 4 in every round of
  nv before v4, none in v4.
- The supervisor has had three rounds, the parallel pattern six, `strkit-union` one of each,
  `strkit-cst` one or two of the supervisor patterns.
- Whether the syntax-tree merge is right where nothing tests it: here the acceptance tests and
  each writer's own checked what it made. Two units put in at one place, or a change to a unit
  the other side moved, can each be right and wrong together.
- One task. A task whose parts depend on each other, or that is larger, may tell otherwise.
