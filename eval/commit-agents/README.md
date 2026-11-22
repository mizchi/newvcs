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

What is judged, besides what `verify.sh` says: what the agents report got in their way, and how
many commands the committing agent needed that failed or that it had to work out. The rounds
and what came of each are in `rounds.md`.
