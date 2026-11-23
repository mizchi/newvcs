# Rounds

## Round 1 (2026-11-22)

Three tasks at once (`slugify`, `wrap`, `word_count`/`reading_time`), each in its own fork with
a worker and a committer; every task adds a line to `textkit/__init__.py`, so pushes after the
first meet a conflict. nv at `9390a8d`.

**What the ref came to** (`verify.sh`): four commits after the start, the tests passing on each;
no provisional message pushed; no markers at the tip; all three pieces of work there. Three
pushes were refused (one committer's twice, another's once), and the committers synced, settled
the conflict in `__init__.py` (keeping every export) and pushed again. Each committer took about two minutes; every nv
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

## Round 3 (2026-11-22)

The same three tasks and prompts, on nv at `92c566a`, with a seed shaped for the conflict every
round had: `textkit/__init__.py` is a list, one import and one name of `__all__` to a line, and
`.gitattributes` gives it `merge=union`. `nv sync` honours that now, and the conditions are run
on what a sync or a push made.

Replayed first on round 2's own files, Git's union merged all three without a conflict and the
tests passed, yet `__all__` named one function of four: each writer had written it on one line.
So `verify.sh` now also checks that every public function is exported, and the seed says to put
one entry to a line. All three workers did.

**What the ref came to**: four commits after the start, each passing; nothing provisional, no
markers, every function exported, and `__all__` with all four names. Three pushes were refused
again, as in rounds 1 and 2; each sync merged `__init__.py` with no markers and exit 0, and the
committers pushed again with nothing to settle.

**The committers' findings**:

| # | Finding | Met by | Done |
| --- | --- | ---: | --- |
| 18 | A union file still refuses the push: every writer after the first syncs and pushes again | 1 | The server merges it (`d703761`, round 4) |
| 19 | Right after a sync, `nv pending` says "conditions not run" while the run behind it is under way | 2 | `--wait` says it; plain `pending` does not yet |
| 20 | A sync that merged cleanly does not say the commit was made again on the new tip, as which | 1 | Not done |
| 21 | `nv rebuild` cannot split a checkpoint by path | 1 | Not done |
| 22 | `nv log` prints times as epoch milliseconds | 2 | Not done |

## Round 4 (2026-11-22)

The same tasks, prompts and seed as round 3, on nv at `d703761`: the server merges a push that
clashes only on `merge=union` paths, and the push lands.

**What the ref came to**: four commits after the start, each passing; nothing provisional, no
markers, every function exported. No push was refused: the second and third committers' pushes
landed on the first try, `__init__.py` merged on the server, and no committer ran `nv sync`.

**The committers' findings**:

| # | Finding | Met by | Done |
| --- | --- | ---: | --- |
| 23 | `nv rebuild` signs the new commits with the committer's own identity, so the developer's name is lost | 3 | Keeps the author of the first commit of each, as a squash does; the committer is whoever runs it |
| 24 | After a push the server merged, "is now at" names nothing: Git's id of the tip needed a version of the list nobody had fetched | 2 | Fetched as `nv git` does; nv's id where it still cannot be had |
| 25 | "6 file contents sent" for a commit of 4 files | 1 | The contents sent for the server to merge with are counted apart |
| 26 | The new ids of one's own commits on the ref are not printed | 2 | Not done |
| 27 | `nv git show -s`, `nv git show <rev> -- <path>`, `nv git diff A B <path>` are refused | 2 | Not done |
| 28 | Nothing says whether the tip the server merged passes the conditions, short of `nv pending --wait` | 1 | Not done |

The committers spent more than in round 3 (vcs tokens +10%, calls +6%) on the things above: two
looked at the merged file and ran the tests by hand, and all three looked into the author and
the empty tip.

## Round 5 (2026-11-22)

The same tasks, prompts and seed, on nv at `353298e`: `nv rebuild` keeps the author, and the push
names the tip after a merge on the server.

**What the ref came to**: four commits after the start, each passing; nothing provisional, no
markers, every function exported. No push was refused; the second and third landed merged on
the server, `__init__.py` with all four imports and names. The rebuilt commits are by `eval`,
the developer, and committed by whoever rebuilt them. One developer took out a line of the seed
(the blank line under the comment) as it added its own; the union on the server took that in.

The workers spent more than in round 4 (dev tokens +16%: 8, 8 and 5 tool calls against 5, 5 and
5); none of it was version control, which none of them met.

**The committers' findings**:

| # | Finding | Met by | Done |
| --- | --- | ---: | --- |
| 29 | `nv rebuild` lists the new commits without the conditions they passed; `nv pending` does | 1 | Not done |
| 30 | Nothing says whether the conditions passed on the tip the server made, short of `nv pending --wait` | 2 | Not done (they are run, behind the push) |
| 31 | "caught up 2 commits" counts the workspace's own commit as it landed | 1 | Not done |
| 32 | Which contents went "for the server to merge with" is not said; a push that caught up with nothing lists five zeros | 2 | Not done |
| 33 | `nv help rebuild` does not say the author's date is kept with the author | 1 | Not done |
| 34 | `nv git show <rev> -- <path>`, `nv git diff A B [--] <path>` are refused (as in round 4) | 1 | Not done |

## Round 6 (2026-11-23)

The same tasks and seed, on nv at `df9750b` (the push and rebuild say more), and one question more
in the committer's report: whether any commit seemed wrong to it.

**What the ref came to**: four commits after the start, each passing; nothing provisional, no
markers, every function exported; no push refused, two merged on the server. All three
committers answered "none" to the new question. One developer put its imports above the comment
in `__init__.py`; the union kept them there, the others' below it.

**Slower than round 5** (makespan +18%, latency +23%, vcs tokens +10%). The committers looked at
more: each read the diff of what landed, two ran the tests by hand after the push although the
push said the conditions were running, and the new question asks them to judge every commit,
which takes reading. Three pairs a round is also few: rounds 4 to 6 ran the same work at 85 to
100 s.

| # | Finding | Met by | Done |
| --- | --- | ---: | --- |
| 35 | `nv git show <rev> -- <path>`, `nv git diff A B [--] <path>` are refused, and they are how one checks what the server merged | 3 | Not done |
| 36 | "for the server to merge … with" is printed where the server merged nothing: the contents go in case | 1 | Not done |
| 37 | `--format=fuller`, `%ad`, `%cn`, `%cd` are refused, so a rebuild's committer cannot be checked; the refusal of `show --format=fuller` names `git log` | 1 | Not done |
| 38 | "(yours: …)" does not say outright that the id pushed is gone | 2 | Not done |

## Rounds 7 to 10 (2026-11-23): no committing agent

Rounds 1 to 6 had a committing agent beside each developer (pattern A). It cost as much as the
developer (an agent is 67,316 tokens before it does anything), and in 18 tries it made each task
one commit every time. Rounds 7 to 10 do without it (pattern C+D): the developer says in a line
what it did at each step that stands by itself (`nv note`, `worker-noted.md`), and when it is done
`publish.sh` runs `nv publish`, which waits, makes a commit at each note and pushes. From round 9
a `[body]` hook (`body.sh`, one `claude -p` with no tools, about 3,000 tokens a commit) writes each
commit's body; where `nv publish` stops at a clash, an agent is given `resolver.md` and nothing
else. A blind judge (`judge.md`) is given one history of each pattern, X and Y in either order,
and rates each commit's boundary and message from 1 to 5.

| | A (rounds 4 to 6) | 7 | 8 | 9 | 10 |
| --- | ---: | ---: | ---: | ---: | ---: |
| what was different | committing agents | notes, publish | the same | + body hook, resolver | + fixes below |
| makespan, s | 84 to 100 | 48.8 | 55.6 | 98.5 | 72.4 |
| latency, s | 42 to 52 | 0.8 | 0.9 | 48.4 | 10.6 |
| total tokens | 466,099 to 468,360 | 223,394 | 225,495 | 387,711 | 235,564 |
| version control's tool calls | 32 to 34 | 0 | 0 | 16 (2 resolvers, 5 hook calls) | 4 hook calls |
| judged against | | round 6 | round 5 | round 4 | round 6 |
| boundary, A / C+D | | 5.0 / 4.5 | 5.0 / 4.5 | 5.0 / 5.0 | 5.0 / 5.0 |
| message, A / C+D | | 5.0 / 3.5 | 5.0 / 3.25 | 5.0 / 3.33 | 5.0 / 5.0 |
| judge prefers | | A | A | A | the same |

What each judge found, and what came of it:

- **Rounds 7 and 8**: a note is a summary line, and nothing said what a reader could not see in
  the diff (that slugify keeps letters of any script, that reading_time raises ValueError). And the
  README section was a commit of its own, apart from the code it documents, which both judges
  took off for, though committers of rounds 3 to 5 had wished they could split it so. Done: the
  `[body]` hook, and the developer prompt says what documents a step goes with it.
- **Round 9**: three developers added to README.md, which is not a union, and two publishes
  stopped; a resolver settled each in one publish (5 and 6 tool calls). Their bodies said the same
  thing twice: a publish run again wrote a body under the one it wrote the first time. And a noted
  commit holding only files the sync left unsettled would have lost its line. Done: the body is
  written afresh under the summary line; a line lost so waits for the settling as a note
  (`tests/remote.rs`). The developers' README lines were not asked for: the prompt had said a step
  goes with "what documents it", and it now says documentation the task asks for.
- **Round 10**: no clash, no resolver. The judge found both histories clean and accurate, rated
  every commit 5 on both counts, and preferred neither. One developer split task 3 into word_count
  and reading_time, each with its README lines; the judge took that for a smaller review unit, not
  a fault. The bodies ended in an attribution trailer and a session line the `claude -p` added of
  its own accord; body.sh now drops such lines.

What this does and does not show: one judge per pair, a model judging models, three small tasks.
Pattern C+D of round 10 matched A's commits at half the tokens and a fifth of the latency; it needs
an agent where developers really clash (round 9), and its notes are only as good as the developer's
idea of a step. A committing agent still earns its place where the history needs judging: a review,
not a restatement.

## Round 11 (2026-11-23): round 10 again

The same as round 10, on nv at `42378e3` with body.sh dropping trailers: was its tie with A one
round's luck? No clash; no resolver; one note a task (task 3 as one commit with its README
section).

| | A (round 5) | round 11 |
| --- | ---: | ---: |
| makespan, s | 84.4 | 34.9 |
| latency, s | 42.1 | 7.2 |
| total tokens | 466,099 | 233,274 |
| version control's tool calls | 32 | 3 hook calls (9,578 tokens) |
| judge, boundary / message | 5.0 / 5.0 | 5.0 / 5.0 |

The judge (X round 11, Y round 5) checked every test and README example against the code by hand,
found no claim wrong in either, and preferred neither. Two rounds of C+D in its last form, judged
against two different rounds of A with the labels both ways round, came out level with A each
time, at half the tokens and a sixth of the latency.

## Rounds 12 and 13 (2026-11-23): made to clash

`setup.sh` with `CONFLICT=1`: the README has a list of functions holding one placeholder line,
and every task is told to replace it with its own. The second and third writers to push meet a
clash no union takes. Round 12 is pattern A (committing agents), round 13 C+D with the body hook,
run at the same time; in 13 the agent given `resolver.md` was started on each publish that stopped,
11 to 17 s after it stopped (counted out of the times below).

| | 12 (A) | 13 (C+D) |
| --- | ---: | ---: |
| makespan, s | 134.6 | 80.4 |
| latency, s | 60.8 | 36.6 |
| total tokens | 488,188 | 391,292 |
| version control's tool calls | 46 | 21 (two resolvers, 15; six hook calls) |
| refused pushes / stopped publishes | 3 | 3 |
| judge, boundary | 5.0 | 5.0 |
| judge, message | 3.67 | 5.0 |
| judge prefers | | 13 |

Both landed everything whole and passing; the README listed every function. The judge took off
for two of round 12's messages: "Add textkit.wrap, merged with slugify, word_count and
reading_time", "This is a merge with …", of commits with one parent that hold one feature. Two
things made them so. The committer prompt asked for "a message saying it is a merge". And the
sync put each committer's commit on the tip without the README it had left holding both versions:
the settling was a checkpoint of its own, after a commit short of its own message and untested,
and the committers rebuilt the two by hand, each time naming what they had merged with. One of them
pushed after two syncs and said it would have pushed a wrong message had it not rebuilt again.
Round 13 met none of it: `nv publish` folds what comes after a note into the noted commit.

Done (`6c89a8d`): the sync notes which commit of the workspace's own it left short, and of what; a
checkpoint that changes nothing but those files goes into that commit, under its message, while it
is the last made here, not pushed, and no push of it unheard; a workspace with no commit of its own
gets a commit of its own, never a change to another writer's (`tests/remote.rs`). The committer
prompt no longer asks for a merge message.

Where it does not happen, each with a test: a file every writer adds to, given `merge=union`, is
merged by the server and nothing is refused (rounds 4 to 11); `nv publish` folds the settling into
the noted commit and writes its body once (rounds 9 and 13); changes to different paths are put on
the tip by the server; and now a checkpoint completes the commit the sync left short.

## Scores

`score.py` over each round: the gates first (all passed in every round), then, lower being
better. Tokens are above a baseline agent that does nothing (67,316 tokens).

| | Round 1 | Round 2 | Round 3 | Round 4 | Round 5 | Round 6 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| makespan, s | 157.9 | 152.1 | 122.2 | 92.7 | 84.4 | 99.9 |
| latency (median, developer done to work on the ref), s | 101.1 | 74.0 | 55.5 | 41.7 | 42.1 | 52.0 |
| vcs tokens (committers) | 54,600 | 48,217 | 41,241 | 45,558 | 40,842 | 44,931 |
| dev tokens (workers) | 18,404 | 18,966 | 19,269 | 18,378 | 21,361 | 19,533 |
| vcs share | 74.8% | 71.8% | 68.2% | 71.3% | 65.7% | 69.7% |
| vcs calls (committers' tool calls) | 47 | 37 | 32 | 34 | 32 | 34 |
| refused pushes | 3 | 3 | 3 | 0 | 0 | 0 |

Against round 1, round 3 took 23% less time to the last push, 45% less latency, 24% fewer
committer tokens and 32% fewer committer calls, for the same work and the same number of
refusals. Round 4, with no refusals, took 41% less time than round 1 to the last push and 59%
less latency; its committers' tokens went back up on what they had to check (round 4 above).
Round 5, with those fixed, has the lowest committer tokens of any round: against round 1, 47%
less time to the last push, 58% less latency, 25% fewer committer tokens and 32% fewer calls. The workers' cost stayed flat (18.4k to 19.3k), as it should: version control does
not reach them. What a conflict costs a committer, from the committers of each round who met
none, one and two: round 2, 12.4k, 15.9k and 19.9k tokens; round 3, 11.2k, 14.3k and 15.7k.
