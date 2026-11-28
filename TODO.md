# TODO

## From the thirteenth round

Found by the thirteenth round of independent review and not fixed yet (the rest is fixed and
listed in the README under "The thirteenth round"). The reproductions are scripts against the
binary; each item gets a failing test or Git case before its fix.

### Differs from Git (`nv git`)

- [ ] A failed `merge` (exit 2 both sides) where a file was staged as new and then deleted: Git
  writes it back from the index, nv leaves it deleted.
- [ ] `pull --no-rebase` conflict markers end `>>>>>>> origin/main` where Git writes the fetched
  commit's id.
- [ ] A `pull` refused for a conflict that is not between lines has already fetched (Git fetches
  too, but the refusal should change nothing).
- [ ] `show dev` where `dev` is a tracked file and a branch: Git refuses it as ambiguous.
- [ ] A branch deleted and made again (`branch -D main; switch -c main`) still follows origin.
- [ ] Output: `switch -c`/`checkout -b` print `M`/`D` lines Git does not; `status` inside an
  untracked directory prints an empty name for `./`; `commit` with nothing to commit from a
  subdirectory names paths from the top and adds `git pull`'s hint on a diverged branch; `rm a a`
  says `rm 'a'` twice; `add` with no paths writes to stdout.
- [ ] Exit codes where several things are wrong at once: an empty message with nothing to
  commit, `--diff-filter=Z` with paths and no `--`, `rev-parse nosuch^{tree}`, `pull` printing
  "Updating" before its error, `merge --ff-only` of a diverged branch over edits, `add` of a
  staged file under a newly ignored directory.
- [ ] "Auto-merging" and "CONFLICT" lines C-quote paths Git prints as they are.
- [ ] A killed merge driver: `nv git merge` exits 128 where Git exits 2.
- [ ] Speed, on 5,000 files and 43 commits: a full `log` 16 times Git's, `ls-tree -r` 8 times, a
  diff of two revisions and `show --stat` 4 times, `fsck` 3.6 times, `switch` and `reset --hard`
  twice.

### Merge drivers

- [ ] Where Git defines a driver or an attribute that nv does not read: `include.path` in the
  user's configuration, `GIT_CONFIG_COUNT`/`KEY`/`VALUE`, `~/.config/git/attributes`, and beside
  Git, `.git/info/attributes`.
- [ ] Macros are read for `merge` only: `text`, `eol` and `binary` by way of a macro are not.
- [ ] A driver that replaces `%A` with a link is read through (as Git does).
- [ ] `merge_cst.py`: a unit one side moved to another section and the other changed (8 of 1,600
  random merges).

### Server

- [ ] On a ref whose first commit has no parent, the rules a union push states at its base are
  believed though the ref's history shows there were none.
- [ ] Ref names Git cannot hold (`a/`, `/a`, `a//b`, `.a`, `a..b`, `main.lock`, `-x`) are taken,
  and `x/` is `x`, `p//q` is `p/q`.
- [ ] Two union merges Git would make that the server refuses: a macro on a line that ends in
  CRLF, and `[attr]` defined again in a subdirectory.

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

- [ ] The names on a ref the server keeps for collisions (`live`) are folded as they were when
  each was written; the twelfth round's NTFS folding is newer. Before a server holding older refs
  is deployed with it, rebuild that table with the fold as it is.

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

## Found by the agent bench

- A sync whose own commit changed nothing but files left with both versions drops that commit,
  message and all: settled, the files are a commit of their own, under a message the writer has
  to give again (`a_sync_merges_by_the_driver_the_attributes_name`, Bob's "Bob hard").
