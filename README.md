# newvcs

> [!WARNING]
> **This is a prototype, not something to use.** newvcs is a proof of
> concept, written to find out whether its direction holds up: a store
> that keeps Git's object ids without Git's storage, recording that costs
> what was written, and many agents writing to one branch at once. It is
> not meant for real work, and it is not safe for it.
>
> - Ten rounds of independent review have each found ways to lose data
>   that the tests had not ("What reviewers found" below). Each was fixed;
>   there is no reason to think the eleventh will find none.
> - The on-disk format, the commands and the server protocol will change
>   without notice or migration.
> - The server has no authentication and has never been deployed.
> - Beside a Git repository (`nv adopt`) it can still leave Git's state
>   other than you would expect.
>
> Do not point it at a repository whose history or working tree you
> cannot afford to lose. Use a throwaway clone.

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
nv fetch <dir> [--quiet]                   bring the ref's commits into the store; no file changes
nv export <dir> <git-repo> <ref>           write the workspace's commit into a Git repository

nv name <store> /<owner>/<repo> <rev>      name a repository in the store
nv link <dir> <path> /<owner>/<repo> [dir] put a directory of another repository there, by its id
nv links <dir> [--update]                  how each link stands; follow the ones that moved
nv unlink <dir> <path>                     stop following one

nv check <dir>                             how the workspace stands with Git and with its watcher
nv git <command>                           Git's command line (see below); toward the ref a
                                           workspace pushes to, `push` is nv push,
                                           `fetch` nv fetch, `pull --rebase` nv sync, and where it stopped
                                           `rebase --continue` and `--abort` finish or undo it
nv help [command | all], nv --version     the everyday commands; one command; all of them
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
against the table before any of it runs; short options run together
(`-qa`, `-qaF <file>`, `-m<text>`) are taken apart first, as Git's parser
takes them, and a cluster with a letter the table does not have is
refused as it was written:

| Command | Options |
| --- | --- |
| `init` | `-q` `-b <branch>` |
| `remote` | the list, `-v`, `get-url origin`: the server `nv remote` set, or the Git repository it was cloned from |
| `clone`, `fetch`, `pull`, `push` | `-q`; `pull --ff-only`, `--no-rebase`; `push` of the branch one is on, to the branch of its name on `origin`. A Git repository on this machine only |
| `add` | `-A`, paths |
| `rm`, `mv` | `rm -r -q -f --cached` (the file stays on disk, where Git then does not track it) |
| `restore` | `--staged`, paths |
| `reset` | `--soft` `--mixed` `--hard` `-q`, a revision or paths |
| `commit` | `-m` (more than once, a paragraph each) `-F <file>` (`-` for standard input) `-a` `-q` `--allow-empty` `--amend` `--no-edit` |
| `status` | the long form, `--porcelain`, `--short`, `-b` (`-sb`) |
| `log` | `--oneline` `--format=` `--pretty=format:` `--pretty=tformat:` `-n`, a revision; the default format. Placeholders: `%H %h %T %t %P %p %s %b %B %an %ae %at %cn %ce %ct %n %%` |
| `show` | the same formats, `--name-status` `--name-only` `--stat`, a revision, an annotated tag, or `<rev>:<path>`; a commit kept to paths after `--` |
| `diff` | patches, `--name-status`, `--name-only`, `--stat`, `--diff-filter=`: two revisions, one revision against the files (or with `--cached` against the index), the working tree against the index, `--cached`; each kept to paths after `--`, or named without it where Git would take them for paths |
| `rev-parse` | a revision, `<rev>^{tree}`, `<rev>:<path>`, `--short`, `--abbrev-ref HEAD`, `--show-toplevel` |
| `cat-file` | `-t` `-p` `-e` for commits, trees, blobs and tags, by revision or `<rev>:<path>` |
| `ls-tree` | `-r` `--name-only`, a revision or `<rev>:<path>`, one path (`dir/` lists what is in it) |
| `branch` | list, create, `--show-current`, `-d`, `-D` |
| `tag` | list, create, `-a` / `-m` for an annotated one, `-d` |
| `checkout`, `switch` | a branch (one only `origin` has is made, following it), `-b` / `-c` with a start point, `checkout [--] <paths>` |
| `merge` | `-q` `--no-ff` `--ff-only` `-m`, one branch; `--abort` |
| `config` | `user.name` and `user.email` |
| `fsck` | `--full` `--strict` `--no-dangling` |

Revisions are `HEAD` (or `@`), a branch (also as `refs/heads/<name>`), a
tag, `origin/<branch>`, a full id or the
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
ok       25-a-third-round.sh (31 lines)
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

Cases 22 to 25 are there because reviewers who were asked to find
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
- A tag of a tag, `show` of a merge that settled a conflict (Git shows
  both sides), and a merge where one side renamed a file the other edited
  are refused or stop, where Git goes on. A branch does not remember which
  branch of `origin` it follows: the lines Git prints about being ahead or
  behind are not printed.
- Short ids are as long as Git makes them from the number of objects
  (seven, and more in a large repository), counted in nv's store, which
  can differ from Git's count near a boundary.
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
- Paths after `--` are names of files and directories, from where the
  command is run. Git's patterns and magic (`'*.txt'`, `:(exclude)...`,
  `:/`) are refused, as are paths given to `show` of a merge or a tag. As
  in Git, `show` of a commit that changes nothing under them prints
  nothing, and a move with one end outside them is the other end alone.
- `diff` while a merge has conflicts is refused: Git shows both sides
  against the result, which is not spoken.
- `.gitattributes` is read for line endings (`text`, `text=auto`,
  `eol`, `-text`, `binary`), and for `merge=union`, which `git merge` and
  `nv sync` both honour: where both sides changed the same lines of such a
  path, both sides' lines are kept, ours first, and the path is merged.
  `merge=<name>` is merged by the command `merge.<name>.driver` gives, as
  in Git (`%O %A %B %L %P %S %X %Y`, the result left in `%A`, exit 0 where
  it is clean), wherever the workspace's configuration (`nv git config
  merge.<name>.driver <command>`) or Git's defines it, never the
  repository's files; a name nobody defined is the text merge. `nv sync`
  runs a driver as `git merge` does, but there is no index to hold a path
  unmerged, and a file is unsettled for as long as it holds markers: a
  driver that says it could not merge and leaves no `<<<<<<< yours` line
  is not believed to have left something to settle by, and the file is
  merged as text instead. The server runs no driver: a push that the ref
  clashes with on such a path is refused, and the sync runs it.
  `tools/merge-cst` is one such driver, optional and outside nv: it merges
  Python and Markdown by their syntax trees (tree-sitter), unit by unit
  (top-level statements, sections, list items), and merges as text, with
  markers, whatever it cannot decide. It assumes what it merged is tested
  afterwards, as the conditions behind writes test what a sync made.
  Line endings are read as Git does with `core.autocrlf` unset on a
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

Whether a file changed is asked of the file system first: its size, the
time it was last written, and the time the file system itself says it
last changed in any way, which nobody can set. A file given back its size
and its time after an edit is still seen as edited. (It was not, until a
reviewer did exactly that and then watched `nv restore` write over the
edit; Git, which looks at the same three things, had seen it.) A fork,
or a copy made behind nv's back, has every file new to the file system.
What nv knows there is when the copy was made: a file that changed later
was changed in the copy, and is read. In a fork no file is read for
having been copied; in a copy made with `cp`, the files copied after
nv's own directory are read once.

What is in the tree and is neither a file nor a link (a pipe, a socket)
is not looked at. A file that may not be read is named in a warning and
left out; the others are recorded.

Every object read from the store is hashed and compared with its name
before it is used. A bit that changed on disk is a refusal that names the
object, not a file restored with other bytes in it; `nv verify` checks
every object this way before it compares ids with Git's.

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
| `nv commit`, the commit made by Git (older than the eleventh round's fixes: see "Not done") | 108 ms | 81 ms | 74 ms |
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

### Checkpoints: what the tests passed on

The point of recording every write is that a writer should not have to
think about version control at all. What it is asked for instead is what
it is asked for anyway: that the tests pass. A repository declares, in
`nv.toml` at its top, the conditions a state is accepted on, and the
stages of a pipeline that come after them:

```toml
auto = true                 # run them behind every write a hook records

[checks.unit]
run = "cargo test --lib"    # passes when it exits 0
timeout = 600               # seconds; past it, it fails, with all it started

[checks.compat]
run = "tools/git-compat.sh"
after = ["unit"]            # run only on a state `unit` passed

[message]
run = "claude -p 'Write a one-paragraph commit message for this diff'"
timeout = 120               # the change on stdin, as `git diff` writes it
```

`nv checkpoint <dir>` records what is on disk and runs the conditions on
it, each after those it waits for. The state becomes `checkpoint-unit`,
`checkpoint-compat`, as far as it got: names `nv restore` and `nv since`
take like any mark. A condition whose wait did not pass is not run. When
every condition passes, the `[message]` hook drafts a commit message from
the change since the last commit, and keeps it with that state.

With `auto = true`, `nv hook claude` starts the same run behind the tool
call and returns at once. There is one runner to a workspace; it goes
round again while the files change under it, and gives up after twenty
rounds.

`nv commit <dir>` with no message then publishes that state under the
drafted message. A message given is used instead. Either way, where
conditions are declared, the state must pass them first: a state they
passed on before is not run again; a new one is run, and refused with
exit 1 when it fails (`--no-checks` commits it all the same).

What a run finds is about the state it began on only if the files are
still that state when it ends. The conditions run in the workspace
itself, not a copy, so a writer that went on writing during the run makes
it prove nothing. Then nothing is named and nothing is kept, and the
runner goes again. The workspace is held only to record and to name,
never while a condition runs.

With `auto = true` a watcher starts the runs too, after each snapshot that
recorded something: a writer that only writes files needs neither a hook
nor a command.

#### A commit at every state that passes, made again before it is pushed

```toml
[commit]
each = true                 # commit every state all the conditions pass on
```

With `each`, a state every condition passed on is committed there and
then, under the drafted message or a provisional one (`checkpoint: unit
passed`), signed as any commit made here is (the environment, the
workspace's `user.name` and `user.email`, then Git's configuration; with
none, the state is not committed and `nv checkpoint` says why). The writer
may have gone on writing while the conditions ran;
the commit is of the state they passed on, and what was written since stays
recorded on top of it, uncommitted. These commits are meant to be made
again before anyone else sees them:

- `nv pending <dir>` lists the commits not pushed, oldest first: number,
  Git id and nv's, message, the conditions the files passed, and each path
  as `A`, `M` or `D`. `--wait` first waits until nothing more is coming
  (the watcher has recorded every write, no run is under way) and says
  whether what is on disk is committed or did not pass.
- `nv rebuild <dir> <commit> <message> [<commit> <message>...]` makes them
  again as fewer commits: each new one ends at a `<commit>` (its number, or
  the start of either id) and holds everything since the one before,
  under the author of the first commit it is made of, as a squash in Git
  keeps it; the committer is whoever ran it.
  Where each `<commit>` passed, each new commit did, as results are kept by
  the files' tree as well as by the state. The files are not touched. It
  refuses where there is no end to what is not pushed (no ref and no commit
  the workspace was made at), and while a push is unheard.
- `nv note <dir> <line>` says what the next step was: the next commit the
  conditions pass on is made under that line, and where the runner has
  committed those files already under a message of its own, that commit
  is renamed. A writer that notes each step it finishes has said where
  commits end without ever committing. A note waits for a commit however
  the work goes meanwhile (a state that fails, commits below it made
  again); notes that wait together, because the conditions passed on
  none of the states in between, are one commit, under their lines
  joined by `; `, as is a line given for files committed under a note
  already. Only a state the conditions passed on is a commit's end.
- `nv publish <dir> [--message <line>]` waits as `nv pending --wait` does,
  makes the commits again with one ending at each note (what came after
  the last, under `--message`), and pushes. A `[body]` hook
  (`run = "..."`, the change on stdin, the summary line in `NV_SUBJECT`)
  is asked for each commit's body. Where the push is refused over files
  the ref changed meanwhile, it catches up, writes every body again of
  the change as it now lands, and pushes again; where catching up leaves
  a file with both versions, it stops with exit 1 and says so, and the
  note of a commit the files held is kept for when they are settled.

A file a sync left with both versions is held out of the commit of the
workspace's own that changed it, and its settling completes that commit:
committed by the runner behind writes, or by `nv commit <dir>` given no
message, which keeps the message of the commit it completes, as
`git rebase --continue` does, and puts those after it on top again.
Given a message, `nv commit` makes the settling a commit of its own, and
the commits below it are on the ref without it.

`nv fetch <dir>` (and `nv git fetch` toward the ref) brings the ref's
commits since the workspace last looked into the store, with the
contents of the files they changed that are in view, and writes no file:
what the workspace stands on stays where it was, and only `nv sync` moves
it. In `nv git`, `origin/<ref>` (and `origin`) is the ref's tip as the
workspace last saw it, from a sync, a push or a fetch, whichever came
furthest along the ref; `log`, `show`, `diff` and `status` compare with
it, as Git's do after `git fetch`. `git rebase origin/<ref>` is not
spoken: nv puts work on the ref's tip as it is now (`pull --rebase`), not
on a tip fetched earlier.

A `git pull --rebase` that left files with both versions says what comes
next in Git's verbs: `git add` them and `git rebase --continue`, or
`git commit -m <message>`, which completes the commit the settling goes
into under that message, wherever it is among the commits not pushed, as
Git's `commit` at a rebase stopped at that commit does. Either commits
what was added, as Git's index has it: the settled files and whatever
else was added, not files that are only on disk. Until that commit is
made, no sync or push goes ahead, whether the markers are still in the
files or were edited out and not committed (`git rebase --abort`, which is
`nv restore <dir> before-sync`, undoes the pull, its staging included).
Anything staged with `git add` before a `git pull --rebase` makes it
refuse, as Git's does; `nv sync` refuses only where the ref changed a
staged path, whose staged copy would not have what the ref brings, and a
push that landed and caught up unstages such a path and says so.

A file a sync left with both versions is settled by whoever settles it,
and the commit that holds the settling says so, as a Git trailer: which
file, and the commits of others' it was settled against, as
`Settled: README.md with 1a2b3c4 (Add a)`. Remade by `nv rebuild` or
`nv publish`, a commit keeps the trailers of those it is made of. Nothing
else in a history says that two writers' work met there, and a reader of
it could not tell (`eval/commit-agents`, "Readers").

A writer that runs no version control can still be told things by
whoever supervises it: `nv tell <dir> <line>` leaves a line in that
workspace, which the writer is given once, the next time nv speaks to it
there (`nv inbox <dir>`, the answer to `nv note`, or the hook behind a
Claude Code session's tool calls, as context for its next turn).

A commit's message is written of what was there when it was made. When a
sync or a push puts it on top of other writers' commits that change the
same files, `nv sync`, `nv push` and `nv pending` name those commits and
say that what the message says of what was there may not hold, until
`nv rebuild` (or `nv publish`, which rebuilds) writes it again.

`eval/commit-agents` runs this the way it is meant for: in each of several
forks, a developer agent that never runs a version control command, and
beside it a committer agent that waits for the work, remakes the commits
and pushes, all to one ref. `rounds.md` there has what each round found.

Not done: a write
that lands after a runner's last look and before it ends waits for the
next write to be run on. A sync's own commit on the ref's tip leaves out
a file the sync left with both versions. Its settling, when the
conditions pass on it and it changes nothing but those files, goes into
that commit as long as it is the last made here and not pushed: the
commit is whole again under its own message (a checkpoint's settling;
`nv commit` of it is a commit of its own). Where the workspace had no
commit of its own, the settling is one, on the ref's tip, never a change
to another writer's. Conditions do not run in a copy, so a slow test
suite and a busy writer may never agree on a state. Nothing is shared
with other workspaces yet: taking another writer's checkpoint in early,
and running one's own conditions on the two together, is the next step.

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
  commit of nv's paths only (`--only`), and their staging is left. The
  commit is built in an index of nv's own, a copy of Git's; Git's index
  is touched only once a commit exists, to be brought up to it. So if no
  commit comes of it (a hook said no, Git would not add a path, nv was
  killed), Git's index was never changed. A file staged in part is
  refused by name: which of its lines go in is the user's to say. A
  removal or a rename made with `git rm` or `git mv` is published like
  any other change. (Staging in Git's own index first, as an earlier
  version did, left nv's staging behind whenever nv was killed, and the
  Git it had started went on staging after it; nothing could then tell
  that staging from the user's.)
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
`nv commit` (Git will not add it); files a sparse checkout keeps out of
the tree are not taken for files removed. An adoption that was cut short
is finished by running `nv adopt` again, and Git is told to ignore `.nv/`
before anything is imported. `nv status` does not
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

`git diff <rev>`, a commit against the files, looks at what `git diff`
does and at the paths that differ between that commit and the last one.
On oxc of 17,002 files on a Linux VM of four threads, one file edited,
means of 20 runs: `diff HEAD` 20.4 ms, Git 16.3; `diff --stat HEAD~5`
22.3 ms, Git 16.7; kept to `-- crates/oxc_parser`, 7.6 ms, Git 7.5. On
that machine plain `git diff` is 23.2 ms against Git's 16.2: the gap is
that of `git diff`, which the table above measured on another.

## Pushing at once

`server/test/e2e.ts`, against the Durable Object under `cf dev` on the same
laptop. Every writer commits on top of its own last accepted commit and never
fetches, as an agent in its own workspace would.

| Writers | Pushes | Accepted without a retry | Kept aside as conflicts | Pushes/s | p50 | p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 400 | 400 | 0 | 212 | 4.0 ms | 14.4 ms |
| 16, disjoint paths | 1,600 | 1,600 | 0 | 419 | 35.0 ms | 74.0 ms |
| 64, disjoint paths | 3,200 | 3,200 | 0 | 524 | 117.6 ms | 167 ms |
| 16, every 10th push on one shared file | 1,600 | 1,450 | 150 | 479 | 31.1 ms | 60.5 ms |

Each push here carries the contents it names, and the server checks that
every content a commit names is there before it takes the commit. Before
that check existed the test named contents it never sent, and the same
table read 723 to 774 pushes a second: those were pushes no workspace
could have checked out.

How the cost of one push grows (`server/test/bench.ts`, medians):

| | 1 | 10 | 100 | 1,000 | 10,000 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Commits the writer's base is behind the tip | 5.6 ms | 5.3 ms | 4.6 ms | 4.1 ms | 3.9 ms |
| Paths changed by the commit, a content each | 4.3 ms | 4.7 ms | 11.3 ms | 74.3 ms | 328 ms |

Since the server merges `merge=union` paths, it also keeps every change to
every path (`path_index`, a row beside the one it had), to check what a push
says a path was at its base. Measured on a 4-core Linux VM, the code before and
after run back to back, twice each (the VM is far slower than the machine of
the table above, for both): a push of 1,000 paths 653 to 732 ms before and 722
to 724 after, of 10,000 paths 35.5 to 38.8 s before and 35.9 to 38.8 s after;
64 writers on one repository 126 to 128 pushes a second before and 107 to 111
after, which is the row. On 8 and 64 repositories, 124 to 128 before and 110 to
128 after.

How far behind a writer is does not matter: the server keeps, for each path,
the position at which it last changed, and looks up the pushed paths. What a
push costs is its own paths, each a row written and a content hashed.
Spreading 64 writers over 1, 8 or 64 repositories gave the same 515 to 557
pushes a second, because the local
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
  between Git's markers, as a change not yet recorded, unless
  `.gitattributes` gives it `merge=union` (below); the sync exits 1,
  as `git pull` does, and `nv commit` refuses while a file from that sync
  still holds the markers. What cannot be merged as text (a file one side
  removed, a binary file) stops the sync with nothing touched. The way on
  from there is `nv restore <dir> pushed`: the workspace goes to the ref
  as it last saw it, and what it had beyond that (commits not pushed,
  snapshots, the files) is set aside under an id it prints, to be read
  with `nv since` or written out beside the workspace with `nv checkout`.
- **A list every writer adds to need not be a conflict.** A file of
  entries, one to a line (imports, a changelog, names exported), meets
  the same lines changed by every writer after the first, though no two
  disagree. Given `merge=union` in `.gitattributes`, as Git users give a
  changelog, a sync keeps both sides' lines and goes on. Union is only
  right where each entry is a line of its own and order does not matter:
  two writers who each wrote `__all__ = ["theirs"]` on one line get both
  lines, and the second assignment wins. Where the conditions run behind
  writes (`auto`), they are run on what a sync or a push made, so that
  `nv pending` says whether the merged commit passes; a union that went
  wrong is caught there, when the conditions look at it. A sync that put
  a commit of the workspace's own on a newer tip says so, and where to hear
  how they end; without `auto`, it says they have not run on it.
- **The server merges such a list itself, and the push lands.** A push
  refused only over paths `merge=union` gives would cost its writer a sync
  and a second push, every writer after the first, every time. So a push
  says, of each such path it changes, what the path was at its base and
  what the `.gitattributes` above it said there, and sends those contents
  with it. Where the ref clashes only on such paths, the server merges each
  one, commit by commit, on what the ref has, with the code a sync uses
  (`src/union.rs`, compiled to WebAssembly in `server/merger`, so that the
  bytes are the same), and the push lands with the merged contents. `nv
  push` names the paths; the workspace is brought to what landed, and a
  later sync knows the commits for its own though their contents differ.
  What the pusher says is checked against the ref's own history wherever
  it has one: what a path was at the base, the rules at the base. Where the
  ref has none (the path is as it was before the ref began), it is taken
  on the pusher's word. That gives a writer nothing it could not have by
  syncing and pushing whatever it liked on top, and a union never drops a
  line of anyone's. Not merged, and refused as before: rules that changed
  on the ref since the base or change in the push, a path removed, a mode
  changed, a binary file, a content the repository was not sent, a
  directory moving above or below the path, and a ref that began before
  the server indexed every change to every path (one row per path a commit
  changes, beside the row it had). A merged push writes the bucket twice:
  the contents it brought, then the merged ones. A file over 256 KiB is
  not merged here: the push is refused and its writer's sync merges it, so
  that no push holds the server for longer than a moment. Where pushes
  arrive at once, a merge made on a tip that moved is made again, up to
  sixteen times. Macros of `.gitattributes` (`[attr]m merge=union`) are
  read as Git reads them, here and in a sync: set, a macro says what its
  attributes say; defined anywhere but the top file, it is not one.
- **What a workspace pushed is its own work, not a change of someone
  else's.** A file committed, edited again and then pushed stays as
  edited, the edit an unpublished change on the commit; a file committed
  and then removed stays removed. The first version compared the pushed
  commit with the files as if another writer had made it: a conflict with
  oneself, a deletion undone, a push of bytes reported refused that had
  landed.
- **A workspace moved back catches up; it does not take back.** After a
  reset to an older commit the workspace asks the server where that
  commit stands, and a sync brings what the ref has since. Before, the
  difference was read as the workspace having removed those files, and
  its next push removed them for everyone.
- **A sync can be undone, and undone means undone.**
  `nv restore <dir> before-sync` puts back the files as they were, a file
  that was never recorded among them, and what the workspace believed of
  the ref, so that a push is refused again as it was. Going back to a
  state from before the last sync any other way is refused: the first
  version printed that state's id and invited a plain `restore` to it,
  which put the old files on top of the new commits, and the next commit
  and push then removed the other writer's work for everyone without a
  word. A reviewer followed the invitation and found that. The price is
  that a mark made before a sync is no longer somewhere to go back to;
  and once the workspace has pushed, there is no sync left to undo.
- **Nothing moves while a file is unsettled.** Until the files a sync left
  with both versions are edited and committed, `nv sync` and `nv push`
  refuse, as `git pull` does. A second sync used to forget which files
  those were, and the next commit and push published the markers.
- **A merge keeps what each side did to a file's mode** as well as to its
  text: one side making a file executable and the other editing it gives
  an executable file with the edit.
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
and smaller. Publishing a commit beside Git is three to four times slower
than `git commit`, because it is `git commit` and more. `git status` with
its file monitor and untracked cache on is quicker than `nv status`, and
`git diff` is level with it. Setting up costs an import: 20 s on oxc,
where `git clone` takes 2.

**The third round**, by a third set of reviewers, again confirmed the
round before fixed (the lost update through `restore` could not be
reproduced in more than twenty variations; 448 more kills of a push or a
sync, and two of the server, all converged with every writer's work
present once; beside Git, 43 of 43 command lines were Git's byte for byte
and `--undo` left `.git` identical). It found:

- An edit that keeps a file's size and time was invisible, and `restore`
  then wrote over it. Git saw it.
- A merge by `nv sync` dropped the other writer's change of a file's mode.
- A second sync after one that left conflicts forgot them, and the markers
  could then be committed and pushed.
- `git push` in nv's own Git commands ignored its arguments; a dozen
  smaller differences in formats and names.
- An adoption killed half-way left `.nv/` visible to `git add -A`; a
  sparse checkout's absent files were reported as removed.

A fourth round was given that build. It found the third round's fixes
holding where they had been tested, and one that did not go far enough:

- **The edit that keeps its size and time was still lost in a fork.** The
  fork's index came with the times of the files it was copied from, and
  was believed. A copy now believes only that its files have not changed
  since the copy was made.
- **One flipped bit in the store was restored as the file, and
  published.** The store checked what it wrote and not what it read.
  Every read is checked now.
- **A server that anyone can reach could be made to write anywhere.** A
  commit naming `../../x`, an absolute path or `.git/hooks/pre-commit`
  was accepted, stored, and written out by the next workspace to sync:
  code execution on another writer's machine. A commit with an empty path
  made a sync delete the workspace. Paths, modes and names are checked
  where a commit is decoded, on both sides, and a workspace refuses a
  commit a server should never have taken.
- **A push could name contents the server did not have**, and the branch
  could no longer be read by anyone; an upload of wrong bytes under a
  right name replaced the right ones. A push is refused unless its
  contents are there, and an upload is checked before it has a name.
- **A watcher that fell behind was believed.** After ten thousand files
  written at once, `--recorded` gave a state missing 184 of them and said
  nothing. A watcher that has not said it caught up within two seconds is
  not trusted: the command looks at every file itself. Events the system
  says it dropped make the watcher look at every file, and a watcher left
  behind when its workspace was moved is replaced.
- A snapshot taken over a sync that had been killed recorded the half-way
  state; a file whose text conflicted lost a mode change; a pipe in the
  tree made nv wait for ever; one unreadable file stopped all recording;
  `nv commit` committed the whole of a file the user had staged in part,
  and published in the middle of a bisect; restoring over a directory of
  ignored files deleted them.
- In nv's own Git commands: a new file of 120,000 lines took 15 GB to
  show, a merge started over staged changes, messages were not cleaned as
  Git cleans them (so ids differed), and a dozen smaller differences.
  They are case 26.

Those are fixed, each with a test or a case that failed first.

A fifth round was given that build: one reviewer on recording and the
store, one on Git beside nv and nv's own Git commands, one on many
writers to one ref. (A fourth, asked to send the server what no client
would, was stopped by the model's own safeguards before it reported. The
fourth round's fixes on that side, the path checks above all, have been
looked at by nobody but their tests.) Concurrency held everywhere it was
tried: 32 writers, 400 kills of push and sync, twelve forks recording on
one store, a full disk. What broke was single workspaces doing ordinary
things in an order nobody had tried:

- **A workspace reset to an older commit removed, with its next push,
  what others had pushed since.** The difference between where it was and
  where the ref was read as its own deletions.
- **A push treated the workspace's own commit as someone else's.** A file
  committed, edited again and pushed came back a conflict with itself;
  one committed, removed and pushed was written back, and the commit
  pushed a second time; with a binary file the push was reported refused
  though it had landed, and the workspace could go no further.
- **`nv git commit`, killed between two writes, left the branch behind
  its workspace for good**, and later commits fell off the branch at the
  next switch.
- **Beside Git, a file only removed was never published**: `git ls-files`
  was given an option it does not have, and the answer taken as "no such
  file". A file moved was published as half a move.
- **Beside Git, `GIT_DIR` in the environment sent the commit into another
  repository**; a mark from before a `git pull` could be restored and
  committed, undoing the pull; a path reported with `nv record` was
  forgotten when Git's own commit finished behind a killed `nv commit`.
- **A watcher answered "caught up" while events were still arriving**:
  7 rounds in 36 of two thousand writes gave a mark missing files, with
  no warning. It answers now only once the events have stopped.
- **A file of 4 GiB or more was recorded as kept and could not be read
  back.** It is refused before anything is removed.
- **`nv restore` removed a file it had just said it could not read**, and
  wrote over a file the rules leave out (`.env`) that stood where a
  tracked file had to go. Both are refused, naming the file.
- **The store's ready copies of files, cloned into workspaces without
  being read, were believed.** One byte changed in one gave a restore of
  other bytes. A single file's copy is hashed before it is cloned;
  `nv verify` reads every copy, and fails when a ref or a Git id names
  an object that is not there (it passed on an emptied segment).
- **A fork took its parent's index on trust**, and an edit the parent had
  not looked at yet was invisible in the fork. The parent is looked over,
  under its lock, before it is copied.
- `nv git switch` wrote half the files before finding one it would not
  write over; `nv git add .nv` was accepted and stopped every later
  commit; a directory that could not be listed stopped every command; a
  workspace stuck on a change that cannot be merged had no way out that
  worked (`nv restore <dir> pushed` is one now).

Those are fixed too, each with a test that failed first.

A sixth round, three reviewers again (and again none sending the server
what no client would), found the fifth round's fixes holding: a thousand
kills of publishing beside Git, 1,477 kills of push and sync, 129 watcher
bursts of up to ten thousand writes without one silent miss, no wrong
bytes out of any damaged store. And it found more:

- **`nv git switch` wrote over an untracked file whose name differed from
  a tracked one only in case.** `WAY`, untracked, became the branch's
  `way`; the bytes were in no state. Where the file system takes two
  spellings for one file, an untracked file under either is in the way.
- **`nv git` wrote and removed files outside the repository** through a
  tracked directory that had been replaced by a link (`reset --hard`,
  `switch`, `merge`, `rm`), and `nv git add` took a path beyond a link.
  Nothing is written or removed beyond a link now; `add` refuses as Git
  does.
- **A switch, merge, reset or pull wrote half its files before finding
  one it could not write** (an untracked file where a directory had to
  be). It is found before anything is written.
- **Publishing beside Git, killed while Git was committing, left the
  commit made and Git's index not knowing of it**: a plain `git commit`
  afterwards recorded the reversal. That was the price of building the
  commit in an index of nv's own, and it was paid by whoever's tool has a
  time limit. The commit and the bringing-up of Git's index now run as a
  process of their own, which whoever kills `nv commit` does not kill.
- **`nv commit` waited for ever** when a hook of the commit ran nv on the
  same workspace. Such a command is told why it cannot run.
- **In a workspace `nv git clone` made, a sync or a push that put its
  commits on a newer tip left the branch behind**, and the commits made
  after were lost at the next switch. The branch goes where the workspace
  goes.
- **A sync forgot a change of mind**: a file committed, then put back as
  it was and not committed, came out of the sync as committed, on the
  ref and for nv, with the disk saying otherwise.
- **A directory that became a file (or the reverse), reported as one
  path, was recorded as that path alone**, and no state before it could
  be restored again. A reported path now answers for what it held.
- **Looking with `nv git status` made nv forget which paths had been
  reported**, and `nv commit --recorded` then found nothing to commit.
- A rule changed so that files are no longer left out went unseen by
  `--recorded`; `nv git add f; nv commit` left the staging behind and the
  next commit removed the file; a file the user had taken out of Git and
  written a rule for was published again; a restore over an ignored file
  where a directory had to be wrote half its files.

Those are fixed, with tests that failed first.

For the seventh round the reviewers were asked to stop writing scenarios
and write generators: random sequences of ordinary operations, checked
after every step against a model of their own, or against real Git doing
the same in a twin directory. About 125,000 steps between them. No
panic, no hang, no damaged store, 240 more kills of publishing with
Git's index right every time. And:

- **A sync merged the wrong version of a file.** Where both sides had
  changed a file and the workspace had edited it again without
  committing, the uncommitted text was merged with theirs and came out
  committed: edits nobody committed were pushed, lines that had been
  committed were dropped, a file could differ from the commit with every
  command calling it clean (the tester met these 322 times). A sync is
  now two steps kept apart: this workspace's commits against the ref's,
  as committed; then what the files have become since, on top of the
  result, still uncommitted.
- **Links again, in the places the sixth round's fix did not reach.** A
  tracked file replaced by a link to a file that was not there: `restore`,
  `checkout` and `nv git switch` wrote through it and made the file it
  pointed at, outside the workspace. A tracked directory replaced by a
  link: `--tracked` and `nv git commit -a` read through it and published
  what was outside. Nothing is read beyond a link now either, and a link
  at a file's own path is removed, never followed.
- **A Git repository with nv beside it could not take a sync of two
  commits to one file**, and could do nothing afterwards. A merge commit
  could be pushed, and nobody else could then read the ref's history; it
  is refused. A push that had landed was reported refused when catching
  up afterwards could not be done.
- **`nv git reset --hard` and a true merge wrote some files before
  finding one in the way**; `nv git add <dir>` did not stage the removal
  of the file the directory had replaced, and a later `commit -a` dropped
  the directory's files from history.
- Looking with `nv git status` still changed what `nv commit --tracked`
  published; a publishing that came to nothing, followed by the user's
  own `git reset --soft`, un-staged what they had staged; undoing a sync
  dropped commits made after it.

Those are fixed, with tests that failed first.

An eighth round ran the random testers again, longer: about 160,000
steps and 1,400 kills. The sync written anew held (nothing lost,
duplicated or published uncommitted in 9,300 steps over three kinds of
workspace); `nv restore` lost no file's content in some 25,000 restores;
no panic, no hang, no store damaged. What was found:

- **`nv checkout` into a directory that was there wrote over what it
  held**, unrecorded edits included, and through a link in it to outside.
  A checkout makes a workspace, in an empty place or none.
- **`nv git add f/in1`, where `f` had been a tracked file and was now a
  directory, did not stage the file's removal**; the next `commit -a`
  then removed `f/in1` from history, with the file still on disk and
  every command calling the tree clean.
- **`nv git rm` removed files beyond a link standing for a directory**,
  where Git refuses.
- **Beside Git, a publishing that a hook refused left a note behind, and
  after a commit of the user's own the next `nv snapshot` reset Git's
  index**, un-staging what the user had staged. The note does not outlive
  the attempt now, and nv's own commit is known by its message.
- **A look with `nv git status` still changed what `nv commit` published
  next** in two orders. Showing snapshots as Git would see them set them
  aside; nv's own commands now take them up again as they were.
- **A workspace with a push that had landed and a catching-up that could
  not be done then conflicted with its own commit**; one that had undone
  a sync and switched branch took the ref's commits for its own; a commit
  that came to nothing against the ref was pushed as an empty commit; a
  workspace that looked for the ref before it existed had no way out.
- `reset --hard`, `checkout -- .`, `restore .` and a merge that stops for
  a conflict still wrote some files before finding a directory in the way
  of a file; a restore did the same at a directory it could not write in.

Those are fixed, with tests that failed first, and nobody has reviewed
that build either. Eight rounds have each found data loss the tests did
not, though less each time since the testers went random: the eighth's
were at the edges (a checkout over an existing directory, a file that
became a directory, a link standing for one). This is not yet something
to trust with work that exists nowhere else.

A ninth round had a reviewer the safeguards of the model doing the
reviews had stopped since the fifth: one sending the server and the
client what an honest peer never would. It found the worst thing any
round has found.

- **A tree's entry names were never checked, by either side.** A commit's
  paths were, since the fourth round; but a commit may set a whole
  directory by its tree, and the names inside that tree went unread. One
  push through the real server, of a directory whose tree named
  `../../outside/pwned`, made every client that synced the ref write
  that file outside its workspace. Named `../.git/hooks/pre-commit`, with
  the mode of a program, it planted a hook in a Git repository with nv
  beside it, and the user's next `git commit` ran it. Every tree is now
  read as a commit is, on both sides: each name is one name (no `/`, not
  `.` or `..`, not empty), not anything a file system takes for `.git` or
  `.nv` (in any case, with characters HFS+ ignores, with the dots and
  spaces NTFS drops, as NTFS's short name `GIT~1`, in full-width letters),
  each mode one a file has, the names in order and once each.
- **The server did not know a directory's id from a file's**, and a push
  giving one for the other made the ref one that nobody could sync again.
  It keeps which ids came as directories now.
- **The server took commits with more than one parent**, parents nobody
  had among them, and the ref's history could no longer be written out
  by anyone. A ref is one line of commits; such a push is refused.
- A malformed escape in an address answered with a server error; a `/`
  spelled `%2F` made two addresses one repository; `after=abc` was taken;
  a client fetched whatever a server named as a large file.

The other reviewers of the round, on the same build:

- **Beside Git, a commit-msg hook that adds to the message (a sign-off),
  with something the user had staged, left Git's index behind nv's
  commit**, and the user's next `git commit` took nv's changes back. nv
  knew its own commit by its message; the step that makes the commit now
  knows it by having made it.
- `nv git commit -a` with nothing to commit dropped what had been staged;
  `nv git restore` of a file staged as new and then edited was refused;
  ignore rules did not match `Secret.env` against `secret.env` on a file
  system that takes them for one file, where Git does; merging a tag gave
  another message than Git's, and another id.

The last two reviewers of the round, with random testers again (about
290,000 steps between them), found:

- **Beside Git, `nv restore <dir> before-sync` left Git on the commits
  the sync had brought** until the next command of nv's, and a plain
  `git commit` in between recorded the undoing of another writer's work,
  which the next push published. Git goes with nv at once now.
- **Beside Git, a sync killed while it brought Git along left Git's
  index on the old tree** with every command saying all was in step; a
  plain `git commit` then reverted what had arrived. Git is brought along
  in an order a kill cannot break: objects, then the index, then `HEAD`.
- **A push that landed without the workspace hearing so (killed) made
  the next commit of the same file a conflict with itself.** A
  workspace's commits are now known on the ref by their message and
  their changes, wherever the server put them.
- A restore wrote half its files before finding a tracked directory it
  could not go into; a watcher missed a directory moved away and another
  made in its place; `nv git status` changed what `nv status` listed; a
  file put back with a sync's markers in it could be committed; a
  checkout went through a link to a directory; a workspace made by
  `nv git clone` had nowhere to set its work aside to.

Those are fixed, each with a test that failed first. The concurrency,
kill and store results of the rounds before held again: no lost content
in 27,000 restores, no panic, no hang, no store damaged, every `verify`
clean.

In the tenth round the reviewer sending crafted input found the ninth's
checks holding, name by name, and went around them:

- **Two names a file system takes for one let a checkout write outside
  its directory.** A tree with a link `Link` to `../outside` and a file
  `link/pwned` is two paths to Git; on macOS the directory `link` made
  for the file became the link, and the file was written through it. The
  server now refuses a commit or a tree with names that differ only in
  case or in how their characters are composed (the directories on the
  way included), and a client on a file system that takes them for one
  refuses to check out or sync such a tree.
- **A path longer than a file system takes, or a tree 20,000 directories
  deep, made a ref nobody could sync**, the deep one by overflowing the
  client's stack. Paths are held to 1,024 bytes on both sides, and the
  depth of what is handed over with them.
- **A directory's id, taken over from another repository, could be given
  to a file**, and the ref could not be synced again. A server that went
  silent on a connection kept from an earlier request was waited for
  until the command's own limit; each request now has a connection of
  its own. A client took a ref that went backwards, and a merge, from a
  server: it refuses both.

The reviewer of recording, with 180,000 steps of random testing (no lost
content in 24,000 restores, nothing written outside, no store damaged):

- **One file reported under two spellings (`a` and `A`, on a file system
  that takes them for one) recorded a state with the same path twice**,
  which nothing could read again: not `since`, not `commit`, not
  `restore base`. A watcher seeing a rename that changes only the case,
  and an edit, did the same. Reported paths are taken once each, as the
  file system has them.
- **A link to a pipe made every walk wait for ever**: checking whether a
  file could be read opened what the link led to. A link is read as a
  link and nothing more.
- Before the first commit, `nv git status` changed what a later commit
  published; a restore wrote half its files before finding a directory
  it could list and not empty; a pipe nobody tracked was replaced by a
  file; the snapshot made before the first commit could not be gone back
  to; a fork that failed left a copy that was not a workspace.

The reviewer of many writers (55,000 steps, 1,500 kills; nothing lost
under concurrency, no duplicate, no panic) found the worst thing of the
round, and it was a fix of the eighth's:

- **After `nv git status`, a command of nv's killed in its first
  milliseconds made what was not committed the workspace's commit**, and
  the next push published it, as a commit called "snapshot". Taking the
  snapshots back up after a look wrote where the workspace stood twice,
  and the first write said the snapshots were the last commit. It is one
  write now.
- A push that landed unheard, of a commit a sync had put on another's
  tip, still made the next commit of those lines a conflict with itself:
  a commit the server took as it was was not counted as the workspace's
  own. A file with a sync's markers in it, committed with `nv git commit`
  after a restore, was pushed; a push refuses it now.

The reviewer beside Git and of nv's own Git commands (32,000 compared
steps, 790 kills of publishing) found nothing lost, written outside,
hung or damaged, and:

- **Beside Git, a hook that staged a file of its own, while the user had
  something else staged, left Git's index without that file**, and the
  user's next `git commit` removed it from history. After a commit, every
  path the commit changed is brought up in Git's index, not only those
  nv named.
- **Beside Git, a file the user had removed from Git (`git rm --cached`)
  was put back by `nv commit --recorded`** when nv had once seen it go
  and come back. Its removal is the user's.
- `nv git add` of a file removed with `git rm` and made again as it was
  did not take the removal back, and the next `commit -a` removed it; in
  a merge that stopped, `reset`, `restore --staged`, `rm` and `mv` of the
  conflicted file did not do what Git does (`mv` is now refused as Git
  refuses it; the others end the merge or settle the path as Git does).

In the eleventh round the reviewer sending crafted input went around the
tenth's checks again, and wrote outside a directory once more:

- **Names APFS takes for one and lower case alone does not, let a
  checkout write outside its directory**: a link `ss` to `../outside` and
  a file `ß/pwned`; the same with `s` and `ſ`, `fi` and `ﬁ`, `σ` and `ς`,
  and a dozen more. The tenth round's fix folded names as NFC and lower
  case; both sides now fold case as Unicode does in full, as APFS does
  (lower, upper and lower again), and take more pairs for one where in
  doubt, never fewer.
- **Names that collide across two commits, two pushes, or a commit and a
  directory set whole were taken by the server**, which only looked
  within one commit, and every macOS client was then refused the ref.
  The server now keeps the folded spelling of every path on each ref,
  the directories on the way and the entries of directories set whole
  included, and refuses a push that would give a path a second spelling.
  The honest form of this is a writer on Linux committing `Readme` one
  day and `README` the next.
- **A directory whose files were never sent, a file entry holding a
  directory's id, and paths too long once the place a directory is set
  at, or several handings-over, are counted, were taken by the server.**
  It now keeps, for each directory handed over, what is in it and the
  longest path below it, and refuses a push setting a directory that is
  not whole or would make a path too long.
- **A client checked length and depth only the first time it saw a
  commit**: the second sync of a 20,000-deep tree overflowed its stack,
  and a path that fits 1,024 bytes but not below where the workspace is
  left it half-written. Both are checked at every arrival, against what
  is left of 1,024 bytes once the workspace's own place is counted,
  before anything is written.
- **The answer to a push could move the ref backwards**, and the client
  removed another writer's file to follow it. The forward-only check of a
  sync is made of a push's answer as well. And an answer is read to at
  most 1,024 MB (`NV_ANSWER_MB`): a server that never stopped sending
  took the client's memory with it.

The server's checks are tested by `server/test/hostile.ts`; the client's,
against a server that lies (it passes requests to the real one and
changes the answers), by `tests/lying_server.rs`.

Publishing beside Git was measured by a reviewer at 3 to 14 times
`git commit`, on a clone with no commit-graph: taking Git's new commit in
asked Git for "what is behind this and not behind that", which without a
commit-graph is a walk of all history. A commit made on the commit
already known is now asked for by itself.

The link check costs a look at each directory: `--tracked` on oxc takes
19.3 ms where it took 18.1.

Two of the fixes cost speed: a fork looks over its parent first (277 ms
on oxc where it was 227), and a restore hashes the ready copy of each
file it writes and looks at what stands in the way (5.6 ms for two files
where it was 5.0).

What the eleventh round found and is now fixed, each with a test that
failed first:

- **A `nv snapshot` killed after it moved the workspace's state and before
  it logged it left a state nothing named**, and the next `nv restore`
  removed a file's only copy. A state is now logged before anything names
  it, everywhere a state is made, and a record is one write: a kill no
  longer tears a line into the next, and a torn line names nothing.
- **`nv git merge --abort` threw away an unstaged edit to a file the merge
  did not touch.** It is Git's `reset --merge` now: the files the merge
  wrote go back, the others keep what was done to them.
- **Finishing a conflicting sync with `--amend` amended the other writer's
  commit**, and a later sync undid a third writer's change. Refused, as
  Git refuses an amend in a merge that stopped.
- **A killed `nv git commit --amend` left the branch on the commit it
  replaced**, and every commit after was refused ("another workspace
  moved it"). The next command finishes the move, as it already did for a
  plain commit.
- `nv commit` opened what a link to a pipe led to, and waited for ever; it
  reads only regular files, and a link as a link.
- Beside Git, `git mv` and `nv commit --tracked` published half the move;
  the paths Git has staged are now looked at by `--tracked`, as
  `git commit -a` commits them. A lock Git left behind (`index.lock`,
  `HEAD.lock`, the branch's) is found before anything is written, where
  `nv sync` used to write the files and fail at the end, and `nv commit`
  wrote Git's index over the lock. A Git commit stopped by
  `NV_GIT_TIMEOUT` no longer leaves its locks behind.
- `nv record . a ../zz` refused `../zz` and had noted `a`; it notes all or
  none. A push refused over a sync's markers in a commit now says what
  takes them out of the chain (a commit on top does not). A restore that
  could not read a file it should have recorded no longer says it is
  done. A change to `.nv/exclude` (or Git's `info/exclude`) is seen by
  `--recorded` without being reported.
- `nv git`: `reset <path>` in a stopped merge, a merge refused over a
  local edit (nothing on stdout, exit 2), a fast-forward over a staged
  file it does not touch, `restore --staged` before the first commit,
  `rm` of a tracked link, "Initial commit", `show` of a merge, and
  `reset --hard` before the first commit now do what Git does (case 33).

**A model of pushing and syncing** (`formal/sync`, checked with
[idealis](https://github.com/mizchi/idealis)) follows the client and the server as they are
written, kills a workspace while it waits for an answer, and writes lines back to what they
were. Its counterexamples, each then reproduced with nv against the server:

- **Work after a push that landed unheard was merged on the wrong tree.** A line changed back
  after that push was silently taken from the ref (the push's own version of it) when another
  writer had changed the file elsewhere, and a line changed again was a conflict with itself.
  It is merged now on what the workspace's landed commits left.
- **Going back past a push that may have landed made its commits again**: `restore pushed`
  set aside as "what the ref does not have" a commit the ref had, `restore before-sync` undid
  a sync whose result was pushed, and an amend made the pushed commit again. Each made the
  next sync a conflict between two versions of the workspace's own. A push leaves a note from
  before it is sent until its answer is heard or a sync finds its commits on the ref, and
  those three are refused while it is there.
- Found beside it: **amending a commit that was pushed and heard** did the same. A ref only
  goes forward, and the amend is refused.

**Tuning on Linux.** Measured on oxc (17,002 files, 22,530 commits) on a
4-core Linux VM (ext4, Git 2.43), each row the old binary and the new one
back to back, medians of 20 runs, with Git beside them in the same session:

| | before | after | Git |
| --- | ---: | ---: | ---: |
| `nv status` (beside Git) / `git status` | 36.3 ms | 34.9 ms | 52.1 ms |
| `nv git status` (no Git beside it) | 48.5 ms | 39.9 ms | 52.1 ms |
| `nv git diff --stat` | 29.2 ms | 19.7 ms | 14.2 ms |
| `nv git commit -qam` | 36.4 ms | 22.2 ms | 36.9 ms |
| `nv git switch` there and back, 52 files | 101.3 ms | 89.6 ms | 81.6 ms |
| `nv git log -20 --oneline` | 7.7 ms | 3.1 ms | 2.5 ms |
| `nv snapshot --recorded` | 10.4 ms | 4.0 ms | |
| `nv snapshot --tracked` | 29.1 ms | 18.9 ms | |
| `nv mark` / `nv since` with `--recorded` | 11.3 / 9.9 ms | 4.8 / 4.7 ms | |
| `nv commit` beside Git, `--recorded` | 81.1 ms | 78.8 ms | 36.9 ms (`commit -qam`) |
| `nv commit` beside Git, `--tracked` | 144.9 ms | 105.9 ms | |
| `nv commit` beside Git, walking | 172.9 ms | 130.0 ms | |
| first `nv status` after `nv fork` | 121 ms, 17,002 files read | 63 ms, none read | |

What changed, found with callgrind: opening the store read every commit's
root into a hash map, which was 90% of a `rev-parse`; roots are now in a
sorted, mapped `roots.idx` as Git ids are in `git.idx`. Maps keyed by ids
hash the id's first eight bytes, where SipHash cost more than the lookups.
Listing the index merged its parts through a map of every path, and comparing
a file with it allocated its path; both are gone. A commit looked for a
sync's markers with a look of its own, so `--tracked` and walking looked at
the tree twice; it now looks in what it recorded. A copy made where there is
no clone kept nothing of when files were written, and every file was read
again after a fork. Publishing beside Git is still twice `git commit`: Git's
index (2 MB for oxc) is written twice, by `git add` and by Git's commit,
where `git commit -a` writes it once.

What the rounds agree on, measured three times by different hands on a
machine busy with the other reviewers: recording is about three times
quicker than Git's quickest commit; going back is five to six times
quicker; eight writers to one branch are nine to thirteen times quicker;
publishing a commit beside Git was two to three times slower than
`git commit` (it is three to four times now: see "Not done").

What nv does that Git does not, or only with a script around it: going
back in one command that saves the state left behind first; snapshots
that never reach history; recording that costs what was written, not the
tree; a server that puts non-overlapping pushes on the tip itself; a
workspace of one directory with nothing else on disk; forks that carry
build output; a directory of another repository standing in this one as
an ordinary directory, by its id.

**The twelfth round** was given the build that added server-side union
merges, `nv note` and `nv publish`, and the new `nv git` paths. Four
reviewers: one with the server's address alone, sending what a client
never would (about 15,000 requests); three writing random testers with an
oracle of their own: many writers on one ref with kills and lost answers
(8,666 steps, 453 kills); a developer that never commits, with
`nv publish` and kills (2,520 steps, 523 kills); and `nv git` against
real Git in a twin directory (95,411 steps). What they found, each fixed
with a test or a Git case that failed first, most severe first:

- **A sync killed while it wrote files, then finished by the next
  command, wrote over what was edited after the kill**: 7 losses in 86
  killed syncs, the edit in no recorded state. A file that is neither what
  the workspace had nor what arrives is now merged with what arrives.
  Killed later, after the workspace stood on the new commit, a clashing
  sync forgot the file it left with both versions, and the next push went
  through with the markers: what the sync knows is now written first.
- **`nv git mv f ..` (and `mv f .` at the top) moved the file to the root
  of the file system**, and `mv . ..` a whole directory. A name moved into
  the top is the name alone; a destination outside the workspace is
  refused (case 38).
- **`reset --hard` removed a file `rm --cached` had let go of**, edits and
  all; `restore -- b d` with `d` a staged removal put `b` back over an
  edit; a file staged under a directory then ignored, or put back to the
  commit after `add`, was shown as gone and left out of the next commit;
  `mv` onto a tracked file already gone dropped the files moved from the
  next commit; `add -A <path>` staged the whole tree; `rm --cached` and
  `.gitignore` did not stop a file being tracked (cases 39 to 41).
- **A commit that fails its conditions landed marked as passing**: the
  runner kept the conditions it started with when the writer changed them.
  A run reads them again once its state is recorded.
- **Provisional messages reached the ref**: `nv commit <dir> <message>`
  dropped the message where the runner had committed first, and `nv push`,
  which nv's refusals named, pushed `checkpoint:` commits as they were.
  The runner's commit takes the message; under `[commit] each`, a push
  refuses provisional messages.
- **Notes were lost**: 110 of 184 reached no commit. A note now waits for
  the next commit the conditions pass on, whatever happens between; notes
  waiting together are one line joined by `; `; and commits a sync makes
  one keep all their lines.
- **A publish killed after its push landed left every later publish
  refused**, and `nv sync` said nothing came. A sync that finds its push on
  the ref says so; a publish after an unheard push syncs first.
- **The server**: one 2 MB push of 40,000 paths took 55 s and stalled
  every repository (a delete that read the whole table for each path: now
  4 s); a fetch answered every repeat of an id with another copy (419 MB
  for 26 KB asked); one commit could set `l` as a link and `l/x` as a file,
  which a client taking changes in order writes through; a change's kind
  byte was read as true or false, so one commit had 255 ids; names NTFS
  reads as `.git`, as a parent or as one another were taken
  (`.git::$INDEX_ALLOCATION`, `..\x`, `a` beside `a.`); a push sent again
  was told it had not been behind.
- Counts: "caught up 0 commits" where a fork's shared store had them;
  "0 of linked directories" where there are no links.

Not fixed, and in `TODO.md`: what the twelfth round found of `nv git`'s
output beside Git's (rename pairing and score, the order of mode lines in
a rename, `--stat` names, quoting, several stderr wordings, `mv` and
refusals that differ either way), a push the server merged not warning of
a message written before the commits it now sits on, and the reviewers'
timings: on a generated tree of 20,000 files, under other load, `nv git
status` took 1.3 to 1.75 times `git status`, where on oxc it is ahead, and
a history with one commit of 20,000 files makes every full `nv git log`
take 33 ms against Git's 2.1.

**The thirteenth round** was given the build that added Git's verbs toward
a server's ref (`fetch`, `pull --rebase`, `rebase --continue`), commits put
on the tip one by one and settled into the writer's own commit, merge
drivers and `tools/merge-cst`. Four reviewers: one with the server's
address alone (about 20,000 requests); three writing random testers with
an oracle of their own: three to six writers on one ref through `nv git`,
with kills and answers dropped after the server acted (3,340 steps, 108
kills, 133 answers lost); `nv git` against real Git in a twin directory
(17,686 steps); and drivers, against Git merging the same histories and
against properties `merge_cst.py` must keep (4,720 twin merges, 571
syncs, 3,200 random merges of its own). What they found, each fixed with
a test or a Git case that failed first, most severe first:

- **A file staged before `git pull --rebase` kept its staged copy through
  the pull, and the next commit took the other writer's line back out of
  the ref.** The pull is refused as Git refuses it; `nv sync` refuses
  where the ref changed a staged path. At a pull that stopped, a file
  settled and added but not committed let a second pull or a push go
  through, dropping the writer's commit or landing it without its change;
  undoing the pull kept what was added during it staged, and a plain commit
  then committed the other writer's line as this writer's change. Nothing
  moves now until the commit is completed or the pull undone, and undoing
  it gives back the index as it was.
- **A commit's header was taken as any bytes.** A `parent` line there gave
  every client's Git history a parent the ref did not have (a merge on a
  ref of one line, or a parent nobody had), and an empty one made the ref
  impossible to export. Both sides hold it to an author line first and no
  `tree`, `parent` or NUL in the header.
- **A link to 5,000 bytes, to a NUL or to nothing, was taken**, and no
  client could sync the ref, or push the fix, again. **An empty file
  could not be fetched by itself**, so a sync of a ref holding one failed
  for everyone.
- **`merge_cst.py` called merges clean that were not.** A unit put after
  one with no line end of its own ran into it (`# note` and `b = 2` became
  one comment); a list one side removed went with the items the other put
  in it. A clean result now has to read back as the units merged. Of the
  reviewer's 1,600 random merges, 128 broke a property before and 8 do now,
  all of them a unit moved between sections; 16,000 units put in at one
  place take 0.3 s where they took 16.
- **`.gitattributes` macros were not read**, so the server merged by union
  a path Git gives a driver. **The server's union merge had no bound**: one
  push of 200,000 reordered lines held it for seconds and the watchdog
  restarted it; and a dozen union pushes at once had half refused.
- **A merge driver that never returned went on after `NV_TIMEOUT` stopped
  the sync**, and wrote into the workspace without its lock. One killed by
  a signal had its half-written result taken as the conflict; one that
  exited 0 and left markers had them committed. A file name with a newline
  broke the notes of what a sync left unsettled, and a push went through
  with the markers on disk.
- **`git fetch` left `log origin/<ref>` unable to name the commits it
  brought**, and after a push never heard back left origin behind the tip.
  `nv restore` over an edit a push had recorded did not say where it was
  kept. A union push sent again was not told what was merged; `after` took
  `0x1` and an empty value.
- **`nv git` beside Git**: `merge origin/dev` made another commit than Git
  ("Merge remote-tracking branch"); annotated tags with a blank message or
  runs of empty lines were other objects; `pull` on a branch that follows
  nothing merged anyway; `checkout <branch>` of a branch only origin has
  was taken for a path; the empty tree was unknown; and revisions nv does
  not look for (`HEAD@{0}`, `HEAD^@`, `:/text`, `origin`) were answered with
  Git's error for a name that is nothing, not refused (case 48).

Not fixed, and in `TODO.md`: the rest of what the Git reviewer found of
output and exit codes, a failed `merge` that leaves a staged file deleted
where Git writes it back, places Git defines a driver that nv does not read
(`include.path`, `GIT_CONFIG_COUNT`, `~/.config/git/attributes`), a ref
whose first commit has no rules taking the rules a union push states, ref
names Git cannot hold, and the reviewer's timings: on 5,000 files and 43
commits, a full `nv git log` takes 16 times Git's, `ls-tree -r` 8 times
and a diff of two revisions 4 times (`status`, `add`, `commit` and `fetch`
are at parity or ahead).

The same four were given the build with those fixes, told what each
changed, and asked to break them (about 15,000 more steps and 12,000
requests). Every finding of the first pass held but those in `TODO.md`.
What the second pass found, fixed the same way:

- **What was added at a stopped pull to a file a later commit of the
  writer's also changed was lost** under that commit's version, and a
  catching-up after a push that unstaged a file lost what was staged of it
  and not on disk. Others added at a stop go on the last commit; what is
  unstaged is kept as a state, and named. A completion killed in its first
  milliseconds left the workspace naming a commit never written, which no
  command could open: it is stored before it is named. A branch left
  behind by such a kill is taken along; staging a completion spent is no
  reason to refuse a pull.
- **A link inside a directory set whole got past the check of a commit's
  own changes**, and the ref was again one nobody could sync: the server
  checks the links of a directory handed over, or their contents when they
  come, and a client opens such a directory for its links.
- **Headers Git's `fsck` refuses** (no committer, a date or zone that is not
  one, two authors, no email, no end) were taken, and a Git that checks
  what it is given refused the export. A server takes, and a client takes
  from one, only what `fsck` would.
- **A macro set on one line and unset by a later one still expanded**:
  attributes are filled as Git fills them, the line of highest priority
  first, each once. **The union merge was bounded by file and not by
  push**: sixteen files of 248 KiB held the server for 2.4 s; a push's
  merging is now bounded as a whole.
- **`merge_cst.py` called clean a unit run into the next under the same
  name** (`a = 1` and `@d` read as `a = 1@d`, and `f` lost its decorator),
  and Lean units with no line end between them: the read-back compares
  text, and Lean units that run together are a conflict. **A driver killed
  inside a shell** (an exit above 128) and **one that started a process in
  a session of its own** are stopped and give no result, as Git has it.
- **`nv git`**: tag messages given in several `-m`, or with lines of `#`,
  made other tags than Git's, and a tag of a tree said it tagged a commit;
  merging a revision that is no branch's name made another commit ("Merge
  commit"); `checkout -b b origin/b` followed nothing, and pull was then
  refused; an amend refused in a stopped merge staged the markers first;
  `tags/<t>` and `remotes/<r>/<b>` were not read; and a tree or a file named
  where a commit is meant read as a damaged store (case 49).

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

## How long nv waits

Nothing nv does waits without end. Each wait has a limit, set by the
environment (`0` is no limit), and when one is reached nv says what it was
waiting for and stops; a command stopped part-way leaves what was, or what
was meant, as one killed at any moment does.

| What | Limit | Set by | When it is reached |
| --- | --- | --- | --- |
| A lock another nv command holds (the workspace, the store, a commit still being made beside Git) | 60 s | `NV_LOCK_TIMEOUT` | exit 2, naming the lock; the command does not go ahead without it |
| A server: making the connection / a request with no byte moving | 10 s / 60 s | `NV_HTTP_TIMEOUT` | the request fails, and the command with it |
| A commit Git makes for `nv commit` beside it, its hooks included | 300 s | `NV_GIT_TIMEOUT` | Git and everything it started are stopped; Git's index is as it was |
| A merge driver on one file | 120 s | `NV_MERGE_DRIVER_TIMEOUT` | the driver and everything it started, in its process group or a session of its own, are stopped; nothing of the merge is taken from it |
| One command, all of it | 1,800 s | `NV_TIMEOUT` | exit 124, as `timeout(1)` gives |

A watcher (`nv watch <dir>`) is no command but a process that stays, and
has no limit. A hook's write is noted before the hook waits for the
workspace, so a hook that gives up waiting loses nothing: the next snapshot
takes it in.

Until these limits were put in, a command that found the workspace's lock
held went on without it once its wait failed: two writers at once. It now
stops.

## Not done

- **Found by the eleventh round and not fixed yet**, each a way to
  publish the wrong thing in ordinary use, on a file system that folds
  case (none was at hand where the rest were fixed):
  - a directory renamed only in case (`d` to `D`) is recorded beside the
    old spelling;
  - `git rm way` then `nv git add WAY` stages nothing.

- The server checks a push against the spellings of the ref's paths it
  has kept since those checks were added: a ref older than them was not
  gone over. It knows the entries of a directory set whole one level
  down, not all the way. A server that sends one byte a second is cut
  off only by the command's own limit (30 minutes).

- Publishing a commit beside Git is two to four times slower than
  `git commit`. On oxc as it is now (17,002 files, 22,530 commits; a
  4-core Linux VM, ext4, Git 2.43; medians of 25 runs), `nv commit
  --recorded` takes 90 to 94 ms, `--tracked` 150, where `git commit -qam`
  takes 39. Until the eleventh round's fixes `--recorded` took 143 to 147
  ms there: looking for a sync's markers before publishing walked the whole
  tree, whatever the look, and now it looks where the commit does. Taking
  Git's commit in asks Git once where it asked twice. `--tracked` still
  looks at every tracked file twice, once for the markers and once to
  record. What makes a killed or refused publishing leave Git's index alone
  costs the rest: a look at what the user has staged, an index of nv's
  own, a process of its own for the commit; six Git commands are run where
  `git commit` is one. `git status` with fsmonitor and the untracked cache
  is quicker than `nv status`, which has no equivalent of either.
- After a push that was not heard back from, `restore pushed`, `restore before-sync` and
  `nv git commit --amend` are refused until the workspace knows where it stands: a push that
  is answered, or a sync that finds the push's commits on the ref. A push that never reached
  the server is known only by pushing again.
- The model in `formal/sync` is checked for two writers, one file of two lines and a ref of
  three commits. A bug that needs more than that is not ruled out by it.
- Two writers who each add a file whose names differ only in case both
  land; on a file system that does not tell the names apart, a workspace
  then has one file on disk for the two.
- The server trusts anyone who can reach it. It checks that contents hash to
  their names, that a commit decodes, names only paths a tree may have and
  only contents that are there, not who is writing. Anyone can upload up to
  the size limit, as often as they like. Removing `.nv/conflicts` by hand
  lets a file with both versions in it be pushed.
- The store is one segment. Bytes cut from its end take the commits written
  last with them, and every mark that named them: nothing wrong is
  restored, and nothing is recovered either. There is no second copy.
- Dates in `GIT_AUTHOR_DATE` are read as ISO 8601 or as seconds and a zone,
  not as RFC 2822.
- `nv export` of 20,876 commits to a bare Git repository took 185 s.
- A file of 4 GiB or more is refused. Recording a large file holds several
  copies of it in memory (6 GB for a file of 1.5 GB), and a byte changed in
  one stores it whole again.
- `roots.map`, `git.map` and the two index files of the store are derived
  and are read without a checksum. `nv verify` recomputes every commit's
  Git id and so finds a changed bit that changes one; a reviewer flipped
  bits in `roots.map` and `git.map` 130 times and `verify` passed 109 of
  them, without a wrong checkout or a wrong id coming of any. A whole
  tree's ready copy is cloned without being read; `nv verify` reads it,
  and the index kept with it.
- A file under an `eol` or `text` rule of `.gitattributes` is recorded as
  the rule says it is stored and comes back as the rule says it is
  written: `mark` and `restore` can change its line endings, as Git's
  stash does, and nothing says so.
- Beside Git: a rename that changes only the case of a name (`git mv a.txt
  A.txt`) is not published; a file Git no longer tracks and that its rules
  leave out is not published, and its removal stays staged for the user.
- In nv's own Git commands: a file replaced by a link shows as modified,
  where Git says its type changed; `switch`, `reset --hard` and `merge`
  killed half-way leave some files of the other commit (the branch has
  not moved, and `reset --hard` puts them back); `status` with 30,000
  untracked files in one directory takes 0.9 s and 320 MB where Git takes
  10 ms; writing 30,000 files in a switch takes three times as long as
  Git.
- `nv commit <dir> <message>` takes no message that begins with `-`.
- A merge commit made with `nv git merge` cannot be pushed to a server: a
  ref is one line of commits.
- `nv checkout` writes into an empty directory or a new one, never over
  files: there is no checkout "in place".
- `nv git rm` and `nv git mv` refuse a path beyond a link (Git, forced,
  removes and moves what the link leads to). `nv git switch -c` and
  `reset --soft` refuse in the middle of a merge, as Git does.
- `nv status` lists what no snapshot has yet, not what differs from the
  last commit; `nv since <dir> base` lists that.
- A server refuses names that differ only in case or composition, on any
  platform: a repository with `README` and `readme` cannot be pushed,
  though Git on Linux holds it. nv checks a tree for such names once,
  the first time it is checked out on a file system that folds them.
- Each request to a server is made on a new connection.
- A ref started by a push of a commit its other writers do not have (an
  amended first commit) cannot be synced by them: "the ref's history does
  not reach any commit of this workspace". The push is not refused.
- A repository can take over another's contents by naming their ids when
  it is handed a directory (`adopt/trees?from=`): with no authentication
  there is no telling whose they were. This is the linking of a directory
  of another repository, and it asks no permission.
- Names that differ only in case, in nv's own Git commands on a file
  system that does not tell them apart: `nv git add Src/Lib` takes the
  name as typed where Git takes nothing, and a rename that changes only
  the case, then `reset --soft`, is published as a removal. A file
  replaced by a directory is listed as `?? f/`, where Git lists nothing.
- A command killed while it writes where the workspace stands can leave a
  temporary file in `.nv` (one at most of each kind).
- Found by the ninth round and not yet fixed: restoring the state that
  `before-sync` kept, which stands on commits the workspace is no longer
  on, brings other writers' changes back as this workspace's own edits; a
  sync can write back, with markers, a file the workspace renamed and
  has not committed; a push that moves commits onto a newer tip can
  report having caught up when it has not (the next sync does); a
  reviewer once saw a killed push leave an uncommitted edit as a commit,
  which 500 tries on a later build did not make happen again.
- `nv git add -A; nv git commit` commits a file that still holds the
  markers of a sync's conflict (`nv commit` refuses; so do push and sync
  until they are gone).
- Workspaces that share a store (a fork of an `nv checkout` workspace)
  share its branch names: a branch one of them moves is not one the other
  can commit on, and `nv git commit` there says so.
- In nv's own Git commands: `restore --staged` of one side of a file that
  became a directory leaves the other side staged; `mv` onto an untracked
  link replaces the link; a file added and then matched by a new ignore
  rule is dropped by `commit -a`; `reset --hard` refuses where an
  untracked file stands in the place of a tracked directory (Git removes
  the file).
- Restoring a state after the workspace was reset to an older commit
  brings back, as changes of this workspace's own, files the commits
  reset away had; if someone has removed one since, the next push adds
  it again. `git checkout <state> -- .` after `git reset --hard` does the
  same.
- A watcher sees a change to `.gitignore`, and then every file is looked
  at; a change to `.nv/exclude` it does not see. A write through a second
  hard link to a tracked file was missed once in three tries.
- `nv fork` fails when the tree holds a file or directory that may not be
  read.
- A file rewritten with its size and time put back *while* a fork or a
  copy of the workspace is being made can go unseen in the copy.
- Going back over 10,000 changed files of 100,000 takes 3.5 s where
  `git checkout` takes 1.4; with 100,000 small files the store's ready
  copies take 517 MB where `.git` is 16 MB.
- A checkout killed half-way leaves a directory that is not a workspace,
  to be removed by hand.
- Beside Git, a submodule's pointer is not nv's to publish: after a commit
  inside a submodule `nv commit` says there is nothing to commit, and
  `git commit` is what records it.
- In a workspace with no Git beside it, `nv git` reads the user's Git
  configuration for who they are, what to leave out (`core.excludesFile`),
  line endings (`core.autocrlf`) and the first branch's name. It runs no
  hooks, expands no aliases, and `nv git config` shows only what was set
  through it.
- `nv git reset --hard <commit>` refuses where an untracked file stands in
  the way of one the commit has; Git writes over it. Some of Git's hint
  lines and a few messages are worded differently (the exit codes are
  Git's); `nv git add` takes no patterns.
- `~N` counts recorded states, not restores: a restore is undone with the
  id it printed, not with `~1`. Taking a linked
  directory's contents over from another repository asks no permission
  either.
- The server keeps contents uncompressed: a pack is the contents of a push
  end to end, the delta chains of the local store do not exist there, and
  nothing is ever collected. Its check of a large upload has only been
  timed locally, and a worker's limits on time and memory have not been
  met by a real file of a gigabyte. Nothing yet fetches a large file only
  when it is opened.
- A workspace with several unpushed commits that has to catch up gets them
  put on the ref's tip one by one, each with its message, where each file of
  each merges as a sync merges and the last comes to the same files as one
  commit would; a file left with both versions belongs to the last commit
  that changed it, and its settling goes into that commit, those after it
  made again on top. Those before it that changed the file are held out of
  it too, and when it is settled each is given its own version with what
  the settling brought in (three-way: from the last commit's version, to
  its own and to the settled one), and the same trailer, as a rebase
  stopped at each would have them; where that does not merge clean, the
  commit keeps the tip's version and nothing says so. A commit that
  changed nothing but such files is on the tip with nothing in it until
  then; where its version does not merge clean, it is one commit with the
  one after it, under the lines of both. While such a commit waits, the
  settling cannot be made a commit of its own (`nv commit <dir>
  <message>` is refused and says to give no message). Where none of this
  can be (a commit clashes with the ref on the way though the commits
  together do not; a directory set whole), they are one commit,
  under the lines of all of them (their summary lines joined by `; `, their
  bodies one after another), and the sync says so. The model in
  `formal/sync` folds them always: it does not cover the commits put on the
  tip one by one.
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
  endings, `merge=union` and merge drivers only: no filters, no LFS, no
  `-merge` (it is merged as text). Beside Git, commits are Git's and
  filters apply to them; nv's own snapshots hold the files as they are on
  disk.
- `.gitignore` is honoured, and `.nv/exclude` is a workspace's own list in
  the same form. Nothing is skipped by name, so a workspace without rules
  sees its `target` and `node_modules`.
- Nothing searches the snapshots for the one that broke a test.
- The watcher has been run on macOS, and on Linux by the tests only.
  Copy-on-write clones are APFS's, and Btrfs's and XFS's through
  `cp --reflink=auto`: on another file system (ext4) a checkout or fork
  copies, keeping the times files were written, so that the index that
  comes with them still vouches for them. A store inside the workspace (as
  `nv git clone` makes one) is copied with a fork: 390 MB for oxc, 10 s on ext4.
- A tree whose entries are not in Git's order, or whose directory modes are
  written with a leading zero, would not round-trip. File modes Git no
  longer writes do. None of the former turned up in 271,812 commits.

## Building and testing

```
cargo test
cargo build --release   # target/release/nv
pkf run ci              # fmt, clippy, tests
cargo test --profile fast   # while working on nv
```

The release profile is built with fat LTO and one codegen unit, and a change
to one file takes a minute to rebuild. The `fast` profile is optimized but
not linked whole, and rebuilds in seconds; the tests pass on it as on
release. Numbers in this file are measured with release builds only.

The end-to-end tests build a Git repository with `git`, so `git` has to be on
the path. Some tests make files unreadable with `chmod 000`, which does not
stop root: run them as another user.

On Linux (ext4, Git 2.43, the tests run as an unprivileged user and against
a local server) everything passes but case 27 of the Git cases (Git 2.43
takes `log -n abc`, newer Git refuses it), and the check that a hook's
process is gone after it was stopped: it can still be seen there for a
moment until it is reaped, and for ever in a container whose first process
reaps nothing (the hook's `sleep` is killed, and stays a zombie of it).

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
