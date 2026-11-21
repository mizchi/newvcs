# Working on newvcs

newvcs (`nv`) is a version control system for many writers committing often,
agents above all, that stays exchangeable with Git. Git compatibility is the
boundary of the system. It is not what the system is for.

Read this before changing anything, and again before adding a dependency.
When a change is correct and compatible but breaks something in "What must
hold", the change is wrong.

## What the system is for

1. **Recording work costs what changed, not what exists.** A writer that
   knows what it wrote records it in milliseconds in a tree of any size.
2. **Nobody needs the whole tree.** A directory can be checked out, worked
   in and committed from without the rest being present, or even known.
3. **A working copy is nearly free.** Another checkout or a fork of a
   workspace is a clone of files already on disk, build output included.
4. **Writers do not queue behind each other.** Two pushes that touch
   different paths both land. The server rebases; the client does not retry.
5. **Looking never changes anything.** A command that reads leaves the
   store byte for byte as it found it, damaged or not.

The order is the priority. When two of these pull apart, the lower number
wins, and the README says what was given up.

## What must hold

Each line is a property with a number behind it. A change that moves a
number the wrong way by more than 5% is not done until it is explained in
the README or undone. "Tree" means the files of one commit.

### Cost

| Operation | Must scale with | Never with | Reference |
| --- | --- | --- | --- |
| `snapshot --recorded` | paths reported | tree size | 1.9 ms at 100,000 files |
| The hook after a tool wrote a file | that file | tree size | 4.8 ms on oxc, process start included |
| `restore`, `mark`, `since` with `--recorded` | paths that differ or were reported | tree size | 4–5 ms on oxc |
| The watcher | writes made | tree size, time spent idle | no CPU when idle; a write is a snapshot 40 ms later |
| `commit` (publishing snapshots), no Git beside it | paths changed | tree size, history | 59 ms for 52 snapshots |
| `commit` beside a Git repository | paths changed, and what `git add` and `git commit` of them cost | history | 74 ms on oxc with `--recorded`; Git's own commit is 30 ms |
| Following a move Git made on its own | commits and paths new to nv | history | 15 ms after one raw commit on oxc |
| `git commit`, the index file (no Git beside it) | paths staged | tree size | 7.1 ms on oxc; an untouched index is an empty file |
| `git add <path>` (no Git beside it) | what is at that path | tree size | 8.1 ms on oxc; Git takes 11 ms |
| Git's id of an object | nothing | how many objects have one | 4.7 ms for `rev-parse HEAD` on oxc |
| Checkout of a directory | files under it | tree size | 12.5 ms of 100,000 files |
| Checkout of a tree seen before, `fork` | one directory clone | file count, to first order | 263 ms / 200 ms on oxc |
| Opening the store | nothing | store size | 5 ms on a 206 MB segment |
| `status`, a snapshot with nothing reported | files walked | rules, attributes, store size | 40 ms on oxc on eight threads; Git takes 58 ms |
| `--tracked`, `git diff`, `git commit -a` | files the index knows | untracked files, directories | 15–19 ms on oxc; Git takes 17–30 ms |
| Server push | paths in the push | how far behind the pusher is, how many commits the push has | 3–5 ms |
| Writes to the bucket for a push | 1, and 1 for each large file | how many files the push has | 200 files: 1 write |
| Linking a directory of another repository | nothing stored but one change | files and bytes of the directory | 464 bytes for 14,716 files, 119 MB |
| Pushing a linked directory | its tree objects, and the files the server cannot find | bytes of files the repository it came from has | 833 ms for oxc, 1 content sent |
| Three-way merge of a file | its lines | | 4.7 ms for 58,000 lines |

`status` is the one place where the whole tree is walked, and only because
no writer reported what it wrote. It has to stay ahead of `git status` with
Git's defaults. Per tracked file it may stat and nothing more: no pattern
matching, no allocation that is not the path itself, no read unless the
time is in doubt. A command that does not need the whole tree does not walk
it: one that names a path looks at that path, one that needs only what is
tracked asks about what is tracked.

### Meaning

- **A commit is its parents and the paths it changed** against its first
  parent. Trees are derived from commits and cached. Nothing may require a
  tree in order to write a commit.
- **A change is to a file, or sets a whole directory.** Only a link sets a
  directory, and it stays one change only while the directory is exactly
  the one that was set. Nothing may turn a file-by-file change into a
  directory change on its own, and nothing that lists changes may assume
  they are files: open them with `manifest::expand`.
- **Ids are BLAKE3 of the object.** Git's SHA-1 ids are derived and kept in
  a map. Every imported commit must give back its Git id (271,812 commits
  across ten repositories so far); a commit that does not is a bug in nv,
  never an exception to record.
- **A tree is one object per directory**, and a directory's id says
  everything below it. That is what makes a view of part of the tree sound.
- **The store is append-only.** Opening it writes nothing. Only a command
  that appends may cut a torn end.
- **A snapshot is private and a commit is published.** Recording often must
  not make history noisy. A snapshot never reaches Git, a server or
  `nv git log`; to those it is an uncommitted change.
- **One writer at a time in a workspace, any number on a store.** Every
  command holds the workspace's lock while it runs; a watcher holds it per
  snapshot. Workspaces share a store and append to it at once: an append
  holds the segment, takes in what others appended since it last looked,
  and only then writes. Nothing that names an object (a root, a Git id, a
  ref) is written before the object is.
- **A process killed at any moment leaves what was, or what was meant.**
  Every file that says where something stands is replaced whole. Where two
  files have to change together, the order is the one that errs towards
  doing work again, never towards believing a file is something it is not:
  the head before the index where files moved with the head, the index
  first where they did not. An arrival from a server that was interrupted
  is finished from what was recorded, not from the half-written files.
- **Trusting reported writes is never a guess.** A command given
  `--recorded` waits for a running watcher to have reported every write
  made before it, starts one that should be running and is not (which
  looks at every file once), and looks itself at any file it is about to
  overwrite.
- **Going back loses nothing.** `restore` records what is on disk before it
  overwrites a file, and the state left behind keeps its id.
- **A workspace owns its files and shares the store.** Workspaces never
  share mutable state other than through the store and its refs.
- **A repository reads what it was given.** Contents are stored once for
  every repository, under names that say what they are; which of them a
  repository may read is its own table's to say. Knowing an id is not
  having the content.
- **No write waits on the whole.** A hash of every repository's state is
  derived when asked for. Nothing a writer does updates it.
- **Where Git shares the directory, Git is Git and nv follows.** `nv git`
  there runs Git. Nothing of nv's stops, wraps or stands in for a Git
  command: a hook that stops Git half-way through is worse than Git moving.
  The state in which the two last agreed is written down; a command that
  builds on where the workspace stands takes Git's moves in first, and
  nothing either side committed is dropped. A commit published there is
  made by Git, from the files, so that Git's rules about what goes in, who
  made it and whether it may be made are the only rules.
- **nv keeps out of what Git is in the middle of, and leaves the user's
  staging the user's.** No publishing during a merge or rebase that
  stopped. If no commit comes of publishing, Git's index is as it was
  found, byte for byte.
- **Going back never undoes someone else's work unsaid.** Undoing a sync
  is `before-sync`, which also takes back what the workspace believes of
  the ref. A state from before the last sync is not somewhere `restore`
  goes: its files on top of the new commits would be a revert nobody asked
  for.
- **A name is the file the file system says it is.** Where the file system
  does not tell two spellings apart (case, composed or decomposed
  characters), neither does nv, and neither does Git there. Removals are
  made before writes, so that a file is never written and then removed
  under its other name.
- **A push lands whole and may be repeated.** Its commits are checked
  together against everything the ref took since their base, and a push
  sent twice gets the answer it got. Writers that touched the same file do
  not overwrite each other: the later one is refused, and its sync merges.
- **What `nv git` does not speak, it refuses** (in a workspace with no Git
  beside it, where it answers itself). Exit 125 and "not supported", with
  the repository as it was. Never an approximation, and never a message
  dressed as one of Git's.
- **A command given wrongly does nothing.** An option it does not take, an
  argument too many, a store that is not one: an error that says which,
  and no file created.

## Borrowing from gitoxide

Where a rule is Git's and nothing more, use gitoxide's implementation
rather than writing a second one. So far: `gix-ignore`, `gix-glob`,
`gix-attributes`, `gix-merge`.

Borrow when all of these are true:

- The behaviour is defined by Git and nv adds nothing to it: a file format,
  a matching rule, a merge or diff algorithm, a way of printing something.
- It runs on what changed, on what is not tracked, or once per command. It
  does not run once per tracked file.
- It takes bytes and returns bytes or an answer. It does not own files,
  refs or objects.

Do not borrow, however convenient, anything that would decide how nv stores
or walks things. That means no `gix-index` (an index that lists every
file), no `gix-odb` or `gix-pack` as the store, no `gix-worktree` for
checkout (it would replace cloning with writing), no `gix-ref` for refs,
and not `gix` itself. Reading and writing a Git repository at the boundary
(`src/git.rs`) is a separate question and may use them.

Every adoption follows the same steps:

1. Measure the operations it touches with the code as it is. Keep a binary
   of that state to compare against.
2. If there is an implementation here already, run both on the same inputs
   and compare the output bytes before replacing it.
3. Adopt, then measure again against the kept binary, on the same files.
   Check that both walk the same files before believing a difference.
4. If a number in "Cost" moved, tune until it is back, or write down why
   it is worth it.
5. Record before and after in the README, with clean build time, stripped
   binary size and dependency count when a crate is added.

The first version of the `gix-ignore` change cost 4 ms on a 62 ms `status`,
none of it in gitoxide: it kept every directory entry in memory to look for
a `.gitignore`. The measurement found it. Do not skip the measurement
because the borrowed code is known to be good.

## How compatibility is checked

- `tools/git-compat.sh <nv>` runs every script in `tests/git-compat/cases`
  under real Git and under nv and compares output and exit code. Commit ids
  are part of the output.
- `tests/git-compat/reads` has Git build a history that nv then reads.
- `tools/git-compat-unsupported.txt` lists command lines that must be
  refused. Moving a line out of it means a case now covers that behaviour.
- A new option goes into `SPOKEN` in `src/gitcli.rs` together with a case
  that fails first. Capture what Git prints before writing the code.
- A difference someone finds becomes a line of a case before it is fixed.
  Cases 22 and 23 are a reviewer's findings.

## How it is checked by someone else

Before a release, and after any change to how commands behave, reviewers
who did not write the code are given the binary, the README and a use case,
and asked to find where it is slower than Git, where it is less safe, and
where it is wrong. They are told that a reproduction of a failure is worth
more than a confirmation. The first round found a lost update on the
server, a store corrupted by two workspaces, a workspace emptied by a
kill, and a hook that left Git half reset; none of the tests had. What
they find is fixed with a test that fails first, and they are asked again.

## Working rules

- Tests first: the case or test fails, then the code makes it pass.
- `pkf run ci` passes before work is called done.
- A number in the README was measured, on the machine and inputs it names.
  Change the code that produced it and the number is measured again or
  marked as older than the change.
- Limits go in the README under "Not done" or beside the feature, in plain
  words, when they are found.
- Benchmark data lives in a scratch directory outside the repository and is
  removed as soon as its numbers are written down.
- Documentation and commit messages are in English. Commit only when asked.
- The Worker in `server/` is not deployed by an agent.
