# TODO

Defects found by the eleventh round of independent review and not fixed yet. Most severe
first within each group. Each one gets a failing test before its fix (AGENTS.md), and moves
to "What reviewers found" in the README when it is fixed. What was fixed is listed there, under
"What the eleventh round found and is now fixed".

## Data loss

- [ ] **(seen once) `nv restore <set-aside id>` taken long after the setting aside, after many
  catch-ups, two `git commit -a --amend` and a killed `nv restore pushed`, wrote that old state
  back**, and the next commit undid others' pushed values. In the simple case the same restore
  is refused ("from before this workspace last caught up"). Not reproduced in 30 variants.

## Wrong history (a file system that folds case: none was at hand on Linux, where the rest was fixed)

- [ ] **A directory renamed only in case is recorded beside the old spelling** on a file system
  that folds case: `mkdir d; echo 1 > d/f; nv commit . c1; mv d D; echo 3 > D/n; nv commit . c2`
  publishes `D/n` and `d/f`, which `nv checkout` then refuses. Same through `nv record . D/n`
  and the claude hook with `…/Src/n` for `src`. A tracked file `a` replaced by a directory
  reported as `A/x` keeps both. A new file `a` reported as `A` is published as `A` (Git records
  `a`). With a watcher the spelling depends on the order of events. Take each component's
  spelling from the index or the disk, as Git with `core.ignorecase` does.
- [ ] **`git rm way` then `nv git add WAY` (another case) stages nothing**, and the commit drops
  the file (no Git). The round-ten fix covers the same spelling only.

## Logs and half-applied commands

- [ ] Once two `.nv/.git-publishing.tmp*` were left, past "one at most of each kind" (a kill
  between writing a file whole and renaming it). Not reproduced.

## Differs from Git (output, exit codes)

- [ ] `nv git show --stat <merge>` prints nothing after the message; Git 2.43 prints the stat
  against the first parent. Check what newer Git does before matching it.
- [ ] A fast-forward `nv git merge` over a staged file that the move changes is refused as not
  spoken (125); find what Git says, and add a case.

## Smaller

- [ ] (unconfirmed) After a killed `nv restore`, `nv since base` and `nv git status` listed
  files that equal HEAD, once until the next sync. A killed restore run again left a file not in
  the target in one twin (3 times in a random campaign, 0 in 300 isolated tries). A push after
  an unheard push answered "0 commits taken" with exit 0 once.
- [ ] Snapshot ids for identical sequences differed in 1 run of 8, with no look at all.
- [ ] A change to `.nv/exclude` made before a workspace's first snapshot by this version is not
  seen by `--recorded` until something walks: nothing was noted to compare it with.

## Speed

- [ ] Publishing beside Git with `--tracked` (150 ms on oxc against `git commit -qam` 39 ms)
  looks at every tracked file twice: once for a sync's markers in `main.rs`, once to record in
  `colocated::commit`. Record first and look for markers in what was recorded.
- [ ] `nv git switch` between two commits 108 files apart: 418 ms against Git's 188 (measured
  under load; not measured again).
- [ ] A copy on a file system without clones (ext4) has every file read once after it is made
  (`roundtrip.rs` and `agent.rs` expect none, as on APFS). Linux has `FICLONE` on Btrfs and XFS;
  elsewhere a copy is a copy.

## The model (`formal/sync`)

- [ ] Bounds: two writers, one file of two lines, a ref of three commits. Grow it (three
  writers, two files, a directory set whole) as idealis's search allows.
- [ ] Not in it yet: a sync killed half-way and finished by the next (`arriving`), several
  refs, partial views, links, Git beside nv, a push of a chain longer than two.
- [ ] Drive the implementation from it (idealis's conformance driver): the counterexamples were
  turned into nv commands by hand.

## Checkpoints

- [ ] Have a fresh reviewer try checkpoints: none of round eleven saw them.
- [ ] The watcher does not start runs, only the hook does. A write landing after a runner's last
  look waits for the next write. Conditions run in the workspace, not a copy.
- [ ] Share checkpoints with other workspaces, and run one's own conditions on another writer's
  checkpoint merged in, to take it in early.
