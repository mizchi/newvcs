# TODO

Defects found by the eleventh round of independent review and not fixed yet. Most severe
first within each group. Each one gets a failing test before its fix (AGENTS.md), and moves
to "What reviewers found" in the README when it is fixed.

## Data loss

- [ ] **A killed `nv snapshot` leaves a state nothing names, and the next restore removes a
  file's only copy.** Killed after it moved the workspace's head and before it wrote
  `.nv/oplog`. `nv snapshot` then says "nothing changed"; `nv restore . green` removes the
  new file with no "kept as"; `nv restore . <head id>` is refused as no recorded state.
  ```
  nv git init -q; echo base > a; nv mark . green; echo 'only copy' > new.txt
  nv snapshot . & sleep 0.003; kill -9 $!        # hit about 1 in 40
  nv snapshot .; nv restore . green              # new.txt gone
  ```
  Log before the head moves, or have restore record and name what it removes.
- [ ] **`nv git merge --abort` throws away an unstaged edit to a file the merge did not
  touch** (no Git). Git keeps it. No snapshot holds it.
  ```
  printf '1\n2\n3\n' > c; echo k > k; nv git add -A; nv git commit -qm base
  nv git switch -c side; (c = 1/S/3) commit -am; nv git switch main; (c = 1/M/3) commit -am
  echo 'my unsaved edit' > k; nv git merge side   # conflict
  nv git merge --abort                           # k is "k" again
  ```
- [ ] **Finishing a conflicting sync with `--amend` amends the other writer's commit**, and a
  later sync shows a conflict between two others' versions of a line; keeping "yours" undoes a
  third writer's pushed change. Refuse `--amend` after a conflicting sync, as Git refuses it in
  a stopped merge.
  ```
  B: S0=B1, S2=B1; commit; push
  A: S0=A1; commit; push (refused); sync (exit 1: HEAD is B1, A's change is markers)
  A: resolve, keep yours; nv git add -A; nv git commit --amend -m A1resolved
  C: sync; S2=C1; commit; push
  A: push (refused); sync (conflict on S2: B1 vs C1); keep yours; commit; push
  # the ref ends with S2=B1: C's change undone
  ```
- [ ] **(seen once) `nv restore <set-aside id>` taken long after the setting aside, after many
  catch-ups, two `git commit -a --amend` and a killed `nv restore pushed`, wrote that old state
  back**, and the next commit undid others' pushed values. In the simple case the same restore
  is refused ("from before this workspace last caught up"). Not reproduced in 30 variants.

## Hang, reading outside the workspace

- [ ] **`nv commit` opens what it publishes through `std::fs::read`
  (`workspace::with_sync_markers`)**: it waits for ever on a pipe, or a link to one, and reads
  a link's target outside the workspace. Read a link as a link; skip what is not a file.
  ```
  printf 1 > a; nv commit . c1; mkfifo ../outside/fifo; ln -s ../outside/fifo l; nv commit . c2   # hangs
  printf 2 > g; nv snapshot .; rm g; mkfifo g; nv commit . c2                                    # hangs
  ```

## Wrong history

- [ ] **A directory renamed only in case is recorded beside the old spelling** on a file system
  that folds case: `mkdir d; echo 1 > d/f; nv commit . c1; mv d D; echo 3 > D/n; nv commit . c2`
  publishes `D/n` and `d/f`, which `nv checkout` then refuses. Same through `nv record . D/n`
  and the claude hook with `…/Src/n` for `src`. A tracked file `a` replaced by a directory
  reported as `A/x` keeps both. A new file `a` reported as `A` is published as `A` (Git records
  `a`). With a watcher the spelling depends on the order of events. Take each component's
  spelling from the index or the disk, as Git with `core.ignorecase` does.
- [ ] **`git rm way` then `nv git add WAY` (another case) stages nothing**, and the commit drops
  the file (no Git). The round-ten fix covers the same spelling only.
- [ ] **Beside Git, `git mv a a2; nv commit <dir> mv --tracked` publishes `D a` only**, leaving
  `A a2` staged. Plain `nv commit` gives the rename. Unconfirmed for `git rm` plus a new file.
- [ ] **A killed `nv git commit --amend` could leave the amended commit on top of the one it
  replaced** (`amended c3 c2 c1` for `amended c2 c1`), or the workspace naming a commit not in
  the store. Fixed (one commit, flushed, then one workspace write) but with no test that kills
  it: write the kill loop.

## Logs and half-applied commands

- [ ] **A kill tears a line of `.nv/oplog`** (one record is several writes) and the next append
  runs into it: `1795288948570 snapshot 1795288948672 snapshot 3cbab4…`. The id on it is then
  not a name `restore` takes, and `~N` counts are off. Write a record in one `write`, and skip
  or repair a torn tail.
- [ ] **`nv record . a ../zz` exits 2 but has registered `a`.** Check every path before noting
  any.
- [ ] **After markers are committed with `nv git add -A; nv git commit`, push says "edit it,
  commit, and push again", which never works**: the commit with markers stays in the chain.
  Name what works (`nv git reset --soft HEAD~N`, or `nv restore <dir> pushed`), or offer it.
- [ ] **Beside Git, with a stale `.git/HEAD.lock`, `nv sync` writes the files and Git's index
  and then fails at `git update-ref`**, exit 2 with the files changed. With a stale
  `.git/index.lock`, `nv commit` writes Git's index where `git commit` refuses. Look for Git's
  locks before writing anything.
- [ ] **A stopped merge, `nv git reset <conflicted path>` does nothing** (rc 0, still `UU`);
  Git gives ` M`. `restore --staged` is right.
- [ ] **A commit beside Git stopped by `NV_GIT_TIMEOUT` leaves `.git/next-index-<pid>.lock`**,
  one per stop. Once two `.nv/.git-publishing.tmp*` were left, past "one at most of each kind".

## Differs from Git (output, exit codes)

- [ ] `nv git merge` refusing over an unstaged edit prints `Auto-merging` / `CONFLICT` on stdout
  and exits 1; Git says nothing on stdout and exits 2.
- [ ] A fast-forward `nv git merge` is refused over a staged file the other side does not touch
  ("would be overwritten"); Git fast-forwards.
- [ ] Before the first commit, `nv git restore --staged .` unstages everything with rc 0; Git
  exits 128 ("could not resolve 'HEAD'"); `restore --staged <path>` gives 1 where Git gives 128.
- [ ] `nv git rm <tracked link>` is refused (125) as "a symbolic link that stands for a
  directory" for any link; Git removes it.
- [ ] `commit` with nothing to commit on an unborn branch: Git says "Initial commit", nv "No
  commits yet". `show --name-status <merge>` lacks Git's trailing blank line. `reset --hard` on
  an unborn branch leaves an empty directory.

## Smaller

- [ ] A restore where a file that may not be read differs from the target exits 0 ("restored
  to X") and leaves the file; with `--recorded` there is no warning either.
- [ ] `.nv/exclude` changes are not seen by `--recorded`, and `.nv/exclude` cannot be reported
  with `nv record`.
- [ ] (unconfirmed) After a killed `nv restore`, `nv since base` and `nv git status` listed
  files that equal HEAD, once until the next sync. A killed restore run again left a file not in
  the target in one twin (3 times in a random campaign, 0 in 300 isolated tries). A push after
  an unheard push answered "0 commits taken" with exit 0 once.
- [ ] Snapshot ids for identical sequences differed in 1 run of 8, with no look at all.

## Speed (measured under load; re-measure on a quiet machine)

- [ ] Publishing beside Git: `nv commit --recorded` 248 ms against `git commit -qam` 43 ms on
  oxc (5.7×; the README says 3–4×). Walking `nv snapshot` 84–132 ms against 33–43 ms.
- [ ] `nv git switch` between two commits 108 files apart: 418 ms against Git's 188.

## Checkpoints

- [ ] Have a fresh reviewer try checkpoints: none of round eleven saw them.
- [ ] The watcher does not start runs, only the hook does. A write landing after a runner's last
  look waits for the next write. Conditions run in the workspace, not a copy.
- [ ] Share checkpoints with other workspaces, and run one's own conditions on another writer's
  checkpoint merged in, to take it in early.
