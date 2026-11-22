# Rounds

## Round 1 (2026-11-22)

Three tasks at once (`slugify`, `wrap`, `word_count`/`reading_time`), each in its own fork with
a worker and a committer; every task adds a line to `textkit/__init__.py`, so pushes after the
first meet a conflict. nv at `9390a8d`.

**What the ref came to** (`verify.sh`): four commits after the start, the tests passing on each;
no provisional message pushed; no markers at the tip; all three pieces of work there. Two of the
three pushes were refused, and the committers synced, settled the conflict in `__init__.py`
(keeping every export) and pushed again. Each committer took about two minutes; every nv
command took well under a second.

**The workers** (3 of 3) noticed nothing: no file they did not write, nothing appearing or
going, no slowness. None ran a version control command.

**The committers' findings**, with how many of the three met each:

| # | Finding | Met by |
| --- | --- | ---: |
| 1 | `nv pending` prints nv's ids; `nv git show`, `diff`, `log` take only Git's, and nothing joins the two | 3 |
| 2 | `nv git status` says "ahead of 'origin/main' by N commits, use git push" after a push that landed: origin is the Git repository the workspace was cloned from, not the ref `nv remote` set | 3 |
| 3 | `nv pending` marks a new file `M` | 2 |
| 4 | Nothing says whether what is on disk is committed and the conditions are done running: each committer polled with sleeps and compared `nv status`, `nv since . base` and `nv pending` | 3 |
| 5 | After a sync that left markers, nv says "edit them, then `nv commit` and `nv push`"; with `[commit] each` the watcher commits the settled file itself, provisionally | 2 |
| 6 | The commit a sync puts on the tip leaves the settled file out of it, and is "conditions not run on it": on its own its tests fail; the committers found that they had to rebuild it together with the watcher's commit of the settling | 2 |
| 7 | Which ids `nv rebuild` takes (number, nv id, Git id) is not said; ids change at every step, and only the numbers stay | 2 |
| 8 | `nv push` says "1 commits", and not which ref it moved or to what | 2 |
| 9 | `nv git log origin/main..HEAD` is refused as "`git origin/main..HEAD` (a range of commits)", the word `log` missing | 1 |
| 10 | Exit codes (2 for a refused push, 1 for a sync that left markers) are not in `nv help` | 1 |
| 11 | A refused push does not name the commits on the ref it clashed with | 1 |

**Done after round 1**, in `13c11e3`: 1, 3, 4 (`nv pending --wait`), 5, 7, 8, 9, 10, and the
count half of 11. Not done: 6, which is how a sync treats a file it leaves unsettled, and is
left as it is (nothing unsettled goes into a commit); what was done is to say so where the
committer looks. 2 was done for a workspace with a ref set by `nv remote`.

## Round 2 (2026-11-22)

The same three tasks, the same prompts, on nv at `13c11e3`.

**What the ref came to** (`verify.sh`): four commits after the start, the tests passing on each;
no provisional message pushed; no markers at the tip; all three pieces of work there. The pairs
finished in the order slugify, stats, wrap: the second push was refused once, the third twice,
and each committer synced, settled `__init__.py` and folded the settling into its own commit
with `nv rebuild`, which the sync's message now tells it to do. In round 1 two of three found
that by trial.

| | Round 1 | Round 2 |
| --- | ---: | ---: |
| Committer's tool calls (each of three) | not counted | 9, 12, 16 |
| Committer's time | about 2 min each | 1.4, 2.0, 2.5 min |
| Waiting for the background commit | polled with sleeps (3 of 3) | `nv pending --wait`, 0.03–0.2 s (3 of 3) |
| Commits on the ref that pass their tests | 4 of 4 | 4 of 4 |
| Findings the committers listed | 11 | 6, 4 of them new |

**The workers** (3 of 3) noticed nothing in their way. Two saw `.nv/` and `nv.toml` and said
what they were for; none ran a version control command.

**The committers' findings**:

| # | Finding | Met by | Done |
| --- | --- | ---: | --- |
| 12 | Checkpoint commits are signed `nv <nv@localhost>`, a name nv made up; after `nv rebuild` they are the committer's | 1 | Signed as any commit is; with nobody to sign as, not committed |
| 13 | After a sync, the commit it put on the tip has lost the unsettled file and keeps its message: nothing in `nv pending` says so | 2 | `nv pending` says which files are in none of its commits until settled |
| 14 | "1 commits" in `nv rebuild` and in a sync's summary | 3 | Done |
| 15 | `nv since` with nothing different prints nothing | 2 | Left: it prints paths, as `git diff --name-status` does |
| 16 | The task says "commit the result with a message saying it is a merge"; there is no merge commit to make | 2 | The prompt's wording; left so the rounds compare |
| 17 | A refused push does not say whose commit it clashed with | 1 | Left: the client has not got that commit yet |

What worked without trial, by their reports: `--wait`; Git ids in `nv pending` and in
`nv rebuild`; `nv git status` against the ref; the push's line naming the ref and tip; the
refused push's count and next step.
