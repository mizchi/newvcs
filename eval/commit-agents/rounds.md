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
