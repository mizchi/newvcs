# A model of pushing and syncing

2026-11-22. Many writers pushing to and syncing with one ref were modelled in TLA+, checked
with [idealis](https://github.com/mizchi/idealis), and the counterexamples turned into nv
commands. The model and how to run it are in `formal/sync/`; this is how it went.

## Result

Three defects, each reproduced with nv against the local server, each fixed with a test that
failed first:

1. **Work after a push that landed unheard was merged on the wrong tree.** A line changed
   back after that push was silently taken from the ref (the push's own version), when
   another writer had changed the file elsewhere; a line changed again was a conflict with
   oneself. `arrive_from` recognised its own landed commits, then merged what it did since on
   the old common point. It merges on what those commits left. (19 steps for either form; the
   "changed again" one showed once `NoConflictWithSelf` was added.)
2. **Going back past a push that may have landed made its commits again**: `restore pushed`,
   `restore before-sync` and `--amend` each made the next sync a conflict between two versions
   of the workspace's own (11 to 20 steps). A push now leaves a note from before it is sent
   until an answer is heard or a sync finds its commits on the ref, and those are refused while
   it is there.
3. **Amending a commit that was pushed and heard** did the same, with no lost answer at all.
   Not from the model: found while reproducing the second. Refused.

With the fixes, five configurations are explored completely with nothing violated (69,118 to
2,839,796 states, 11 s to 7.7 min on 4 cores). With `Fixed = {}` each finds its counterexample
again; `formal/sync/check.sh` does both.

## How the model was built

**Follow the code, not the design.** The server is `repo.ts` `push` line by line: a repeat
gets the answer it got (`landed`), a push on the tip lands, one on an older commit is checked
path by path against everything since its base. The client is `remote.rs`: the chain since
the ref position it last saw, a push of all of it, `arrive_from`'s three-way merge of what was
published and what is on disk. Where the code makes a choice, the model makes the same one:
own commits on the ref are recognised by message and changes, not by id, because the server
rebases them. A counterexample is then a sequence of nv commands, and was each time.

**Kills.** A workspace can be killed while it waits for an answer; what it sent stays in the
network and may land later, before or after anything else. Every defect found involves one.

**What a line is.** Files are two lines, merged line by line (Git's three-way merge for lines
far apart). In the first version every write was a new value, and the checker found nothing:
845,857 states, the push and sync core as written held. Reading `arrive_from` against the model
then showed the merge base the first defect is about, which no run could reach, because with
values always new a line that looks unchanged never is. The decisive change was to make a line
a content and a write: nv compares contents, the invariants follow writes, and a writer can
write a line back to what it was. "Changed back" is ordinary for an agent; with it the checker
found the defect in 19 steps, and the others after it.

**Invariants.** Each write records the writes its writer saw it replace (closed transitively):
- `NoLostUpdate`: a line the ref changes was changed by someone who saw what it replaces;
- `OnlyCommitted`: nothing reaches the ref that was not committed;
- `CommittedWins`: the ref does not end up with a line a commit replaced, unless that commit
  is still with its writer or was replaced in turn by someone who saw it;
- `NoConflictWithSelf`: a sync never leaves a writer its own two versions to choose between;
- `LandsOnce`.

Two first versions were wrong and were caught by their counterexamples being nonsense: "saw
what it replaces" was not transitive (a writer editing a line twice "lost" its own first
edit), and "every committed write survives" flagged a writer whose two commits cancelled out.
Reading each counterexample before believing it was the step that mattered.

**Do the invariants see anything?** Bugs were put into the model by hand: a server that does
not check for clashes, a sync that commits uncommitted changes, a sync that keeps its own side
of a conflict without markers. Each was caught. A sync that does not recognise its own landed
commits was not, because a second check in `arrive_from` (a change both sides made alike counts
once) gives the same files: a redundancy in nv, not a hole in the model.

**Fixes inside the model.** To look past a defect, its fix was put in the model behind a
constant (`Fixed`), and the search run again. Two fixes were tried there before one went into
nv: matching an amended commit to the one it replaced (`amend-own`), and forgetting
`before-sync` once a push is sent (`push-forgets`). Both worked in the model; the note of an
unheard push (`unheard-guard`) covered all three cases with one rule and was what nv got.

## The checker

idealis checked this model, with four local changes (`formal/sync/idealis-local.patch`,
against `bcf018be`):

| Limit | Default | Why it mattered | Change |
| --- | --- | --- | --- |
| IR version | v1, which inlines | `Arrive` nests deeper than 40 | v2 by `IDEALIS_IR=2` |
| Evaluation nesting | 128 | v2 evaluates calls lazily, frame by frame | 250 |
| States, work | 100,000; 10 million | first configurations had 10^5 to 10^6 states | from the environment |
| Work counter | `u32` | runs ended "bounded (work)" after about 1.6 million states | `u64` |

v2 does not memoize: a definition used five times in an action is evaluated five times, and
`Arrive` chained a dozen of them. Splitting it into four steps that store what they worked out
in a variable (`ar`), with every other action waiting meanwhile, took a run from "bounded" to
complete in 76 s. It is sound because an arrival reads only its own workspace and a prefix of
the ref that cannot change.

Comparing a record with a tuple (`None` was `<<"none">>`) is an error, not `FALSE`, as in TLC;
a variable that is sometimes a record has to have a sentinel of the same kind.

## What is left

- The bounds: two writers, one file of two lines, a ref of three commits, two writes and two
  commits a writer. A defect needing a third writer or a longer chain is not ruled out.
- Not modelled: a sync killed half-way and finished by the next, several refs, partial views,
  links, Git beside nv.
- The counterexamples were turned into nv commands by hand. idealis has a conformance driver
  that could run the implementation against the model directly; not tried.
- A push that never reached the server now blocks `restore pushed`, `before-sync` and
  `--amend` until the next push is answered. The README says so.
