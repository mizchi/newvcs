# Tuning on Linux

2026-11-22. oxc at `a96e2bdf` (17,002 files, 22,530 commits), on a 4-core Linux VM, ext4,
Git 2.43, release builds. Three workspaces of it: one beside Git (`git clone` and
`nv adopt`), one with no Git beside it (`nv git clone`), and a plain Git clone for Git's own
numbers.

## Result

Each row is the binary before the tuning and the one after, run back to back, medians of 20
runs, with Git measured in the same session:

| | before | after | Git |
| --- | ---: | ---: | ---: |
| `nv status` beside Git / `git status` | 36.3 ms | 34.9 ms | 52.1 ms |
| `nv git status` | 48.5 ms | 39.9 ms | 52.1 ms |
| `nv git diff --stat` | 29.2 ms | 19.7 ms | 14.2 ms |
| `nv git commit -qam` | 36.4 ms | 22.2 ms | 36.9 ms |
| `nv git switch` there and back, 52 files | 101.3 ms | 89.6 ms | 81.6 ms |
| `nv git switch` there and back, 206 files | 101.2 ms | 84.4 ms | |
| `nv git log -20 --oneline` | 7.7 ms | 3.1 ms | 2.5 ms |
| `nv snapshot --recorded` | 10.4 ms | 4.0 ms | |
| `nv snapshot` walking / `--tracked` | 46.7 / 29.1 ms | 37.1 / 18.9 ms | |
| `nv mark` / `nv since` with `--recorded` | 11.3 / 9.9 ms | 4.8 / 4.7 ms | |
| `nv commit` beside Git, `--recorded` | 81.1 ms | 78.8 ms | 36.9 ms (`commit -qam`) |
| `nv commit` beside Git, `--tracked` | 144.9 ms | 105.9 ms | |
| `nv commit` beside Git, walking | 172.9 ms | 130.0 ms | |
| first `nv status` after `nv fork` | 121 ms, 17,002 files read | 63 ms, none read | |

Before this table's "before", in the work on the eleventh round, the check for a sync's markers
was made to look only where the commit looks: `nv commit --recorded` beside Git went from 143
to 90 ms.

## How it was measured

- **Back to back.** The machine's speed drifted by 20% or more between runs half an hour
  apart (Git's own `switch` went from 69 to 95 ms). Comparing a run of the new binary with a
  run of the old one made earlier read as regressions everywhere. A script measured each
  operation with the old binary and then the new, in the same minute, on the same workspace.
- **CPU as well as wall time.** Wall time moved ±10% from run to run on the same binary; user
  plus system time moved less, and settled whether a 3 ms difference was real.
- **Nothing else running.** The local server (workerd) uses CPU when idle; it was stopped.
- **Instructions, not time, to find the cause.** `perf` could not be installed; valgrind
  could. Callgrind's instruction counts per function are the same from run to run, and showed
  where the time went where `strace` only slowed the command down (900 ms before its first Git
  process, against 125 ms untraced).

## What was found, and what was done

| Found by callgrind | Instructions | Done |
| --- | --- | --- |
| Opening the store hashed every commit's root into a `HashMap` with SipHash | 19.9 M of a `rev-parse`, 90% | Ids hash their first eight bytes (they are BLAKE3): 3.8 M. Then roots in a sorted, mapped `roots.idx`, as Git ids are in `git.idx` |
| `Index::entries` built a `BTreeMap` of all 17,002 paths to return them sorted | 36 M of 92 M for `diff --stat` | A merge of the sorted base and the sorted overlay: 6.8 M |
| The map of directories a look goes through used SipHash on paths | 10.7 M | A word-at-a-time hash: 7.2 M |
| Each file compared with the index allocated its full path | about 30% of what was left, in `malloc` | One path buffer per thread: `diff --stat` 58 M to 44 M |

`roots.idx` is rewritten only by a store that recorded roots itself and has 1,024 or more
outside the index, as `git.idx` is; a command that only reads writes nothing. The cost table
says opening the store must not grow with the store: before, a history of a million commits
would have made every command read a 65 MB `roots.map`.

Not found by the profiler:

- **The tree looked at twice.** `nv commit` looked for a sync's markers with a look of its own,
  then recorded with another. The markers are looked for now in what the commit recorded.
  The first version read those files from the store, which unpacks a delta chain, and cost
  `commit --recorded` 8% CPU: the benchmark caught it. The file is now read from the disk when
  the index vouches for it, from the store otherwise.
- **Copies on ext4.** Where there is no copy-on-write clone, a fork or checkout copied with
  `cp -R`, which gives every file a new modification time; the index that comes with the copy
  then vouched for nothing, and the first look read all 17,002 files. The copy keeps the times
  (`--preserve=mode,timestamps`) and shares blocks where it can (`--reflink=auto`, Btrfs and
  XFS). The three tests that expected that pass on Linux now.

## What was not done

- **Publishing beside Git is twice `git commit`.** Of `nv commit --recorded`'s 79 ms, Git's
  processes take about 60: `git add` 18 to 24 ms and Git's commit 35 to 50, each rewriting the
  2 MB index (`git update-index` of one path alone takes 17 ms here). `git commit -qam` writes it
  once. `git commit --only <paths>` could stage and commit in one, but how it treats a hook that
  stages files, and new files, is what the earlier review rounds broke most often beside Git;
  it was left for a change of its own.
- **`nv git diff --stat` is 1.4 times Git.** What is left is a stat of every tracked file and
  the 17,002 paths of the index.
- **A fork of a workspace whose store is inside it** (as `nv git clone` makes one) copies the
  store: 390 MB and about 10 s for oxc on ext4. Sharing the store, as workspaces beside one
  another do, would make it a copy of the files only.
- Opening the store still reads up to 1,024 roots after `roots.idx`, the Git ids after `git.idx`
  likewise, and up to 512 objects after `objects.idx`.
- The cost table in `AGENTS.md` still has the macOS numbers; only its row for publishing
  beside Git has a Linux number beside it. The README's "Tuning on Linux" has the rest.

## What was learned

- The 90% was in the one place nobody had looked because it "only opens the store", and it
  was the hasher, not the work.
- A benchmark without the old binary beside it in time misleads more than no benchmark.
- An optimization that moves work (reading from the store instead of the disk) can cost where
  it looked free; measured, it showed up the same day.
