You look after version control for a developer who never does any. The developer works in
`{{DIR}}`, a workspace of `nv`, a version control system. Use `nv` from `{{BIN}}`
(`export PATH={{BIN}}:$PATH`). `nv help` lists its commands; `nv help <command>` explains one.

How the workspace is set up:
- A watcher records every write. After each, the tests run in the background (`nv.toml`), and
  each state they pass on is committed automatically, under a provisional message
  (`checkpoint: unit passed`). These commits are meant to be made again before they are pushed.
- Several developers work like this at once, each in a workspace of their own, and all push to
  one ref on a server. Others may push before you.

Your job:
1. While the developer works, follow what is being committed: `nv pending {{DIR}}` lists the
   commits not pushed yet; `nv since`, `nv git show` and `nv git diff` show what changed. Do not
   edit the developer's files while they work.
2. The developer is done when the file `{{DONE}}` exists. Wait for it, for example with
   `until [ -e {{DONE}} ]; do sleep 10; done`, running that again if it stops before the file is
   there. Then wait until what is on disk is committed: the background run of the tests takes a
   moment after the last write.
3. Make the pending commits again as one to three commits that each make sense on their own,
   with good commit messages (a short summary line, a blank line, then what and why), using
   `nv rebuild`. End each new commit at a commit whose tests passed.
4. Push with `nv push {{DIR}}`. If another developer pushed changes to the same files first,
   the push is refused: `nv sync {{DIR}}` merges theirs in. If the sync leaves a file with both
   versions between `<<<<<<<` and `>>>>>>>` markers, settle it (keep what both sides meant),
   run the tests (`python3 -m unittest discover -q -s tests -t .` in `{{DIR}}`), commit the
   result with a message saying it is a merge, and push again.
5. Stop once your commits are on the ref (`nv git log` after a push shows them).

End your answer with a report on using nv for this, as concrete as you can make it:
1. The commits you pushed, as `nv git log --format='%h %s'` shows them.
2. Every command that failed, surprised you, or gave output you could not act on: the command,
   what it said, what you expected.
3. What you had to work out by trial, and what you wished nv did or said.
4. How long you spent waiting, and on what.
5. Whether any commit seemed wrong to you: one you were given (a checkpoint), one you made, or
   one as it landed on the ref (after a sync or the server's doing). What was wrong with it, and
   what you did about it. Say "none" if none did.
