# newvcs

An experiment: a version-control store that keeps Git's object ids but not
Git's storage, shaped for many writers (agents) snapshotting continuously.

Git's wire protocol and object model are not going away, so whatever comes
next has to map onto them exactly. Its on-disk format and its working-copy
model are another matter. This repository tests how far those two can change
while every commit still comes back out with the id Git gave it.

Status: a prototype. The store and workspaces are local, one process at a
time. The server (`server/`) has only run on a local Durable Object and
bucket; it has not been deployed, and it has no authentication.

## What it does

```
nv import <git-repo> <store> [rev] [max]   import history, checking each commit maps back to its Git id
nv verify <store>                          recompute every Git commit id from the store alone
nv checkout <store> <rev> <dir> [prefix…]  a workspace of the given path prefixes (all, if none)
nv status <dir>
nv snapshot <dir> [--recorded]             record what is on disk as a commit nobody else sees
nv record <dir> <path>…                    say that these paths were written
nv commit <dir> <message>                  publish the snapshots since the last commit as one commit
nv log <dir>                               the workspace's operations
nv remote <dir> <url-of-repository> <ref>  point the workspace at a server
nv push <dir>                              send the commits the ref has not seen, then catch up
nv sync <dir>                              bring the workspace to the ref's tip, its own work on top
nv fork <dir> <new-dir>                    clone a workspace copy-on-write, build output included
nv export <dir> <git-repo> <ref>           write the workspace's commit into a Git repository
nv stats <store>
```

## The model

- **A commit is a list of changes** (`src/commit.rs`): its parents, the paths
  it changed against its first parent, and the bytes of the Git commit after
  the `tree` and `parent` lines, verbatim. Git names a commit by the hash of
  its whole tree, so nobody can commit without knowing every directory at
  least by hash. Here the tree is derived afterwards and cached.
- **A tree is one object per directory** (`src/manifest.rs`). A change
  rewrites the directories on its path. A directory's id says everything
  about what is below it, so a subtree can be checked out without the rest.
- **Files are stored whole, compressed as bounded delta chains**
  (`src/store.rs`): zstd with the previous version of the same path as a raw
  dictionary, at most 32 links before a full copy. Directories are stored
  the same way against the directory they replace.
- **Git ids are a derived map**, not the identity. Objects are named by
  BLAKE3. Git's id for each is recorded on import and computed on demand for
  anything made here.
- **The workspace's index is changed by appending** (`src/index.rs`): every
  entry sorted by path in a file that is searched in place, and a log of the
  entries set since. Nothing a command does to it is proportional to the
  workspace.
- **A workspace is a set of path prefixes** (`src/workspace.rs`), not the
  whole tree. Each blob is materialised once in the store and cloned
  copy-on-write (APFS `clonefile`) into every workspace that needs it.
- **Recording is not publishing.** A snapshot is a commit on top of the last
  one, taken after every step of work and seen by nobody else. A commit
  folds the snapshots since the last published commit into one. What changed
  is found from an index of sizes and times, or from the writer saying so,
  never by reading the tree.
- **Only the ref is agreed on** (`server/`). One Durable Object per
  repository serialises pushes. A push whose paths nobody else touched since
  its base is put on the tip by the server; one whose paths were touched is
  kept as a divergence of the ref. The writer does not retry either way.
- **Catching up fetches changes, not trees** (`src/remote.rs`). A commit is
  a few hundred bytes of changed paths; the tree is derived locally. File
  contents are fetched only for paths inside the workspace's view, so what
  others do elsewhere in the repository costs a narrow workspace nothing.

## What was measured

Every commit of four repositories, imported and written back:

| Repository | Commits | Git ids reproduced | Import | Verify from the store | `.git` packs | Store |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| helix-editor/helix | 7,373 | 7,373 | 2.3 s | 1.3 s | 85 MB | 23 MB |
| microsoft/playwright | 17,182 | 17,182 | 11.7 s | 8.4 s | 236 MB | 215 MB |
| moonbitlang/core | 4,318 | 4,318 | 1.4 s | 0.3 s | 35 MB | 15 MB |
| google/flatbuffers | 3,338 | 3,338 | 1.9 s | 0.8 s | 23 MB | 17 MB |

"Reproduced" means the Git tree built from the derived manifest and the Git
commit built from it have the SHA-1 the original has, for every commit,
merges included. `verify` does it again from the store with no Git repository
at hand. The `.git` sizes are the clones as they happened to be packed.

Where the bytes of the playwright store go: 132 MB in 7,108 files stored
whole (mostly binary assets), 38 MB in 70,697 file versions stored as deltas
(2.8 GB raw), 19 MB in 102,489 directories, 17 MB in commits. Storing
directories as deltas took them from 127 MB to 19 MB.

On the playwright store (3,205 files at HEAD), on an M5 laptop, next to Git
on a clone of the same repository. Best of 12 to 15 runs with `hyperfine`,
while the machine was busy with other work, so compare within a row:

| Operation | Git | nv |
| --- | ---: | ---: |
| A new working copy of everything | 415 ms (`worktree add`) | 378 ms (`checkout`), 41 ms (`fork`) |
| A new working copy of one directory (51 files) | 72 ms (worktree + cone sparse-checkout) | 41 ms |
| `status`, nothing changed | 25 ms | 15 ms |
| `status` after one edit | 26 ms | 15 ms |
| Record one edited file | 42 ms (`commit -am`) | 21 ms (`snapshot`), 3 ms (`snapshot --recorded`) |
| Disk for ten working copies | 728 MB | 18 MB (`checkout`), 12 MB (`fork`) |

A fork also carries untracked build output: forking a workspace of 6,659
files with `node_modules` took 118 ms where a plain copy took 1.9 s.
Publishing 52 snapshots as one commit took 59 ms, and the 52 snapshots of one
growing file added 0.1 MB to the store. Exporting a commit to Git (8 objects)
took 139 ms.

Every command opens the store. That was 58 ms of the 80 ms a snapshot first
took, spent reading the 206 MB segment to find its objects. With the objects
sorted by id in a memory-mapped index it is 5 ms.

The exported commit sits on top of the original HEAD in Git, `git diff`
shows exactly the two paths, and `git fsck` passes.

## Pushing at once

`server/test/e2e.ts`, against the Durable Object under `cf dev` on the same
laptop. Every writer commits on top of its own last accepted commit and never
fetches, as an agent in its own workspace would.

| Writers | Pushes | Accepted without a retry | Kept aside as conflicts | Pushes/s | p50 | p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 400 | 400 | 0 | 231 | 4.0 ms | 9.2 ms |
| 16, disjoint paths | 1,600 | 1,600 | 0 | 723 | 21.5 ms | 30.8 ms |
| 64, disjoint paths | 3,200 | 3,200 | 0 | 774 | 79.9 ms | 168 ms |
| 16, every 10th push on one shared file | 1,600 | 1,453 | 147 | 744 | 20.4 ms | 45.5 ms |

How the cost of one push grows (`server/test/bench.ts`, medians):

| | 1 | 10 | 100 | 1,000 | 10,000 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Commits the writer's base is behind the tip | 4.8 ms | 4.5 ms | 3.9 ms | 3.3 ms | 3.3 ms |
| Paths changed by the commit | 3.5 ms | 4.3 ms | 6.0 ms | 25.5 ms | 133 ms |

How far behind a writer is does not matter: the server keeps, for each path,
the position at which it last changed, and looks up the pushed paths. What a
push costs is its own paths, each a row written. Spreading 64 writers over
1, 8 or 64 repositories gave the same 800 pushes a second, because the local
runtime is one process; whether separate Durable Objects scale apart in
production has not been measured.

Through `nv` on the playwright store a push takes 13 to 27 ms: one request
carrying the commit and the contents it names, answered with what the ref
went through meanwhile. A sync with nothing to do takes 10 ms, and a
workspace's first contact with a ref 23 to 52 ms (it asks the server where it
stands instead of walking the history).

After each run the ref's history is one line (each commit's parent is the
commit before it), every commit's id is the hash of its bytes, and accepted
plus kept-aside equals pushed. These are local numbers: there is no network
and no replication in them. In production a Durable Object confirms each
write with three of five followers before answering.

`tests/remote.rs` runs three workspaces, each with its own store and its own
import of one Git history, against that server: one holding everything, one
holding a single directory, one arriving late. They push in turn and at
cross purposes, one of them is refused over a file both changed, and the
commit they end on is exported to Git and passes `git fsck`.

## Against Git, with writers in each other's way

Sixteen working copies of playwright, each committing to its own file and
pushing to one branch, ten times each. The Git writers do what Git asks of
them: push, and when rejected, `pull --rebase` and push again. The remote is
a local bare repository; the nv server is the local Durable Object.

| Writers | Git | nv |
| --- | ---: | ---: |
| 1 | 6.0 pushes/s, 1 rejected | 19.4 pushes/s |
| 4 | 5.9 pushes/s, 45 rejected | 50.0 pushes/s |
| 16 | 2.7 pushes/s, 919 rejected | 50.1 pushes/s |

All 160 commits land either way. With Git each of them was rejected nearly
six times on average first, and adding writers made the branch slower. With
nv no push was refused, because none touched another's paths.

## How far it goes

Larger repositories, with Git and Jujutsu 0.36 beside it (best of 8):

| | oxc (14,716 files, 20,876 commits) | lean4 (13,539 files, 41,201 commits) | synthetic (100,000 files) |
| --- | ---: | ---: | ---: |
| Import, Git ids reproduced | 20.5 s, all | 484 s, all (94.6 GB raw) | 1.7 s |
| Store / Git packs | 219 MB / 271 MB | 2.8 GB / 6.4 GB | |
| New working copy: Git / nv / nv fork | 989 / 1,273 / 200 ms | 3,038 / 1,147 / 156 ms | 4,808 / 7,845 / 1,032 ms |
| One directory: Git / nv | 144 / 100 ms | 111 / 73 ms | 117 / 12.5 ms |
| `status`: Git / jj / nv | 64 / 52 / 66 ms | 39 / 53 / 44 ms | 179 / – / 229 ms |
| Record an edit: Git / jj / nv | 41 / 158 / 70 ms | 37 / 149 / 49 ms | 113 / – / 236 ms |
| Record an edit the writer reported: nv | 6.9 ms | 7.1 ms | 1.9 ms |

What this says:

- **Git ids hold at every size tried**: 94,288 commits across six
  repositories, none different.
- **Checking out everything is not faster than Git**, and at 100,000 files
  it is slower: each file is written once into the store's shared copy and
  cloned from there. The first checkout of that tree took 20.7 s.
- **Looking over the whole tree is not faster than Git either.** `status`
  and an unreported snapshot stat every file; at 100,000 files Git is ahead,
  179 ms to 229.
- **What does not grow with the repository is where it wins**: a workspace
  of one directory (9x at 100,000 files), a fork (5x), a snapshot of
  reported writes, and pushing when others are pushing.
- **A snapshot of reported writes costs the same at any size**: 1.9 ms in a
  workspace of 100,000 files, where `git commit` takes 113 ms. The
  workspace's index is a sorted file searched in place plus a log of what
  changed since, so recording one file appends one record (`src/index.rs`).
  The two larger real repositories were measured before that change, when
  the index was rewritten whole.

Limits that are arithmetic rather than measurement, from Cloudflare's
published numbers: a push that changes one path writes five rows (the ref,
the history and its index, the commit, the path), which is about $5 per
million pushes at $1 per million rows; a Durable Object holds 10 GB, room
for something like twenty million commits at half a kilobyte each; and one
object is expected to serve about a thousand requests a second, which is the
ceiling for one repository whatever the number of writers.

## Why these choices

Measured on real histories before any of this was written (the tools are
`examples/chunk_probe.rs` and `examples/pack_probe.rs` in
[astscan](https://github.com/mizchi/astscan)):

- **Content-defined chunking loses to deltas on source code, five to one.**
  Over 2,000 playwright commits: chunks of about 1 KB store 22% of what
  whole files do, a delta against the previous version 4.4%. Cutting chunks
  at syntax-tree boundaries instead of by rolling hash made no difference
  (21.9% against 22.2%).
- **Cutting delta chains is nearly free.** Unbounded chains store 0.52% of
  the raw bytes, a full copy every 64 versions 0.62%, every 16 versions
  1.18%. A shared trained dictionary instead of deltas stores 14.9%.
- **Most commits cross directories.** In playwright 59% of commits touch
  more than one second-level directory (helix: 36%). So a checkout is a set
  of prefixes, not one subdirectory.

## Not done

- The server trusts anyone who can reach it. It checks that contents hash to
  their names and that a commit decodes, not who is writing.
- The server keeps commits in the Durable Object and contents in the bucket,
  one object per file version, uncompressed. The delta chains of the local
  store do not exist there, and nothing is ever collected.
- A workspace with several unpushed commits that has to catch up first gets
  them back as one commit.
- A conflict is recorded, not resolved. There is no merge.
- The server compares paths. Two changes to different functions of one file
  are a conflict to it.
- A ref has to be started from history its writers share. The server knows
  the ref's own commits and the one before the first; a workspace on
  something older cannot find its place.
- Segments are written in arrival order. Reading one old version may touch a
  chain scattered over the file; a compaction that puts a path's chain
  together is not written.
- A fork keeps build output, but whether a build tool still trusts it under
  a new absolute path has not been tried.
- No rename tracking, no ignore files (`.git`, `target` and `node_modules`
  are skipped by name), no locking between processes.
- Git objects that are not in canonical form (a tree with unsorted entries,
  say) would not round-trip; none turned up in 32,211 commits.

## Building and testing

```
cargo test
cargo build --release   # target/release/nv
pkf run ci              # fmt, clippy, tests
```

The end-to-end tests build a Git repository with `git`, so `git` has to be on
the path.

The server:

```
cd server && pnpm install
pnpm test                # the commit encoding, against an id pinned on the Rust side
pnpm typecheck
pnpm dev -- --port 8799  # then: NV_SERVER=http://localhost:8799 pnpm test:e2e
                         # and:  NV_SERVER=http://localhost:8799 cargo test --test remote
```

## License

MIT
