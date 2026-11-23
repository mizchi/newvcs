# Developers that do not commit

2026-11-22 to 23. A way of working was built into nv and tried with agents for ten rounds:
developer agents that never run version control, several at once in forks of one workspace,
all pushing to one ref, with the commits made for them. Each round was scored for speed and
tokens, and from round 7 a blind judge rated the commits. The harness is `eval/commit-agents/`;
`rounds.md` there has every round in full. This is how it went.

## Result

The pattern the rounds arrived at commits as well as a committing agent does, at half the tokens
and a fifth of the wait:

| | A: an agent commits for each developer (rounds 4 to 6) | C+D: notes and `nv publish` (round 10) |
| --- | ---: | ---: |
| from launch to the last push landed | 84 to 100 s | 72 s |
| from a developer's done to its work on the ref (median) | 42 to 52 s | 11 s |
| tokens for the round, every agent's start included | 466,099 to 468,360 | 235,564 |
| version control's tool calls | 32 to 34 | 4 (body hook calls) |
| blind judge: boundary, A / C+D (1 to 5) | | 5.0 / 5.0 |
| blind judge: message, A / C+D (1 to 5) | | 5.0 / 5.0, "the same" |

In C+D the developer says in a line what each step that stands by itself did (`nv note`); when it
is done, `nv publish` waits for what is coming, makes a commit at each note, has a `[body]` hook
write each body from the note and the change, and pushes; the server merges files every writer
adds to (`merge=union`). An agent is called only where a push meets a clash nobody can settle
without judgment. Every round passed the same gates: each commit on the ref passes its tests, no
provisional message and no conflict marker reaches it, every function is exported.

## The setting

Three tasks on a small Python package (`textkit`): add `slugify`, add `wrap`, add `word_count`
and `reading_time` with a README section. Each task is in a fork of one workspace with a watcher;
`nv.toml` runs the tests behind every write and commits each state that passes
(`[commit] each`). Every task adds to `textkit/__init__.py`, so every push after the first meets
the others' work. Agents are subagents with prompt files; the server is the Worker run locally.

The scores (`score.py`) count tokens above a baseline agent told to say "OK" (67,316 tokens: what
any agent costs before it does anything), and, from round 7, the round's total with those starts
included, since that is what is paid. Each round is three pairs: a single round moves by 10 to
20% on its own (rounds 4 to 6 ran the same work at 85 to 100 s).

## Pattern A: an agent that commits (rounds 1 to 6)

| | 1 | 2 | 3 | 4 | 5 | 6 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| makespan, s | 157.9 | 152.1 | 122.2 | 92.7 | 84.4 | 99.9 |
| latency, s | 101.1 | 74.0 | 55.5 | 41.7 | 42.1 | 52.0 |
| committers' tokens above the start | 54,600 | 48,217 | 41,241 | 45,558 | 40,842 | 44,931 |
| committers' tool calls | 47 | 37 | 32 | 34 | 32 | 34 |
| refused pushes | 3 | 3 | 3 | 0 | 0 | 0 |

What changed between rounds, each from what the committers reported:

- **Round 1 to 2: saying what was meant.** All three committers polled with sleeps to learn
  whether the background commit was done, read nv's ids where `nv git` wanted Git's, and were told
  "ahead of origin" by `nv git status` after a push that had landed. Added: `nv pending --wait`,
  Git ids in `nv pending`, `nv git status` against the ref `nv remote` set, a sync message that
  says how a settled file becomes part of a commit. Committer calls fell from 47 to 37.
- **Round 3: the conflict every round had.** Every task added a line to one file. Git's
  `merge=union` attribute, now honoured by `nv sync` and `nv git merge` (case 34, against Git),
  keeps both sides' lines. Replayed on round 2's files, union merged all three with the tests
  passing and `__all__` naming one function of four, because each writer had put it on one line:
  the seed now has one entry to a line, and the harness checks every function is exported.
  Conditions now run on what a sync or a push made. Latency fell to 55 s.
- **Round 4: the server merges.** A push still had to be refused and synced for each writer after
  the first. The server now merges a push that clashes only on union paths, with nv's own merge
  code compiled to WebAssembly (`server/merger`), checking what the pusher says of the base
  against the ref's history (`server/test/hostile.ts` sends the lies). No push was refused, and the
  committers spent the time saved on checking what the server had done.
- **Rounds 5 and 6: what the output said.** `nv rebuild` had put the committer's name on the
  developer's work; the push named no tip after a merge on the server; counts were wrong. Fixed.

The committers were asked from round 6 whether any commit seemed wrong; earlier reports said so
unasked. Four kinds came up: a commit that failed its tests alone because a sync left a file out
of it (rounds 1, 2), the wrong author (2, 4), checkpoints too coarse or too fine to make good
commits of (3 to 5), and commits landed on the ref in a state nobody had tested (4 to 6).

Two observations decided what came next. A committing agent cost as much as the developer it
served: by round 6 the round's total was 468,360 tokens, more than half of it the committers'.
And in 18 tries every committer made each task exactly one commit: a whole agent to squash.

## Pattern C+D: notes, a publish, a hook (rounds 7 to 10)

Added to nv:

- `nv note <dir> <line>`: the next commit the conditions pass on is made under that line; where the
  runner has committed those files already under a message of its own, that commit is renamed.
- `nv publish <dir> [--message <line>]`: waits, makes a commit at each note (what came after the
  last under the message), keeps each one's author, pushes; catches up and pushes again where the
  catching up leaves nothing for a person; stops and says so where it does.
- A `[body]` hook in `nv.toml`, asked by `nv publish` for each commit's body. `body.sh` is one
  `claude -p` with no tools and a one-line system prompt: about 3,000 tokens a commit, where an
  agent starts at 67,000.

| | 7 | 8 | 9 | 10 |
| --- | ---: | ---: | ---: | ---: |
| what was different | notes, publish | the same | body hook; an agent where publish stops | the fixes below |
| makespan, s | 48.8 | 55.6 | 98.5 | 72.4 |
| latency, s | 0.8 | 0.9 | 48.4 | 10.6 |
| tokens for the round | 223,394 | 225,495 | 387,711 | 235,564 |
| judged against round | 6 | 5 | 4 | 6 |
| boundary, A / C+D | 5.0 / 4.5 | 5.0 / 4.5 | 5.0 / 5.0 | 5.0 / 5.0 |
| message, A / C+D | 5.0 / 3.5 | 5.0 / 3.25 | 5.0 / 3.33 | 5.0 / 5.0 |
| the judge prefers | A | A | A | the same |

- **Rounds 7 and 8** were twice as fast at half the cost, and judged worse: a note is a summary
  line, with nothing a reader could not see in the diff, and the README section, noted as a step
  of its own, was a commit apart from the code it documents. Both judges took that off, though
  committers in rounds 3 to 5 had wished they could split it so.
- **Round 9** added the body hook, and a developer prompt saying a step goes with "what documents
  it". Two developers then added README lines nobody had asked for, and with the third's section
  they clashed; README is not a union. Two publishes stopped, and an agent given `resolver.md`
  (settle, test, publish again) finished each in one publish. The judge found the boundaries as
  good as A's and two bodies saying the same thing twice: a publish run again had written a body
  under the one it wrote the first time. Reproducing it found a second hole: a noted commit holding
  nothing but the files the sync left unsettled vanished from the tip, and its line with it.
- **Round 10** with both fixed and the prompt saying "documentation the task asks for": no clash,
  every commit rated 5 for boundary and message, no preference. One developer split a task into its
  two functions, each with its README lines; the judge took that for a smaller unit to review. The
  bodies ended in an attribution trailer the `claude -p` had added of its own accord; body.sh drops
  such lines now.

## What was learned

- **What removed the most was structure, not wording.** Rounds 1 to 6 took time and tokens down
  10 to 20% a round with clearer output, and 40% with a union file merged on the server, which
  took the conflict out of the work.
- **An agent beside every developer is mostly its own start.** At 67,000 tokens before it does
  anything, it doubles what a round costs to do what a squash and a 3,000-token call do.
- **Tests passing is not a commit's boundary.** Checkpoints that pass were half a function or a
  rename; a line from whoever did the work marks a step that makes sense. It costs the developer
  about 10% more tokens and 5 ms a note.
- **Ask for judgment only where it is needed.** An agent settled the clash of round 9 in one try at
  about 7,000 tokens above its start; a rule decides everything else.
- **A blind judge found what the scores could not.** Speed and tokens said C+D won from round 7;
  the judge said its commits were worse until round 10, and found a bug (the body written twice)
  that every test had passed.
- **Wording moves behaviour.** "What documents it" in one prompt brought README edits nobody asked
  for and two clashes.

## What is left

- One judge per pair, a model judging models, three small tasks in one package. Round 10 is one
  round: the tie should be tried again, with other tasks and other judges.
- The clash of round 9 was settled by an agent the harness started by hand; `nv publish` says it
  stopped and does not start one. Nothing measured the wait for it.
- Notes are only as good as the developer's sense of a step; nothing checks that a noted commit is
  whole, short of the conditions.
- The formal model in `formal/sync` does not cover the server's merge, nor a commit known for one's
  own though its contents differ: only tests do.
- Not done from the committers' findings: `nv git show <rev> -- <path>` and `nv git diff A B <path>`
  (how one checks what the server merged), `--format=fuller` and `%ad`/`%cn`/`%cd`.
