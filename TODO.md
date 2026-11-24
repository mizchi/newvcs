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

Nothing known: case 33 covers what was found, and everything listed before is fixed.

## Smaller

- [ ] (unconfirmed) After a killed `nv restore`, `nv since base` and `nv git status` listed
  files that equal HEAD, once until the next sync. A killed restore run again left a file not in
  the target in one twin (3 times in a random campaign, 0 in 300 isolated tries). A push after
  an unheard push answered "0 commits taken" with exit 0 once.
- [ ] Snapshot ids for identical sequences differed in 1 run of 8, with no look at all.
- [ ] A change to `.nv/exclude` made before a workspace's first snapshot by this version is not
  seen by `--recorded` until something walks: nothing was noted to compare it with.

## Speed (oxc, 4-core Linux VM; see "Tuning on Linux" in the README)

- [ ] Publishing beside Git: `nv commit --recorded` 79 ms against `git commit -qam` 37 ms. Git's
  index is written twice (`git add` into nv's copy, then Git's commit, 17 ms each for 2 MB);
  `git commit --only <paths>` could do both in one, but its handling of hooks that stage files
  and of new files has to be gone through first.
- [ ] `nv git diff --stat` 19.7 ms against Git's 14.2: what is left is a stat of every tracked file
  and the paths of the index (17,002 allocations).
- [ ] `nv fork` of a workspace whose store is inside it (`nv git clone`) copies the store: 390 MB
  and 10 s for oxc on ext4. It could share the store, as workspaces beside one another do.
- [ ] Opening the store still reads `roots.map` after `roots.idx`, up to 1,024 records, and
  `git.map` likewise; `objects.idx` up to 512 objects.

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
