# What an eleventh round of review found, against Git doing the same.
git init -q -b main repo && cd repo
printf '1\n2\n3\n' > c && echo k > k && git add -A && git commit -q -m base
git switch -q -c side && printf '1\nS\n3\n' > c && git commit -q -am side
git switch -q main && printf '1\nM\n3\n' > c && git commit -q -am main
# An unstaged edit to a file the merge does not touch outlives the abort.
echo 'my unsaved edit' > k
git merge -q side >/dev/null 2>&1; echo "merge: $?"
git merge --abort; echo "abort: $?"; cat k c; git status --porcelain
# A path a stopped merge left conflicted, reset: no longer unmerged, its edit kept.
git merge -q side >/dev/null 2>&1; echo "merge: $?"
git reset -q c; echo "reset c: $?"; git status --porcelain
git merge --abort; git status --porcelain
# A merge refused over an unstaged edit to a file it has to merge: nothing on stdout.
printf '1\nM\n3\nmine\n' > c
git merge side; echo "merge over an edit: $?"
git checkout -q -- c
# The same over a merge within the file that would have gone through.
git switch -q -c clean main && printf '1\nM\n3\n4\n5\n6\n7\n' > c && git commit -q -am clean
git switch -q -c clean-side && printf '1\nM\n3\n4\n5\n6\nS\n' > c && git commit -q -am clean-side
git switch -q clean && printf 'C\nM\n3\n4\n5\n6\n7\n' > c && git commit -q -am clean-ours
echo edit >> c
git merge -m 'm clean' clean-side; echo "clean merge over an edit: $?"
git checkout -q -- c && git switch -q main
# A fast-forward over a file staged that the other side does not touch.
git switch -q -c ahead && echo f > f && git add f && git commit -q -m f && git switch -q main
echo s > staged.txt && git add staged.txt
git merge -q ahead; echo "fast-forward with a file staged: $?"; git status --porcelain; git log --format=%s -1
git reset -q --hard
# A fast-forward over a staged file the move changes: refused, unless what is staged is what it brings.
git reset -q --hard HEAD~1 && echo staged > f && git add f
git merge ahead; echo "fast-forward over a staged change: $?"; git status --porcelain
git merge -q ahead; echo "the same, quiet: $?"
echo f > f && git add f && git merge -q ahead; echo "staged as it brings: $?"; git status --porcelain; git log --format=%s -1
git reset -q --hard
# A merge's commit, shown by name and status.
git merge -q --no-ff -m 'merged ahead' ahead 2>/dev/null; git reset -q --hard HEAD~1 2>/dev/null
git switch -q -c other main~1 && echo o > o && git add o && git commit -q -m o && git switch -q main
git merge -q -m 'merge other' other 2>&1; git show --name-status HEAD | tail -3
# A merge's commit with its stat: against its first parent.
git show --stat HEAD | tail -3; git show --stat --format=%s HEAD; git show --name-only --format=%s HEAD
# A tracked link removed.
ln -s c lc && git add lc && git commit -q -m link && git rm -q lc; echo "rm a link: $?"; git status --porcelain
git commit -q -m 'no link'
cd ..
# Before the first commit.
git init -q -b main unborn && cd unborn
git commit -m nothing; echo "commit with nothing: $?"
echo x > x && mkdir d && echo y > d/y && git add -A
git restore --staged x; echo "restore --staged x: $?"
git restore --staged .; echo "restore --staged .: $?"; git status --porcelain
git reset -q --hard; echo "reset --hard: $?"; ls; git status --porcelain
cd ..
# A switch over an edit that already is what the other branch has, and over an untracked file that is: refused.
git init -q -b main sw && cd sw
echo 1 > f && git add f && git commit -q -m a && git switch -q -c b && echo 2 > f && echo n > n && git add -A && git commit -q -m b
git switch -q main && echo 2 > f && git switch -q b; echo "an edit as the target has it: $?"
git checkout -q -- f && echo n > n && git switch -q b; echo "an untracked file as the target has it: $?"
