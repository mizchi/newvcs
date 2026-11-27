# Agents given a task of several parts, and what it cost them to get there

`run.py` gives a task of several parts to as many agents as it has parts, each in a workspace
(or a clone) of its own, all pushing to one ref, and measures what it took for the ref to come
to a state the task's acceptance tests pass on. No agent sees those tests. The agents are
`claude -p`, with no settings of the machine's (no hooks, no MCP servers), the file and shell
tools only, and their transcripts kept.

```
eval/agent-bench/run.py --task strkit --pattern supervisor --nv target/fast/nv \
    --server http://localhost:8799 --work <scratch>/round --out <scratch>/results.tsv --label now
eval/compare.py <scratch>/results.tsv --by pattern,label --spread
```

`--dry` sets up the round and reads the ref with no agents: a try of the harness, which costs
nothing. The server is the local Worker (`cd server && pnpm dev -- --port 8799`); the `git`
pattern needs none.

## Patterns

| Pattern | Who does the version control | Prompts |
| --- | --- | --- |
| `supervisor` | one agent, for everyone: it tells the developers how to keep apart before they start, then makes each one's commits again, pushes, syncs and settles; the developers write, test and note (`nv note`), with a watcher in each workspace, the tests behind every write and a commit at each state that passes | `prompts/supervisor.md`, `prompts/worker-supervised.md` |
| `nv` | each developer, with `nv commit`, `nv push` and `nv sync` | `prompts/worker-nv.md` |
| `git` | each developer, with Git, in a clone of one bare repository: what the others are measured against | `prompts/worker-git.md` |

## Tasks

A task is a directory under `tasks/`: `seed/` (the project as it starts), `tasks.json` (its
name, the command that runs its tests, what every developer is told, and one text for each part),
`accept/` (the acceptance tests, run at the tip from the project's root) and `reference/` (an
implementation the acceptance tests pass on, which checks the tests, not the agents).

`strkit` has four parts, independent in code (durations, Roman numerals, versions, templates)
and meeting in five files: the package's `__init__.py` (a union), `errors.py`, the README's list
of functions and its sections, and the changelog. Its 28 acceptance tests are each part's
behaviour, to the letter of its text, and what the parts share: every name exported once, every
error class in `errors.py`, every function listed once, every section once and in place, every
changelog line once, no markers anywhere.

## What a round gives

One row of the TSV:

| Field | What it is |
| --- | --- |
| `accept_passed`, `accept_total` | acceptance tests passing at the tip |
| `reached` | 1 where all passed and no agent was stopped for time |
| `makespan_s` | from the first agent started to the last one ended |
| `first_start_s` | when the first developer started: the supervisor's planning |
| `tokens` | every agent's tokens, input (cached or not) and output |
| `output_tokens`, `cost_usd` | of those, the output; and what the CLI says they cost |
| `turns`, `tool_calls` | every agent's turns, and its tool calls |
| `vcs_calls` | tool calls whose command runs `nv` or `git` |
| `refused`, `clashes` | tool results that say a push was refused, or that a file was left with both versions |
| `commits`, `commits_passing` | commits on the ref after the start, and how many pass the project's tests |
| `provisional`, `markers` | `checkpoint:` messages on the ref; files at the tip with markers |
| `timed_out` | 1 where agents were stopped at `--timeout` |

`<work>/result.json` has each agent's own numbers, its final report, and the ref's log.

Rounds vary: the same prompts give different paths. Read medians over several rounds of each,
with their quartiles (`compare.py --spread`), before believing a difference.
