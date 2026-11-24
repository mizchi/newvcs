# TODO

## From the twelfth round

Found by the twelfth round of independent review and not fixed yet (the rest of what it found is
fixed, and listed in the README under "The twelfth round"). The reviewers' reproductions were
scripts against the binary; each item gets a failing test or Git case before its fix.

### Differs from Git (`nv git`, no Git beside it)

- [ ] Rename pairing: Git pairs files with the same base name first, nv in order; and one pair
  Git scores below 50% nv takes as `R052`. Changes `status`, `diff`, `show`, `--name-only`.
- [ ] A rename that also changes mode: Git prints `old mode`/`new mode` before `similarity
  index`, nv after `rename to`.
- [ ] `--stat`: `d1/{s => }/b` where nv writes `d1/{s/b => b}`; a quoted name is not
  abbreviated by Git; a binary file whose mode alone changed is `Bin` without byte counts.
- [ ] Quoting the other way round: the commit summary's `rename f => été` (Git quotes), and
  "Unstaged changes after reset:" (Git does not).
- [ ] `ls-tree [-r] <rev> .` at the top prints an empty line or an empty name; `status` from
  inside an untracked directory prints an empty name where Git says `./`; `commit` with nothing
  to commit from a directory names paths from the top; `rm` of a path named twice says it twice;
  `show --pretty=format:` of a commit with no changes adds a newline; `show` of an annotated tag
  with a format drops the blank line after the tag message; `commit -a` before the first commit
  with an `AD` file says another thing; before the first commit `reset -q HEAD -- f` fails and
  `rev-parse 'HEAD^{tree}'` prints nothing.
- [ ] `mv` goes ahead where Git refuses: a directory holding an `AD` entry ("bad source").
- [ ] Refused where Git goes ahead (safe, but scripts break, and the README does not say): `rm`
  of an `AD` file, `rm --cached` of an `MD` file, `switch` over a tracked file deleted on disk or
  over an ignored untracked file. (`reset --hard` over an untracked file the commit has is
  refused on purpose: nv writes over nothing it does not track.)
- [ ] stderr wording: `bad revision` against "ambiguous argument", the "Use '--'" lines, `rm`'s
  and `mv`'s particular refusals, `show A:path` "exists on disk, but not in", and the refusal of
  an outside path, which names no command.
- [ ] `add` of a path in an ignored directory names the file where Git names the directory, and
  prints a newer Git's hint; `checkout <dir>` whose files are all staged removals exits 0.

### Slower than Git

- [ ] A history with one commit of 20,000 files: every full `nv git log` takes 33–42 ms against
  Git's 2.1–2.7 (`log -n 20` that stops short of it, 2.6). `ls-tree -r HEAD` 4.4 times Git's.
- [ ] On a generated tree of 20,000 files under other load, `nv git status` took 1.3–1.75 times
  `git status` with Git's defaults, where on oxc it is ahead. To be measured again on quiet
  hardware before the claim in "What must hold" is called held.

### Output

- [ ] A push the server merged (`merge=union`) does not warn that the commit's message was
  written before the commit it now sits on; a sync does.
- [ ] A command that finishes a killed sync says "nothing else came from the ref" while it
  writes files, and does not say that it folded commits.
- [ ] `nv pending` can show "checkpoint: unit passed (conditions not run on it)".
- [ ] A publish that stops over a file the developer removed and the other writer edited exits
  2, not 1, with advice a developer that never commits cannot follow.
- [ ] With a writer that never stops, `nv pending --wait` said "settled" while writes still came,
  and said `runner.log` would say why the conditions were not run; it did not.

### Server

- [ ] Ref names Git cannot hold together are taken: `a` beside `a/b`, `main` beside `Main`,
  `x.lock`, `y..z`, `HEAD`; `a/` and `a//b` are `a` and `a/b`.
- [ ] Symlinked `.gitmodules`, `.gitattributes` and `.gitignore` are taken; Git's fsck refuses the
  first, and Git will not check out the others.
- [ ] A ref's first commit may name a parent the server does not have (by design: a workspace on
  that parent finds itself in the history), and then nobody without that parent can sync it.
  Say so where a ref is started, or refuse where the parent is nobody's.

### Seen, not reproduced

- [ ] Through a proxy that dropped answers, a fork that synced cleanly then could not `nv git log`:
  "blob … is not in the store", gone after later syncs; and `nv pending` printed a Git id as
  `-------` once. Perhaps one cause: a commit in a shared store naming content it lacks.
- [ ] Two `.workspace.tmp*` files left in one workspace by kills, past "one at most of each kind".

## From the eleventh round

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
