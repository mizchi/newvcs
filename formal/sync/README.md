# A model of pushing and syncing

`Sync.tla` is a model of several workspaces pushing to and syncing with one ref, written to
find the orders of events in which nv does the wrong thing. It follows the code, not a design
of it: the server is `server/src/repo.ts` `push`, a workspace is `src/remote.rs` `push`,
`sync` and `arrive_from`, and going back is `src/workspace.rs` (`restore pushed`,
`restore before-sync`) and `nv git commit --amend`. Where the code makes a choice (to tell its
own commits on the ref by what they say and change, to merge on a given tree), the model makes
the same one, so that a counterexample is a sequence of nv commands.

It is checked with [idealis](https://github.com/mizchi/idealis), a TLA+ checker in Rust.

## What is in it

- A ref, as the server keeps it: commits, and which push landed where. A push on the tip lands
  as it is; one on an older commit is checked, path by path, against everything the ref took
  since, and lands rebased or is refused; a push sent again gets the answer it got.
- Workspaces: what each published and has not pushed, its files, the position of the ref it
  last saw. They write lines, commit, push, sync, amend, set aside and undo a sync.
- A workspace can be killed while it waits for the answer to a push. What it sent may land all
  the same, before or after anything else.
- A file is two lines, merged line by line as Git's three-way merge does for lines far apart.
  A line can be written back to what it was: the same content, a new write. nv compares
  contents; the invariants follow writes.
- An arrival is worked out in four steps (`Begin`, `Sides`, `Commits`, `Files`), each from what
  the one before stored. Nothing else happens meanwhile: it reads only its own workspace and a
  part of the ref that does not change.

## What must hold

- `NoLostUpdate`: a line the ref changes is changed by someone who saw what it replaces.
- `OnlyCommitted`: nothing reaches the ref that was not committed.
- `CommittedWins`: a committed write wins over what its writer saw it replace. The ref does
  not end up with a line a commit replaced, unless that commit is still with its writer (in
  its files, in what it has not pushed, set aside under an id) or was replaced in turn by
  someone who saw it.
- `NoConflictWithSelf`: a sync never leaves a writer its own two versions of a line to choose
  between.
- `LandsOnce`: a push lands once.

The invariants were checked against bugs put into the model by hand: a server that does not
check for clashes, a sync that puts uncommitted changes into the commit, and a sync that keeps
its own side of a conflict without markers were each caught. A sync that does not recognise its
own commits on the ref was not: a second check in `arrive_from` (a change both sides have
alike counts once) gives the same files.

## What it found

Each was then reproduced with nv against the server, and is a test that failed first.

1. **Work after a push that landed unheard was merged on the wrong tree.** A workspace's push
   lands and it never hears so (killed, timed out). It changes the same line again, or back,
   and commits. Someone else changes the file elsewhere. At the next sync the workspace
   recognises its landed commit as its own, but merged what it did since on the old common
   point: a line changed back looked unchanged and was silently taken from the ref (the
   workspace's own landed version), and a line changed again was a conflict with itself. It is
   merged now on what its own landed commits left. (`landed-base`;
   `tests/remote.rs`, `work_after_a_push_that_landed_unheard_is_merged_on_what_that_push_left`.)
2. **Going back past a push that may have landed made its commits again.** After a push that
   landed unheard: `restore pushed` set aside, as "what the ref does not have", a commit the
   ref had; `restore before-sync` undid the sync whose result had been pushed; and amending
   made the pushed commit again under another id. In each, the next sync was a conflict between
   two versions of the workspace's own. A push now leaves a note from before it is sent until
   its answer is heard, or a sync finds its commits on the ref; while it is there those three
   are refused. (`unheard-guard`; `tests/lying_server.rs`,
   `after_a_push_that_landed_unheard_nothing_goes_back_past_it`.)
3. Found while reproducing the second, outside the model: **amending a commit that was pushed
   and heard** made the next push a conflict and the sync after it a conflict with oneself. A
   ref only goes forward; the amend is refused, as it is for another writer's commit.

## Checking the model

```
formal/sync/check.sh
```

runs every configuration twice: with the fixes nv has, where nothing may be violated, and with
`Fixed = {}`, where the counterexample the configuration was written for has to be found again:

| Configuration | What happens | With the fixes | Without them |
| --- | --- | --- | --- |
| `Landed` | two writers on one file; pushes lost and landing late; lines written back | 2,435,451 states, 7.2 min | `NoConflictWithSelf`, depth 19 |
| `Back-amend` | and amending; `b` writes another line | 69,118 states, 11 s | `NoConflictWithSelf`, depth 11 |
| `Back-aside` | and `restore pushed` | 96,877 states, 14 s | `NoConflictWithSelf`, depth 11 |
| `Back-before-sync` | and `restore before-sync` | 2,839,796 states, 7.7 min | `NoConflictWithSelf`, depth 19 |
| `SameKeys` | as `Landed`, every commit saying the same (one author, message and second) | 2,435,451 states, 6.6 min | `NoConflictWithSelf`, depth 19 |

With the fixes nothing is violated; the times are on a 4-core VM. Without them `Landed` also
violates `CommittedWins` (the line changed back and lost); the checker names the first
invariant it finds broken.

A counterexample prints as a story with `trace.py`:

```
IDEALIS_IR=2 idealis tlc -difftrace -config Landed.cfg MC.tla | python3 trace.py
```

These are bounded: two writers, one or two files of two lines, a ref of at most three commits,
two writes and two commits a writer. A bug that needs more than that is not ruled out.

### idealis

The configurations need more than idealis's command line gives: its search bounds are fixed
(100,000 states, 10 million units of work, counted in 32 bits), it lowers to its first IR
unless told otherwise, and it stops at an evaluation nesting of 128. `idealis-local.patch`
(against idealis `bcf018be`) reads the bounds and the IR from the environment, counts work in
64 bits and allows a nesting of 250:

```
git clone https://github.com/mizchi/idealis && cd idealis && git checkout bcf018be
git apply /path/to/newvcs/formal/sync/idealis-local.patch
cargo install --path crates/idealis-cli --bin idealis --locked
```

With the first IR the model does not compile (an expression nests deeper than 40), and with
the second and the nesting left at 128 the check stops with "evaluation nesting exceeds 128".
