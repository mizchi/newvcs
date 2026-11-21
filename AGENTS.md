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
| The hook after a tool wrote a file | that file | tree size | 5.6 ms on oxc, process start included |
| `restore`, `mark`, `since`, `commit` with `--recorded` | paths that differ or were reported | tree size | 5–6 ms on oxc; 39 ms for a commit with Git brought along |
| The watcher | writes made | tree size, time spent idle | no CPU when idle; a write is a snapshot 40 ms later |
| `commit` (publishing snapshots) | paths changed | tree size, history | 59 ms for 52 snapshots |
| `git commit`, the index file | paths staged | tree size | 8.3 ms on oxc; an untouched index is an empty file |
| Git's id of an object | nothing | how many objects have one | 5.9 ms for `rev-parse HEAD` on oxc |
| Bringing a Git repository in the same directory along | paths changed | tree size, history | 27 ms on top of the commit |
| Checkout of a directory | files under it | tree size | 12.5 ms of 100,000 files |
| Checkout of a tree seen before, `fork` | one directory clone | file count, to first order | 263 ms / 200 ms on oxc |
| Opening the store | nothing | store size | 5 ms on a 206 MB segment |
| `status` | files walked | rules, attributes, store size | 63.8 ms on oxc; Git takes 64 ms |
| Server push | paths in the push | how far behind the pusher is | 3–5 ms |
| Three-way merge of a file | its lines | | 4.7 ms for 58,000 lines |

`status` is the one place where the whole tree is walked, and only because
no writer reported what it wrote. It has to stay level with `git status`.
Per tracked file it may stat and nothing more: no pattern matching, no
allocation that is not the path itself, no read unless the time is in doubt.

### Meaning

- **A commit is its parents and the paths it changed** against its first
  parent. Trees are derived from commits and cached. Nothing may require a
  tree in order to write a commit.
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
- **One writer at a time in a workspace.** Every command holds the
  workspace's lock while it runs; a watcher holds it per snapshot. Nothing
  reads the workspace's state, changes it and writes it back outside it.
- **Trusting reported writes is never a guess.** A command given
  `--recorded` waits for a running watcher to have reported every write
  made before it, starts one that should be running and is not (which
  looks at every file once), and looks itself at any file it is about to
  overwrite.
- **Going back loses nothing.** `restore` records what is on disk before it
  overwrites a file, and the state left behind keeps its id.
- **A workspace owns its files and shares the store.** Workspaces never
  share mutable state other than through the store and its refs.
- **Where Git shares the directory, nv knows who moved.** The state in
  which the two last agreed is written down; a command that writes never
  builds on a head Git has left, and nothing either side committed is
  dropped to bring them back together.
- **What `nv git` does not speak, it refuses.** Exit 125 and "not
  supported", with the repository as it was. Never an approximation.

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
