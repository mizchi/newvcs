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

## Not yet known

- Four rounds of Git and three of v4 are few, and their ranges overlap: makespan 100 to 131 s
  against 98 to 123 s, tokens 1.96 to 2.58 M against 1.81 to 2.11 M, tool calls 40 to 54
  against 36 to 41. What does not overlap is the work redone by hand: 2 to 4 in every round of
  nv before v4, none in v4.
- The supervisor has had one round, before any of these changes.
- One task. A task whose parts depend on each other, or that is larger, may tell otherwise.
