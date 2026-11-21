# What a tenth reviewer's random tester found, against Git doing the same.
git init -q -b main repo && cd repo
echo b > base && echo x > f && git add -A && git commit -q -m b
# Removed with `git rm`, made again as it was, and added: the removal is gone.
git rm -q f && echo x > f && git add f && git status --porcelain
git commit -q -am again >/dev/null 2>&1; echo "commit -a: $?"; git ls-tree -r --name-only HEAD
# Commands in the middle of a merge that stopped.
printf '1\n2\n3\n' > c && git add c && git commit -q -m c
git switch -q -c side && printf '1\nside\n3\n' > c && git commit -q -am side
git switch -q main && printf '1\nmain\n3\n' > c && git commit -q -am main
git merge -q -m mg side >/dev/null 2>&1; echo "merge: $?"
git mv c c2 2>&1; echo "mv of a conflicted file: $?"; git status --porcelain
git restore --staged c; echo "restore --staged: $?"; git status --porcelain
git add c && git reset -q; echo "reset: $?"; git status --porcelain
printf '1\nboth\n3\n' > c && git add -A && git commit -q -m after; git log -1 --format='%s %P' | wc -w | tr -d ' '
git merge -q -m mg2 side >/dev/null 2>&1; git switch -q -c x 2>/dev/null; git merge --abort 2>/dev/null; true
