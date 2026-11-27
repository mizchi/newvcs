# What a history comes to, with no agents

`bench.py` drives writers through nv by script: each makes its steps, one commit to a step,
under the step's note; then they push in turn, and a writer whose push is refused syncs, settles
what was left with both versions (the lines of both sides), waits for the settling to be
committed, and pushes again. The ref's history is then read from a workspace that only watched.
Two binaries given the same scenarios make the same writes in the same order, so what differs
is what nv made of them.

```
eval/history-bench/bench.py --server http://localhost:8799 --nv old=<an nv> --nv now=target/fast/nv \
    --reps 5 --out <scratch>/history.tsv
eval/compare.py <scratch>/history.tsv --by binary --within scenario --spread
```

| Field | What it is |
| --- | --- |
| `granularity` | commits on the ref over steps made: 1.0 where every step is a commit of its own |
| `attribution` | of the lines the steps wrote that are once at the tip, the part brought in by a commit whose summary line is the note of the step that wrote them |
| `folds` | syncs that made several commits one |
| `refused`, `trailers` | pushes refused; commits with a `Settled:` trailer |
| `sync_ms`, `push_ms` | wall time of every sync, and of every push, together |

The times are mostly the local server's: read them over fifteen runs or more, with quartiles.

## Scenarios

| Scenario | What it is |
| --- | --- |
| `list-two-steps` | round 17's case: a second writer's two steps each add a line to the list one file holds, which the first writer changed |
| `three-lists` | three writers, two such steps each |
| `clash-in-one-step` | the clash belongs to one step of three |
| `clash-only-step` | a step that changes nothing but the file that clashes, then one that changes it again |
| `union-only` | three writers, three steps each, meeting only in a `merge=union` file |
| `disjoint` | nothing shared |

A scenario is JSON: the seed's files, and the writers, each with steps of a note and edits
(`write`, `replace` `[old, new]`, `append`, `after` `[anchor, text]`).
