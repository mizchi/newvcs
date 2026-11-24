# What the eleventh round found

2026-11-21. Working through `TODO.md` as the eleventh round of review left it: 24 defects in
six groups, most severe first, plus two speed items and three about checkpoints. Each fix
began with a test or a Git-compat case that failed, as `AGENTS.md` asks.

## Result

| Group | Listed | Fixed | Left |
| --- | ---: | ---: | --- |
| Data loss | 4 | 3 | 1, seen once and not reproduced in 30 variants |
| Hang, reading outside the workspace | 1 | 1 | |
| Wrong history | 4 | 2 | 2, need a file system that folds case; none here |
| Logs and half-applied commands | 6 | 6 | |
| Differs from Git | 5 | 5 | |
| Smaller | 4 | 2 | 2, unconfirmed, not reproduced |
| **All** | **24** | **19** | **5** |

Two more defects were found on the way and fixed: one by a kill loop written for an item
marked as fixed but untested, one by a Git-compat case written for another item.

## What was fixed, and how

**A killed `nv snapshot` left a state nothing named.** Killed after the head moved and before
`.nv/oplog` was written, the snapshot existed and nothing listed it; the next
`restore` removed the file it held. Every place a state is made now logs it before anything
names it. A log record is one `write`, and a record torn by a kill is ended before the next
one starts, so a torn line names nothing instead of running into the next. The test kills a
snapshot 300 times at random moments and checks after each kill that the head is in the log.

**`nv git merge --abort` threw away an unstaged edit** to a file the merge did not touch. It
was `reset --hard`. It is Git's `reset --merge` now: the files the merge wrote, and what was
added since, go back; the rest keeps its edits.

**Finishing a conflicting sync with `--amend` amended another writer's commit**, and a later
sync undid a third writer's change. Refused, as Git refuses an amend in a merge that stopped.
(Later widened, in the model's work, to any commit the ref has.)

**`nv commit` waited for ever on a pipe** and read through a link to look for a sync's
markers. It read the file system's paths with `std::fs::read`. First it read only regular
files, opened with `O_NOFOLLOW | O_NONBLOCK`; later (in the tuning) the check was moved to
read what the commit recorded, from the store, or from the disk only where the index vouches
for the file.

**Beside Git, `git mv` and `nv commit --tracked` published half the move.** `--tracked`
looks at the files nv's index knows, and the new name was only in Git's. With `--tracked` the
paths Git has staged are looked at too, as `git commit -a` commits them; with `--recorded`
the user's staging is left alone, as tests from earlier rounds require. The one
`git diff --cached` this needs replaces the `--quiet` one publishing already ran.

**A killed `nv git commit --amend`** was listed as fixed but untested. The kill loop written for
it found a real defect: killed between moving the workspace to the amended commit and moving
the branch, the branch stayed on the commit that was replaced, and every later commit was
refused ("another workspace moved it"). The step that finishes a half-made move only went
forward; it now also finishes an amend, recognised as a commit this workspace logged after
the branch's, on the same parents.

**Git's locks.** With a stale `.git/HEAD.lock`, `nv sync` wrote the files and Git's index and
then failed at `update-ref`; with `.git/index.lock`, `nv commit` renamed its index over Git's.
`index.lock`, `HEAD.lock` and the branch's lock are now looked for before anything is written,
by `commit`, `sync`, `push` and the step that brings Git along. A Git commit stopped by
`NV_GIT_TIMEOUT` no longer leaves `next-index-<pid>.lock` (removed only when its process is
gone) or the lock on nv's own index.

**Smaller ones.** `nv record . a ../zz` noted `a` and refused `../zz`; it checks every path
first. A push refused over a sync's markers said "commit and push again", which never
works (the commit with markers stays in the chain); it now names
`nv git reset --soft HEAD~N`, and a test follows that advice to a successful push. A
restore that could not read a file it should have recorded said it was done; it exits 2 and
names the file. A change to `.nv/exclude` (or Git's `info/exclude`) was not seen by
`--recorded`: a hash of those files is noted at each look at everything and compared, one
small read, so a fork has the same note.

**Git differences** (case 33): `merge` refused over a local edit printed `Auto-merging` on
stdout and exited 1 (Git: nothing on stdout, exit 2, and a longer message), and the same for
a merge that would have gone through; a fast-forward over a staged file it does not touch was
refused (Git fast-forwards); one over a staged file it changes was "not supported" (Git
refuses with exit 1, or goes ahead when what is staged is what it brings); `reset <path>` in a
stopped merge left the path unmerged; `restore --staged` before the first commit exited 0
(Git: 128, "could not resolve HEAD"); `rm` of a tracked link was refused; "No commits yet"
where Git says "Initial commit"; `show` of a merge without Git's trailing blank line, and
`show --stat` of one without its stat; `reset --hard` before the first commit left empty
directories. Two lines of the case check that `switch` still refuses an edit, or an
untracked file, that is already what the other branch has, as Git does.

## Speed items

Publishing beside Git was listed at 5.7 times `git commit`, measured under load. On a quiet
machine it was 3.4 times, as the README said. Instrumenting the command found that looking
for a sync's markers walked the whole tree whatever the look: `--recorded` went from 143 to 90
ms on oxc. `nv git switch` was listed at 418 against 188 ms; measured here it was 1.2 times
Git, and became 1.1 times in the tuning.

## What is left

- A directory renamed only in case, and `git rm way` then `nv git add WAY`: both need a file
  system that folds case. This VM's kernel has no `CONFIG_UNICODE` (no casefold on ext4 or
  tmpfs) and no vfat or FUSE.
- A restore of a set-aside state, seen once after many catch-ups, amends and a killed restore;
  not reproduced in 30 variants.
- After a killed restore, files equal to HEAD listed once; snapshot ids differing in one run
  of eight; two temporary files left once. All unconfirmed.
- Checkpoints, which nobody in round eleven looked at, were not touched.

## What was learned

- A test written for an item marked "fixed but untested" found a defect. Items marked that way
  are worth the test.
- A reviewer's number measured under load is a hint where to look, not the size of the
  problem: 5.7 times was 3.4, and the 3.4 had a cause nobody had named.
- Where Git's behaviour was in question, running real Git first settled it, and once it said
  the opposite of what seemed right: Git refuses a switch over an edit that already is the
  other branch's version. A first version of that fix let it through, for every switch.
