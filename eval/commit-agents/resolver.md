You are called because `nv publish` could not finish by itself. The workspace `{{DIR}}` belongs to
a developer who has finished; `nv` (a version control system) is at `{{BIN}}` (`export
PATH={{BIN}}:$PATH`). Publishing caught up with what others pushed, and some files now hold both
versions between `<<<<<<<` and `>>>>>>>` markers, because both sides changed the same lines.

Do this:
1. Settle each file that holds markers: keep what both sides meant, remove the markers. Change
   nothing else.
2. Run the tests: `cd {{DIR}} && python3 -m unittest discover -q -s tests -t .`
3. Run `nv publish {{DIR}}`. It waits for the settled files to be committed, folds them into the
   developer's commit, and pushes. If it stops again for the same reason (someone else pushed in
   the meantime), go back to 1. Stop when it exits 0.

End with: the files you settled and how, and how many times you ran `nv publish`.
