# newvcs

An experiment: a version-control store that keeps Git's object ids but not
Git's storage, shaped for many writers (agents) snapshotting continuously.

Git's wire protocol and object model are not going away, so whatever comes
next has to map onto them exactly. Its on-disk format and its working-copy
model are another matter. This repository tests how far those two can change
while every commit still comes back out with the id Git gave it.

Status: a prototype that has been tried hard and not yet used by anyone
but its author. Workspaces and stores are local; any number of processes
and workspaces may use one store at once. The server (`server/`) has only
run on a local Durable Object and bucket: it has not been deployed, and it
has no authentication. `## Not done` lists what is missing.

## What it does

```
nv adopt <dir> [--undo]                    make a Git repository's directory an nv workspace too
nv import <git-repo> <store> [rev] [max]   import history, checking each commit maps back to its Git id
nv verify <store>                          recompute every Git commit id from the store alone
nv checkout <store> <rev> <dir> [prefix…]  a workspace of the given path prefixes (all, if none)
nv fork <dir> <new-dir>                    clone a workspace copy-on-write, build output included

nv status <dir>
nv snapshot <dir> [--recorded | --tracked] record what is on disk as a commit nobody else sees
nv record <dir> <path>…                    say that these paths were written
nv hook claude                             the same, from a Claude Code hook
nv watch <dir> [--start | --stop | --status]  the same, from the operating system's file events
nv mark <dir> <name>                       record, and name the state
nv since <dir> <state>                     what differs from a recorded state
nv restore <dir> <state>                   go back to one, keeping what was on disk
nv commit <dir> <message>                  publish the snapshots since the last commit as one commit
nv log <dir>                               the workspace's operations

nv remote <dir> <url-of-repository> <ref>  point the workspace at a server: http://host/<owner>/<repo>
nv push <dir>                              send the commits the ref has not seen, then catch up
nv sync <dir>                              bring the workspace to the ref's tip, its own work merged on top
nv export <dir> <git-repo> <ref>           write the workspace's commit into a Git repository

nv name <store> /<owner>/<repo> <rev>      name a repository in the store
nv link <dir> <path> /<owner>/<repo> [dir] put a directory of another repository there, by its id
nv links <dir> [--update]                  how each link stands; follow the ones that moved
nv unlink <dir> <path>                     stop following one

nv check <dir>                             how the workspace stands with Git and with its watcher
nv git <command>                           Git's command line (see below)
nv help [command], nv --version
```

A state is a name given with `nv mark`, `base` (the last commit), `pushed`
(the ref's tip when the workspace last looked), `before-sync` (where the
workspace stood before its last `nv sync`), `~N` (N recorded states ago)
or the start of an id. An option a command does not take is an error,
not something ignored: `--recoded` for `--recorded` would otherwise quietly
do the slow thing.

## Speaking Git

There are two cases, and they are different on purpose.

**In a directory that is a Git repository, `nv git` is Git.** It runs the
real `git` with the arguments it was given: every command, Git's index,
configuration, hooks and remotes. Nothing is approximated because nothing
is reimplemented. See "Sharing a directory with Git".

**In a workspace with no Git repository** (made by `nv git init`,
`nv git clone` or `nv checkout`) there is no Git to run, and `nv git`
answers the everyday commands itself, on the nv store, printing what Git
would print and making commits with the ids Git would give them. The rest
of this section is about that. `tools/git-shim/bin/git` is the same under
the name `git`: put it first on `PATH` and a script written for Git runs
against nv.

What is spoken is one table in `src/gitcli.rs`, each command with the
options it takes (`nv git --spoken` prints it). An invocation is checked
against the table before any of it runs:

| Command | Options |
| --- | --- |
| `init` | `-q` `-b <branch>` |
| `clone`, `fetch`, `pull`, `push` | `-q`; `pull --ff-only`, `--no-rebase`. A Git repository on this machine only |
| `add` | `-A`, paths |
| `rm`, `mv` | `rm -r -q -f` |
| `restore` | `--staged`, paths |
| `reset` | `--soft` `--mixed` `--hard` `-q`, a revision or paths |
| `commit` | `-m` `-a` `-q` `--allow-empty` `--amend` `--no-edit` |
| `status` | the long form, `--porcelain`, `--short` |
| `log` | `--oneline` `--format=` `--pretty=format:` `--pretty=tformat:` `-n`, a revision; the default format. Placeholders: `%H %h %T %t %P %p %s %b %B %an %ae %at %cn %ce %ct %n %%` |
| `show` | the same formats, `--name-status` `--name-only` `--stat`, a revision, an annotated tag, or `<rev>:<path>` |
| `diff` | patches, `--name-status`, `--name-only`, `--stat`: two revisions, the working tree against the index, `--cached` |
| `rev-parse` | a revision, `<rev>^{tree}`, `<rev>:<path>`, `--short`, `--abbrev-ref HEAD`, `--show-toplevel` |
| `cat-file` | `-t` `-p` `-e` for commits, trees, blobs and tags, by revision or `<rev>:<path>` |
| `ls-tree` | `-r` `--name-only`, a revision or `<rev>:<path>`, one path (`dir/` lists what is in it) |
| `branch` | list, create, `--show-current`, `-d`, `-D` |
| `tag` | list, create, `-a` / `-m` for an annotated one, `-d` |
| `checkout`, `switch` | a branch (one only `origin` has is made, following it), `-b` / `-c` with a start point, `checkout [--] <paths>` |
| `merge` | `-q` `--no-ff` `--ff-only` `-m`, one branch; `--abort` |
| `config` | `user.name` and `user.email` |
| `fsck` | `--full` `--strict` `--no-dangling` |

Revisions are `HEAD`, a branch, a tag, `origin/<branch>`, a full id or the
start of one (four characters or more, if only one object's begins so),
with `^`, `~N` and `^{commit}`. `--version` and `help` are answered by the
real Git, if there is one.

The test is the design of [bit](https://github.com/bit-vcs/bit)'s harness,
turned around. bit puts a shim named `git` in front of Git's own test suite
and routes chosen commands to its implementation, the rest to the real Git.
nv keeps its data elsewhere, so nothing can be passed through; instead
`tools/git-compat.sh` runs scripts under both and compares what they print
and how they exit, with the same author, committer and clock. Since nv
reproduces Git's ids, the commit ids in that output have to agree too.

```
$ tools/git-compat.sh target/release/nv
ok       01-init-commit.sh (15 lines)
ok       02-status.sh (19 lines)
ok       03-add-paths.sh (18 lines)
ok       04-history.sh (46 lines)
ok       05-branches.sh (22 lines)
ok       06-modes-and-names.sh (30 lines)
ok       07-refusals.sh (12 lines)
ok       08-status-long.sh (81 lines)
ok       09-diff-and-show.sh (221 lines)
ok       10-rm-mv-restore-reset.sh (41 lines)
ok       11-tags-and-config.sh (23 lines)
ok       12-merge.sh (33 lines)
ok       13-amend.sh (18 lines)
ok       14-fsck.sh (2 lines)
ok       15-index.sh (144 lines)
ok       16-what-commands-say.sh (76 lines)
ok       17-merge-within-a-file.sh (50 lines)
ok       18-annotated-tags.sh (54 lines)
ok       19-ignore.sh (33 lines)
ok       20-line-endings.sh (32 lines)
ok       21-merge-conflict.sh (85 lines)
ok       22-everyday-odds.sh (71 lines)
ok       23-smaller-odds.sh (20 lines)
ok       24-from-below-and-by-path.sh (26 lines)
ok       reads/01-history-made-by-git.sh (150 lines)
ok       reads/02-work-on-both-sides.sh (57 lines)
ok       reads/03-tags-ignores-and-merges-made-by-git.sh (38 lines)
refused  79 commands nv does not speak
all compatible
```

Three things are checked:

- **What nv writes, Git would have written** (`tests/git-compat/cases`). The
  same script under Git and under the shim, each in a repository of its own.
- **What Git writes, nv reads** (`tests/git-compat/reads`). Git builds a
  history upstream, with things nv does not make itself: a merge within one
  file, a rename with edits, an octopus merge, a binary file, an annotated
  tag. A clone made and pulled by Git and one made and pulled by nv then
  answer the same questions. In the second case the clone also commits,
  pulls into a merge and pushes, and the real Git reads back what arrived
  and runs its own `fsck` over it.
- **What is not spoken is refused, never approximated**
  (`tools/git-compat-unsupported.txt`, seventy-nine command lines): commands that
  are not in the table, options that are not in a command's row, and
  arguments a command does not take. Each must exit non-zero saying "not
  supported", must be something Git itself accepts, and must leave the
  repository as it was.

`fsck` is nv's own check behind Git's name: every object is read and hashed
against its name, and from every ref each commit's parents, trees and files
must be there. Like Git's it prints nothing when nothing is wrong; a test
flips single bytes of the store and expects it to say so.

The index is real: `git add` takes the content as it is then, a commit
records that and nothing else, and `status` and `diff` tell the last commit,
the index and the working tree apart. It is kept as the paths where it
differs from `HEAD` (`.nv/git-staged`), so an untouched index is an empty
file whatever the size of the tree.

Opening a store never changes it. Bytes after the last whole record of the
segment (a record cut short by a crash, or damage that hides what follows)
stay where they are until a command has to append; `fsck` reports them.

Where Git's rules are only Git's, the code is gitoxide's rather than a
second writing of it: `.gitignore` is matched by `gix-ignore`, and
`.gitattributes` is parsed by `gix-attributes` and matched by `gix-glob`.
Each was measured before and after on oxc (14,716 files, 2,499
directories, 11 `.gitignore` files, 49 rules at the root):

| | before | after |
| --- | ---: | ---: |
| `status`, clean tree (40 ms since: see "Looking over the tree") | 62.4 ms | 63.8 ms |
| `status`, 10,000 untracked files and 10,000 ignored ones | 306 ms | 189 ms |
| first checkout | 1243 ms | 1215 ms |
| repeated checkout | 272 ms | 263 ms |
| files that differ from Git's checkout | 14 | 0 |

The first attempt cost 4 ms on the clean tree, none of it in matching: it
kept every directory entry to look for a `.gitignore` among them. Keeping
the names alone, and asking the rules about a directory that holds known
files only when a file that is not known turns up, left 1.4 ms. With
untracked files it is faster than before because ignored files are no
longer read.

The merge within a file is gitoxide's too (`gix-merge`). It replaced one
written here, after both were run on the same inputs and gave the same
bytes every time:

| three-way merge of one file | written here | `gix-merge` |
| --- | ---: | ---: |
| 2,319 lines, two changes a side | 0.25 ms | 0.34 ms |
| 2,319 lines, a change every 40 lines a side | 0.75 ms | 0.30 ms |
| 57,975 lines, two changes a side | 3.6 ms | 2.8 ms |
| 57,975 lines, a change every 40 lines a side | 151 ms | 4.7 ms |

It also writes Git's conflict markers, which the one here could not. So a
merge whose changes meet stops as Git's does: the file holds both sides'
lines, `status` shows it as `UU`, `git add` marks it settled, `git commit`
concludes the merge with both parents, and `git merge --abort` puts
everything back. The price is in the build: 19.1 s to 21.9 s from clean,
66 KB more in the stripped binary, and a dependency tree of 364 lines
where it was 168.

Cases 22 to 24 are there because reviewers who were asked to find
differences found them: an empty commit message, names Git refuses for a
branch or a tag, names with `/` missing from listings, paths told from the
wrong directory when a command is run below the top, a subject of several
lines, a path with a space in a patch, the message Git prepares for a
merge that stopped, a tree listed from below the top, an object named as
`<rev>:<path>`, removing the last file of the directory one stands in.
Each was a difference then and is compared now.

Where it is not Git even inside those commands:

- A file that became a link, or the reverse, shows as modified (`M`), not
  as a change of type (`T`), and its patch is one change, not a removal and
  an addition.
- On a file system that does not tell `File.txt` from `file.txt`, renaming
  one to the other shows as a removal and a new file.
- `show --stat` of a merge prints no statistics, and the words of some
  errors (`mv`, `rm`, `switch` during a merge) are not Git's.
- A directory with a Git repository of its own inside is left alone: Git
  would record it as a submodule.
- `status` during a merge with conflicts ends with a different last line;
  `commit --amend` of a commit that changes nothing is made, where Git says
  "No changes"; `init` says it made an nv repository, in `.nv/`; a few
  exit codes differ where a command fails (`switch` during a merge, a
  rejected `push`).
- `clone` of a repository of 20,876 commits and 578 tags takes 25 s where
  `git clone --local` takes 2: the history is converted, not copied. (It
  took 465 s when every tag asked Git for the whole history again.)

What is not spoken at all is a long list, and an ordinary day runs into
it: `diff HEAD`, `diff <path>`, `log -p`, `log -- <path>`, `stash`,
`rebase`, `cherry-pick`, `blame`, `switch -`, a detached `HEAD`. A
workspace with no Git beside it is for a writer that records and
publishes, and for scripts that stay within the table. Someone who wants
all of Git has it by keeping Git in the directory.

- A merge stops for someone only where lines of text met. Where one side
  deleted a file the other changed, both added one of the same name, or the
  file is binary, it refuses and touches nothing.
- `diff` while a merge has conflicts is refused: Git shows both sides
  against the result, which is not spoken.
- `.gitattributes` is read for line endings only (`text`, `text=auto`,
  `eol`, `-text`, `binary`), as Git does with `core.autocrlf` unset on a
  system whose line ending is LF. Filters, `ident` and
  `working-tree-encoding` are not applied. A file under `text=auto` that
  was committed with CRLF and is then edited is stored with LF, where Git
  would keep its CRLF.
- A file that was added and then became ignored is not looked at again.
- `fsck` does not report dangling objects.
- Commit dates are taken from `GIT_AUTHOR_DATE` / `GIT_COMMITTER_DATE` only
  as `<seconds> <+hhmm>`.
- Patches come from a plain shortest-edit diff. Where Git's heuristics pick
  among equally short ones, the lines shown can differ.

## For a writer that changes files all day

An agent knows which files it wrote, changes them hundreds of times between
commits, and most wants to get back to the last state that worked. The
commands for that are few, and most are not typed by anyone:

| When | What runs | Who runs it |
| --- | --- | --- |
| A task starts | `nv fork <workspace> <task>` | the agent |
| A tool wrote a file | `nv hook claude` (records that file, snapshots) | a hook |
| A shell command ran | `nv hook claude` (walks the tree, snapshots) | a hook |
| Anything wrote anything | `nv watch <dir> --start`, once (the system reports writes, the watcher snapshots) | a watcher, left running |
| The tests pass | `nv mark <dir> green` | the agent |
| What changed since they passed | `nv since <dir> green` | the agent |
| Something broke | `nv restore <dir> green`, or `~3`, or an id | the agent |
| Someone says commit | `nv commit <dir> "<message>"` | the agent, when told |
| Someone wants to look | `nv git status`, `diff`, `log` | a person |

`nv agent-init claude` prints the two `PostToolUse` entries for
`.claude/settings.json`. The hook reads the event on stdin, finds the
workspace the file is in, and never fails the tool: outside a workspace, or
handed something it cannot read, it does nothing.

A snapshot is private. It is not in `nv git log`, and where a Git
repository shares the directory it never reaches Git: to Git, the work
recorded since the last commit is uncommitted changes, which is what it is
to anyone but the workspace. `nv commit` publishes it as one commit. Beside
a Git repository that commit is made by Git, from the files.

`restore` records what is on disk before it overwrites anything, and says
under which id, so going back can itself be gone back on. It touches the
files that differ between the two states and no others, and it does not
publish: the next commit holds whatever lies between the last commit and
the files. Build output and anything else nv does not track stays put.

A `<dir>` may be the top of a workspace or any directory inside it, and
paths given to `nv record` are told from that directory. Only paths inside
the workspace are taken: not `../x`, not an absolute path elsewhere, not a
path through a directory that is a symbolic link.

On a file system that does not tell `A.txt` from `a.txt`, or a letter with
an accent from the letter followed by the accent (macOS, by default), a
file found under one spelling is the file known under the other, as it is
to Git there. A new file's name is recorded with its characters composed,
which is how Git records it.

`nv status` lists what no snapshot has yet. It also says, without listing,
how many paths are recorded and not yet published (`nv since <dir> base`
lists those), and whether Git has moved since nv last looked.

There are three ways to look for what changed, and a command says which:

- **Nothing said**: the tree is walked, on several threads. Everything is
  found, new files included.
- **`--tracked`**: only the files already known are looked at, as
  `git commit -a` does. New files are not found.
- **`--recorded`**: only the paths that were reported (`nv record`, the
  hook, the watcher). The cost is those paths, in a tree of any size.

On oxc (14,716 files), in a directory shared with Git, medians of 12 to 15
runs with `hyperfine`:

| | walking the tree | `--tracked` | writes reported (`--recorded`) |
| --- | ---: | ---: | ---: |
| Hook after a file edit | | | 4.8 ms |
| `nv record` and `nv snapshot`, two processes | 38 ms | 15.5 ms | 7.0 ms |
| `nv mark` | | | 4.4 ms |
| `nv since` | | | 4.1 ms |
| `nv restore` (two files differ) | | | 5.3 ms |
| `nv commit`, the commit made by Git | 108 ms | 81 ms | 74 ms |
| `git commit -a`, for scale | | 30 ms | |

Recording is what happens hundreds of times, and it is four to six times
quicker than Git's quickest commit. Publishing happens once a task and is
slower than `git commit`, by design: it is `git add` of the changed paths,
`git commit` of exactly those, and nv taking the new commit in. When nv
made the commit itself and handed Git the objects this row read 39 ms, and
the commit ignored the user's identity, excludes, filters and hooks.

With `--recorded`, `restore` also looks at each file it is about to
overwrite, so an unreported change to one of those is still kept. An
unreported change to a file the two states agree on stays on disk as it
is, and is in no snapshot until something reports it or walks the tree:
that is the bargain of `--recorded`, and the reason for the watcher.

### A hook, a watcher, or both

Both were run against a real Claude Code session (`claude -p`, asked to use
Edit, Write and one shell command that wrote two files), and both recorded
every write: a walk of the tree afterwards found nothing left over.

| | hook (`nv hook claude`) | watcher (`nv watch`) |
| --- | --- | --- |
| Needs | two entries in `.claude/settings.json` | a process left running per workspace |
| Sees | what the agent's tools did | every write, whoever made it: an editor, a formatter, a build, a person |
| A tool that names its file | 5.6 ms, one file looked at | recorded about 40 ms after the write |
| A shell command | 65 ms on oxc: the tree is walked, because nothing says what was written | the same 40 ms, and no walk |
| Snapshots | one per tool call | one per burst of writes (25 ms of quiet ends a burst) |
| When it is not there | nothing is recorded, silently | started again by the next hook or `--recorded` command, and looks at every file once |

So the hook is exact and immediate for the agent's own tools and pays a
walk for every shell command; the watcher pays nothing per tool and makes
`--recorded` true for everything, at the price of a process and a short
lag. They compose: with both, the hook records an edit at once and never
needs the walk.

The lag would be a hole: a command that trusts reported writes, run in the
40 ms after a write, would not see it. So such a command first asks a
running watcher whether it has caught up (a file in `.nv` that the watcher
removes once every event before it has been recorded) and waits for the
answer. With a watcher on oxc, a shell write followed at once by
`nv mark --recorded` takes 48 ms (measured before publishing went through
Git; the wait for the watcher is the same).

The watcher under load, on oxc:

- Idle, it uses no CPU.
- 208 tracked files appended to by one loop: one snapshot, nothing left for
  a walk.
- 5,000 files written into an ignored directory: no snapshot, 0.12 s of CPU
  to find out that none of them matter.
- A directory of 112 files renamed: one snapshot holding the 112 removals
  and 112 additions. The system reports the rename as two paths and says
  nothing of the files, so the watcher lists them itself.
- Memory: 83 MB resident, most of it the store mapped for each snapshot.

A hook, a watcher and a person's command can arrive at once, and each
reads the workspace's state, changes it and writes it back. There was
nothing to stop two of them from doing so together. There is now one lock
per workspace (`.nv/lock`), held by every command for as long as it runs
and by the watcher for each snapshot.

A watcher that has stopped would be the worst kind of failure here: nothing
recorded, nothing said, and every `--recorded` command trusting reports
that are not being made. So a workspace remembers that it is meant to be
watched (`nv watch <dir> --start` says so, `--stop` unsays it), and whether
a watcher lives is a lock the watcher holds, not a process id that could
have passed to another process. A hook, and any command with `--recorded`,
starts a missing watcher before doing anything else; a watcher starting
looks at every file once, so what was written while nobody watched is
found. `nv check` says how the workspace is watched and exits non-zero
when it should be and is not.

On oxc: `nv watch --start` returns in 71 ms, most of it that one look;
`--status` takes 4.7 ms; a hook with a watcher running, 6.7 ms; and
`nv since --recorded` right after the watcher was killed, 114 ms, with the
file written while it was down in the answer.

The watcher uses the `notify` crate: 26.8 s to build from clean where it
was 21.9 s, 218 KB more in the stripped binary.

Not done: nothing searches the snapshots for the one that broke a test,
`~N` counts states in the workspace's own log (not across forks), a
watcher does not come back after a restart of the machine until something
asks for it, and it has only been run on macOS.

## Sharing a directory with Git

`nv adopt <dir>` makes a Git repository's directory an nv workspace as
well: the history of the commit Git is on is imported, the files are left
alone, and one line (`.nv/`) is added to `.git/info/exclude`. Nothing else
of Git's is touched, and `nv adopt <dir> --undo` takes nv out again.

There Git is Git:

- **`nv git <anything>` runs the real `git`.** Stash, rebase, worktrees,
  remotes, `-c`, `--no-pager`: whatever Git speaks. One index, Git's. The
  shell function from `nv shell-init` therefore changes nothing here; it
  matters in workspaces that have no Git.
- **Raw `git` is free to do anything**, with or without nv knowing. There
  are no hooks. An earlier version installed two that stopped Git from
  moving a branch, and a reviewer showed what that does: the hook that
  runs when a ref changes runs after `git reset --hard` has rewritten the
  files, so stopping it left the files reset and the branch not. Letting
  Git move and following it loses nothing; stopping it half-way does.
- **nv follows.** It writes down where the two last agreed
  (`.nv/git-synced`). A command of nv's that builds on where the workspace
  stands (`commit`, `restore`, `mark`, `since`, `push`, `sync`, `links`)
  first compares that with Git's `HEAD`, which is two small files read,
  and if Git has moved takes the new commits in with their ids. On oxc,
  after a raw `git commit`, that costs 15 ms once. `nv check <dir>` says
  how the two stand without changing anything.
- **`nv commit` is a commit by Git.** The paths that differ between the
  last commit and the recorded state are staged with `git add` and
  committed with `git commit`, from the files. So what
  `.git/info/exclude`, `core.excludesFile`, a clean filter or
  `core.autocrlf` keeps out or changes is kept out or changed; the author
  is Git's; a `pre-commit` hook runs, may change what is committed, and
  its refusal is a refusal. nv's own look at the files reads the same
  exclude files and `core.autocrlf`, and asks Git which files are as
  committed where a filter is configured.
- **What the user staged is the user's.** If the user has staged nothing,
  the commit is the plain one of the whole index. If they have, it is a
  commit of nv's paths only (`--only`), and their staging is left. If no
  commit comes of it (a hook said no, Git would not add a path), Git's
  index is put back byte for byte as it was found. A removal or a rename
  made with `git rm` or `git mv` is published like any other change.
- **nv keeps out of what Git is in the middle of.** During a merge, a
  rebase, a cherry-pick or a revert that stopped, `nv commit` refuses
  before touching anything, and says to conclude or abort it with Git. A
  commit made around a stopped merge would lose what the index says is
  settled; a reviewer showed the merge then concluding without the other
  side's files. Recording (`snapshot`, `mark`, `restore`) goes on working.
- **What nv recorded survives whatever Git does.** After
  `git reset --hard`, `nv restore <dir> '~0'` brings back the state
  recorded before it; a mark made before a rebase still names its state.

The one way nv moves first is `nv sync`, which brings commits from a
server; Git's branch, and its index for the paths that changed, are then
brought along in the same command.

Killing `nv commit` at a random moment a hundred times, in a test, leaves
Git's history and `fsck` clean every time, and the next `nv commit`
publishes what was left. The content is staged before it is committed for
that reason: Git's partial commit brings the real index up to date last,
and a process killed before that would leave the index behind the commit.
What a kill between the two does leave is the changed paths staged and not
committed, which is what `git add` followed by nothing leaves.

On oxc: `nv adopt` takes 20 s, nearly all of it importing 20,876 commits,
and adds a 252 MB store beside a 283 MB `.git`. That is the price of
admission and it is paid once; `--undo` takes 10 ms.

Not done here: a Git worktree or submodule (`.git` as a file) is not
recognised. A file marked `skip-worktree` that was edited stops
`nv commit` (Git will not add it), and one marked `assume-unchanged` is
committed where `git commit -a` would pass over it. `nv status` does not
take Git's moves in, because looking changes nothing; it says when Git
has moved, and what it then lists is against the commit nv knows. Only the history behind the commit Git is on is imported, and
more when Git goes elsewhere. A file stored through a filter is known to
be as committed only until it is touched; after that nv, which compares
bytes, calls it changed where Git would run the filter and call it the
same. Includes in Git's configuration are not followed.

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
- **A workspace of everything is cloned whole.** Cloning files one at a
  time takes turns inside the kernel, about 75 microseconds each, on any
  number of threads; cloning a directory is one call. So the store keeps
  the two most recent whole trees it was asked for, and a full checkout is
  a clone of one, or of the nearer one with the difference applied.
- **Recording is not publishing.** A snapshot is a commit on top of the last
  one, taken after every step of work and seen by nobody else. A commit
  folds the snapshots since the last published commit into one. What changed
  is found from an index of sizes and times, or from the writer saying so,
  never by reading the tree.
- **Only the ref is agreed on** (`server/`). One Durable Object per
  repository serialises pushes. A push is a chain of commits that lands
  whole or not at all. If nobody else touched its paths since its base, the
  server puts it on the tip; if somebody did, it is refused, and the writer
  syncs, which merges the two as text, and pushes again.
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
| A new working copy of everything | 371 ms (`worktree add`) | 289 ms the first time, 37 ms after (`checkout`), 41 ms (`fork`) |
| A new working copy 200 commits back | 335 ms | 205 ms the first time, 65 ms after |
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

## Looking over the tree

`status`, a snapshot nobody reported writes for, and `nv git status` all
have to find out what changed with nothing to go on. That used to be one
thread listing every directory and asking about every file: 62 ms on oxc,
the same as Git, which was the stated goal ("level with `git status`").

Timing the two halves showed where it went: 39 ms listing 2,499
directories, 22 ms asking about 14,716 files. Both are now done on up to
eight threads. The top two levels of the tree are listed first and the
directories found there are walked at the same time; the files to compare
are then compared at the same time. Commands that have no use for the
whole tree no longer look at it: `git add <path>` looks at that path,
`git diff` and `git commit -a` at the files Git tracks.

On oxc, a workspace with no Git beside it against a Git clone of the same
commit, medians of 15 to 20 runs:

| | before | now | Git |
| --- | ---: | ---: | ---: |
| `status` | 62.3 ms | 40.4 ms | 58.3 ms |
| `nv snapshot`, one file changed, nothing reported | 66.1 ms | 42.2 ms | |
| `git add <one file>` | 68.6 ms | 8.1 ms | 11.2 ms |
| `git commit -a`, one file changed | 67.5 ms | 18.9 ms | 30.3 ms |
| `git diff`, one file changed | 66.3 ms | 18.1 ms | 16.6 ms |
| `git commit`, one file staged | 8.3 ms | 7.1 ms | 29.5 ms |

Both builds were checked to look at the same 14,716 files. The threads cost
processor time: 205 ms of it for the 40 ms `status`, where one thread used
62. Git's `status` here is with its defaults; with its untracked cache
turned on a reviewer measured it at 21.5 ms, which nv does not match and
has no equivalent of. `git diff` is level, not ahead.

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

What a push and a sync promise, each with a test in `tests/remote.rs` or
`server/test/e2e.ts`, and each there because a reviewer broke the version
before:

- **A push of several commits is one request and lands whole or not at
  all.** Its paths, all of them together, are checked against everything
  the ref took since the chain's base. Sent a commit at a time, the second
  commit's parent was by then the tip, nothing was checked, and it wrote
  over a change it had never seen.
- **A push that is sent again gets the answer it got.** The server keeps,
  for the last commit of each push that landed, where it landed. A writer
  that was killed before it heard back repeats the push and goes on.
- **A ref started by a workspace holds every commit made since the
  workspace was**, not only the newest; the workspace remembers the commit
  it was made at.
- **A push that is refused has a way out, and it is `nv sync`.** Files both
  sides changed are merged as text, by the code `git merge` uses
  (`gix-merge`). Where the two changed different lines the result is the
  workspace's change on top of theirs, and the push then goes through.
  Where they changed the same lines the file is left holding both versions
  between Git's markers, as a change not yet recorded; the sync exits 1,
  as `git pull` does, and `nv commit` refuses while a file from that sync
  still holds the markers. What cannot be merged as text (a file one side
  removed, a binary file) stops the sync with nothing touched.
- **A sync can be undone, and undone means undone.**
  `nv restore <dir> before-sync` puts back the files as they were, a file
  that was never recorded among them, and what the workspace believed of
  the ref, so that a push is refused again as it was. Going back to a
  state from before the last sync any other way is refused: the first
  version printed that state's id and invited a plain `restore` to it,
  which put the old files on top of the new commits, and the next commit
  and push then removed the other writer's work for everyone without a
  word. A reviewer followed the invitation and found that.
- **A sync that is killed is finished by the next one.** Before it writes
  the first file it notes which tip it is arriving at (`.nv/arriving`);
  whoever finds the note completes that arrival from what is recorded,
  rather than taking half-written files for the workspace's own work. Forty
  pushes killed at a random moment each, with another writer moving the
  ref in between, all recover by `nv push || (nv sync && nv push)`.
- **`nv export` after a sync fetches what it lacks.** A sync brings the
  contents of the tip, not of every commit on the way; writing that
  history into Git needs Git's id of each version, so the missing ones are
  fetched then.

The server still compares paths, not lines: two writers who change
different functions of one file do not both land without a retry. The
second is refused, syncs (which merges) and pushes again. That is one
round trip more than Git's `pull --rebase`, and it is the writer's client
that does the merging, where the files are.

## One namespace, and directories shared across it

Every repository in a store has a name, `/<owner>/<repo>`, which is a ref
to its latest commit (`nv name`, `nv names`). A directory's id says
everything below it, so a directory of one repository can stand in another
by being there with the same id:

```
$ nv link work vendor/lib /mizchi/lib src
$ cat work/nv.links
vendor/lib	/mizchi/lib	9f3c…	src
```

- Nothing is stored twice, and the commit does not list the files. A link
  is one change, "this path is that directory", so linking all of oxc
  (14,716 files, 119 MB) into another repository added 464 bytes to the
  store, and publishing it 530 more. When the change was the list of
  14,716 paths it was 601 KB.
- To Git it is an ordinary directory, not a submodule: after
  `nv export`, `git rev-parse main:vendor/lib` in the one repository is
  `git rev-parse main:src` in the other, and `git fsck` passes.
- Where it came from is not in the tree but in `nv.links`, a tracked file,
  as a lock file names versions. `nv links` says for each whether it is as
  linked and whether its repository has moved on; `nv links --update`
  follows, writing only the files that differ, and refuses where the
  directory was changed here.

`nv names` also prints one hash over every name and its commit. It is
derived from the names and stored nowhere: a hash of everything that a
write had to update would put every writer to every repository in one
queue, which is the thing the per-repository ref exists to avoid.

A change that sets a whole directory is the one kind of change that is not
a file. Only a link makes one. Publishing keeps it as one change for as
long as the directory is still exactly the one that was linked; once a
file below it is changed, the commit that does so lists that file, as any
other would. Whoever has to touch files (a checkout, `nv sync`, `nv links
--update`) opens the directory against what stood there and gets the
files that differ.

On a server the directory changes hands without its files being sent
(`tests/remote.rs`, `server/test/links.ts`). The writer sends the tree
objects from the directory down in one request and names the repository
it linked from. The server reads them, finds the files below that the
receiving repository lacks, and copies the rows for those that the named
repository has from that repository's table into this one's; no content
moves. It answers with the ids neither has, and the writer sends those
with the push. Someone who syncs gets the commit, asks once for the
directories it names, and fetches the files in its view like any others.

Against a local `cf dev`, with all of oxc pushed once as a repository of
its own (14,716 contents, 1.1 s) and then linked into another:

| | |
| --- | ---: |
| `nv link` of the 14,716 files, most of it writing them | 2.8 s |
| Stored by the link, and by publishing it | 464 + 530 bytes |
| `nv push` of the link | 833 ms |
| File contents sent in that push | 1 (`nv.links`) |
| Contents the other repository's table was asked for and gave | 13,693 |
| `nv sync` into a store that had never seen oxc | 3.8 s, 14,717 files |
| A later push and sync of one file changed inside the directory | 83 ms, 79 ms |

What this does not do yet: the request that hands a directory over
carries every tree object below it each time, so it grows with the number
of directories and names, though not with the bytes of the files. Taking
contents over from another repository asks no permission, because nothing
on the server does. And the first `nv commit` after linking oxc took 1.3 s
where later ones took 70 ms; where that time goes has not been looked
into.

## Where the server keeps contents

The server kept each file's content as an object of its own in the bucket,
under the repository's name, and every byte went through the worker. It
now keeps two kinds of object, both named by what is in them and so
written once:

- `packs/<hash>`: the contents that came with one push, end to end. Where
  each is in the pack is a row in the repository's own table.
- `objects/<id>`: one content by itself, for what a writer sends outside a
  push because it is large (a megabyte or more). It is streamed into the
  bucket and hashed on the way, and never held whole.

Names are not per repository, so the same content is stored once for all
of them. What a repository may read is what its table says it was given:
another repository asking for the same id gets a 404 until it has sent the
bytes itself. Knowing a hash is not having the content.

Against a local `cf dev`, the same requests before and after:

| | one object per file | packs | packs, compiled hash |
| --- | ---: | ---: | ---: |
| Push of 200 small files: writes to the bucket | 200 | 1 | 1 |
| Push of 200 small files (335 KB) | 99 ms | 46 ms | 21 ms |
| Fetch of those 200 | 54 ms | 15 ms | 14 ms |
| Push of 2,000 small files (3.5 MB) | 1,195 ms | 392 ms | 157 ms |
| Fetch of those 2,000 | 1,584 ms | 103 ms | 100 ms |
| 8 MB by itself | 597 ms | 362 ms | 106 ms |
| 256 MB by itself | 9.9 s | 11.4 s | 3.0 s |

The times are of a simulation on one machine, where a write to the bucket
costs no network; the count of writes is what carries over, since the real
bucket is billed and rate-limited per operation. Locally nothing stops a
worker from holding 256 MB, so the old server passes the last row where a
deployed one would not; after the same requests its `workerd` held 1.6 GB
against 0.9 GB for the new one, which is suggestive and no more.

The last column is the same server with its hashing changed. The first two
hash in JavaScript, which the local runtime does at about 25 MB a second,
and that was most of a push and nearly all of a large upload. The server
now hashes with the `blake3` crate nv itself uses, compiled to WebAssembly
with SIMD (`server/hasher`, 28 KB as `server/src/blake3.wasm`, rebuilt by
`pnpm run build:hasher`): 2.2 GB a second in node against 98 MB a second
for the JavaScript one there. With the hash left out altogether the 256 MB
upload still takes 2.75 s, so what is left is the stream and the local
bucket. The compiled side allocates nothing and holds one hasher; a
content being hashed while another arrives has its 1.9 KB of state kept on
the JavaScript side, so an upload that is abandoned leaves nothing behind.
A test hashes the same bytes with both, at sizes around every boundary and
in pieces of any size.

`nv push` sends a file of a megabyte or more by itself ahead of the push
that names it, and `nv sync` fetches such files one by one; a test pushes
fifty small files and one of 3 MB and finds one pack and one object in the
bucket.

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
| New working copy: Git / nv / nv fork | 989 / 1,273 / 200 ms | 3,038 / 1,147 / 156 ms | 4,890 / 1,729 / 1,032 ms |
| One directory: Git / nv | 144 / 100 ms | 111 / 73 ms | 117 / 12.5 ms |
| `status`: Git / jj / nv | 64 / 52 / 66 ms | 39 / 53 / 44 ms | 179 / – / 229 ms |
| Record an edit: Git / jj / nv | 41 / 158 / 70 ms | 37 / 149 / 49 ms | 113 / – / 236 ms |
| Record an edit the writer reported: nv | 6.9 ms | 7.1 ms | 1.9 ms |

Four more, cloned to see what would break (`first / again` is a whole
checkout the first time the store is asked for that tree, and after):

| | Commits | Files | Git ids reproduced | Import | Store / Git packs | Checkout: first / again / Git |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| vuejs/core | 7,303 | 704 | all | 2.8 s | 34 / 53 MB | 73 / 12 / 76 ms |
| astral-sh/ruff | 18,268 | 11,338 | all | 18.6 s | 189 / 247 MB | 983 / 210 / 838 ms |
| git/git | 83,079 | 4,876 | all | 52.9 s | 316 / 323 MB | 439 / 101 / 360 ms |
| golang/go | 68,874 | 16,366 | all | 50.7 s | 482 / 482 MB | 1,509 / 244 / 1,162 ms |

git/git did break it: 98 of its commits came back with another id. They are
from Git's first weeks, April 2005, when trees recorded files as mode
`100664`. `git log --raw` and `git ls-tree` both print that as `100644`, and
the import believed them. It now reads the tree objects themselves whenever
a tree's id does not come out right. Octopus merges (37), several roots (7),
a submodule, symlinks and signed commits (16,767 in ruff) all went through.

Where the files of a checkout differ from Git's, it is `.gitattributes`: 13
files in ruff and 3 in git are marked `eol=crlf`, and Git writes them with
CRLF where nv writes the bytes it stores. Every one of them equals its blob.

What this says:

- **Git ids hold at every size tried**: 271,812 commits across ten
  repositories, none different (see below for the one bug that took).
- **Checking out everything is one clone of a tree the store keeps.** At
  100,000 files that is 1.7 s where `git worktree add` takes 4.9 s, and on
  playwright 37 ms against 371. A commit near one already checked out is
  that tree with the difference applied (3.1 s at 100,000 files for the
  commit before; 205 ms for 200 commits back on playwright). Only the
  store's very first tree is written file by file, and that is the one case
  slower than Git: 6.1 s against 4.9. The oxc and lean4 columns predate
  this and are the old file-by-file figures.
- **Looking over the whole tree was not faster than Git** when these tables
  were made: `status` and an unreported snapshot asked about every file on
  one thread, and at 100,000 files Git was ahead, 179 ms to 229. It is now
  done on several (see "Looking over the tree": 40 ms against Git's 58 on
  oxc); the 100,000-file figure has not been taken again.
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

## What reviewers found

Before anything here was called finished, reviewers who had not written it
were given the binary, this file and one use case each, and told that a
reproduction of a failure was worth more than a confirmation. They ran Git
beside it, on oxc and on repositories of their own making. Three use
cases: an agent recording its work; Git and nv in one directory, and nv's
own Git commands; many writers, partial workspaces, forks and links. What
follows is what they measured and what they broke, not what the tests
here say.

**The first round** found that nothing lost by the tests was the same as
nothing lost:

- `kill -9` during a snapshot could leave the workspace's own file empty,
  after which a commit replaced Git's branch with a parentless one. Git
  under the same 1,500 kills lost nothing.
- Ten forks committing at once corrupted the store they share.
- A push of two commits could silently overwrite a change it had never
  seen; a ref started by two commits held one; a push that was repeated
  conflicted with itself; a sync killed half-way left a workspace that
  could neither push nor sync.
- The hooks that kept Git from moving stopped `git reset --hard` after it
  had rewritten the files. Commits made for Git ignored its excludes,
  filters and identity.
- A dozen outputs of `nv git` were wrong without being refused.

Each of those is now a test that failed first. Git's directory is no
longer guarded but followed, and its commits are made by Git.

**The second round**, by other reviewers on the build after those fixes,
confirmed every one of them fixed (about 1,750 kills of recording
commands, 102 of a push, 76 of a commit: no damage; 16 forks committing
and pushing at once: no corruption), and found a new set:

- Following the `restore` hint that a sync printed, then committing and
  pushing, removed the other writer's work for everyone.
- `nv commit` during a Git merge wiped the merge's index state.
- After a rename that changed only a letter's case, `restore` deleted the
  file; a name with a decomposed accent could not be published beside Git.
- A directory that is a symbolic link let `nv record` reach outside the
  workspace.
- A refusal by the user's own hook dropped what the user had staged.

Those are fixed in the same way. What the second round measured, on oxc
unless said otherwise, with two other reviewers loading the same machine:

| | nv | Git |
| --- | ---: | ---: |
| Record one edited file (hook; `git commit -am` with fsmonitor and untracked cache) | 6.5 ms | 20.2 ms |
| Record 100 edited files (`--recorded`; `git commit -am`) | 24.8 ms | 66.3 ms |
| Mark a state; list what changed since (`--recorded`) | 4.3 ms; 3.9 ms | 37.4 ms; 14.9 ms |
| Go back to the marked state, keeping the bad attempt | 14.7 ms | 75.7 ms |
| Publish one changed file as a commit, beside Git | 74.6 ms | 31.2 ms |
| `status`, Git with its defaults | 37.4 ms | 59.3 ms |
| `status`, Git with fsmonitor and untracked cache | 37.4 ms | 22.5 ms |
| No Git beside it: commit one staged file; `add`; `commit -a`; `diff` | 5.7; 8.9; 21.2; 18.2 ms | 30.8; 12.3; 31.8; 17.9 ms |
| 8 writers, 50 commits each, to one branch | 2.9 to 4.1 s | 43.7 s |
| 16 writers, 50 commits each | 8.7 to 14.4 s | 275.7 s |
| A workspace of one crate | 17 ms | 58 ms (sparse worktree) |
| 10 working copies, with build output | 2.4 to 3.0 s, 60 MB | 10 to 11.6 s, 1.25 to 1.68 GB (`worktree add`, no build output) |
| Put 201 files of another repository in this one | 103 ms, 4 KB | 1.0 s (submodule), 1.4 s (subtree) |

So, in their words and numbers: recording is about three times quicker
than Git's quickest commit and going back five times; many writers on one
branch are ten to thirty times quicker and need no retries where their
paths differ; a partial workspace and a fork are several times quicker
and smaller. Publishing a commit beside Git is two to three times slower
than `git commit`, because it is `git commit` and more. `git status` with
its file monitor and untracked cache on is quicker than `nv status`, and
`git diff` is level with it. Setting up costs an import: 20 s on oxc,
where `git clone` takes 2.

What nv does that Git does not, or only with a script around it: going
back in one command that saves the state left behind first; snapshots
that never reach history; recording that costs what was written, not the
tree; a server that puts non-overlapping pushes on the tip itself; a
workspace of one directory with nothing else on disk; forks that carry
build output; a directory of another repository standing in this one as
an ordinary directory, by its id.

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

- Publishing a commit beside Git is two to three times slower than
  `git commit`. `git status` with fsmonitor and the untracked cache is
  quicker than `nv status`, which has no equivalent of either.
- Two writers who each add a file whose names differ only in case both
  land; on a file system that does not tell the names apart, a workspace
  then has one file on disk for the two.
- The server trusts anyone who can reach it. It checks that contents hash to
  their names and that a commit decodes, not who is writing. Taking a linked
  directory's contents over from another repository asks no permission
  either.
- The server keeps contents uncompressed: a pack is the contents of a push
  end to end, the delta chains of the local store do not exist there, and
  nothing is ever collected. Its check of a large upload has only been
  timed locally, and a worker's limits on time and memory have not been
  met by a real file of a gigabyte. Nothing yet fetches a large file only
  when it is opened.
- A workspace with several unpushed commits that has to catch up first gets
  them back as one commit.
- The server compares paths. Two changes to different functions of one file
  need a sync and a second push from one of the writers; the merge is the
  client's. A push that was refused stays on the server as a row nobody
  reads or removes.
- A ref has to be started from history its writers share. The server knows
  the ref's own commits and the one before the first; a workspace on
  something older cannot find its place.
- A workspace that is a view of part of the tree does not see the
  `.gitignore` or `.gitattributes` of the directories above its view: they
  are not on its disk. Files written outside the view are pointed out by
  `status`, `snapshot` and `commit`, and not recorded.
- `nv adopt` imports the whole history behind the commit Git is on: 20 s
  and a store about the size of `.git` on a repository of 20,000 commits.
  There is no shallow adoption.
- Segments are written in arrival order. Reading one old version may touch a
  chain scattered over the file; a compaction that puts a path's chain
  together is not written.
- A fork keeps build output, but whether a build tool still trusts it under
  a new absolute path has not been tried.
- In a workspace with no Git beside it, `.gitattributes` is read for line
  endings only: no filters, no LFS. Beside Git, commits are Git's and
  filters apply to them; nv's own snapshots hold the files as they are on
  disk.
- `.gitignore` is honoured, and `.nv/exclude` is a workspace's own list in
  the same form. Nothing is skipped by name, so a workspace without rules
  sees its `target` and `node_modules`.
- Nothing searches the snapshots for the one that broke a test.
- The watcher has only been run on macOS, and copy-on-write clones are
  APFS's: on another file system a checkout or fork would have to copy.
- A tree whose entries are not in Git's order, or whose directory modes are
  written with a leading zero, would not round-trip. File modes Git no
  longer writes do. None of the former turned up in 271,812 commits.

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
