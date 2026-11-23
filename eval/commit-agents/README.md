# Agents that do not commit, and an agent that does

An evaluation of one way of working: the developing agent does no version control at all, and
another agent, in the background, makes the commits. Several such pairs work at once, each in
a fork of one workspace (as Git's worktrees are), and all push to one ref.

nv does the part that needs no judgment. A watcher records every write; `nv.toml` runs the
tests behind each; with `[commit] each = true` every state the tests pass on is committed then
and there, under a provisional message (`checkpoint: unit passed`). These commits are made to
be made again: the committing agent reads them (`nv pending`), makes them again as a few
commits that each say what they are (`nv rebuild`, each new commit ending at one that passed),
and pushes, syncing and settling conflicts when another pair pushed first.

## A round

```
eval/commit-agents/setup.sh <work-dir> http://localhost:8799 <tasks>   # server: cd server && pnpm dev -- --port 8799
```

makes a small Python package (`textkit`) on a new ref, and one fork of it for each task, a
watcher in each. Then, for each task, two agents in parallel: one given `worker.md` with the
task, one given `committer.md`, with `{{DIR}}`, `{{DONE}}`, `{{BIN}}` and `{{TASK}}` filled in.
The worker writes `{{DONE}}` when it has finished; the committer waits for that, makes the
commits again, and pushes.

```
eval/commit-agents/verify.sh <work-dir> <ref-url>
```

then says what the ref came to: each commit after the start, its message and files, and
whether the tests pass on it; how many provisional messages were pushed; whether any file at
the tip holds a conflict's markers.

## Without a committing agent

`worker-noted.md` is a developer that may run `nv note` and nothing else. With it, no committing
agent is started: `publish.sh <work-dir> <n>` waits for task n's done file and runs `nv publish`,
which makes a commit at each note and pushes. `setup.sh` with `BODY=1` gives the seed a `[body]`
hook (`body.sh`) that writes each commit's body. Where `nv publish` stops over a clash a union does
not take, an agent is given `resolver.md`. `judge.md` is a blind judge of two histories of the same
tasks. `rounds.md` has what came of each.

## Scores

```
eval/commit-agents/verify.sh <work-dir> <ref-url> > <work-dir>/verify.txt
eval/commit-agents/score.py <work-dir> [<an earlier round's work-dir>]
```

`score.py` reads `verify.txt` and `usage.tsv`, which the one running the round writes from what
it is told as each agent finishes (tokens, tool calls, milliseconds; for a committer, the pushes
it had refused), with one more agent as a baseline: told to answer "OK" and use no tools, it is
what any agent costs before it does anything, and tokens are counted above it.

First the gates: every commit on the ref passes its tests, no provisional message and no marker
reached it, every public function is exported, a commit for every task. A round that fails one
has no score: nothing is traded for speed. Then, lower being better:

| Score | What it is |
| --- | --- |
| makespan | seconds from launch until the last committer was done |
| latency | the median over tasks of how long after its developer was done the work was on the ref (the committer's time less the worker's; both start together) |
| vcs tokens | the committers' tokens above the baseline, in all |
| dev tokens | the workers' tokens above the baseline: version control should not cost them |
| vcs share | vcs tokens as a part of vcs and dev tokens |
| vcs calls | the committers' tool calls, each a turn |

Against an earlier round it prints each with the change in per cent. The tokens are as the
harness reports them for an agent: they grow with what it read and wrote, not with its turns,
so the tool calls stand for the turns. A committer polls for the developer's file every ten
seconds, so latency is to within that.

What is judged besides: what the agents report got in their way, and how many commands the
committing agent needed that failed or that it had to work out. The rounds and what came of
each are in `rounds.md`.
