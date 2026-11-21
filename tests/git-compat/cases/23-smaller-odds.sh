# Smaller things that have to come out as Git has them.
git init -q -b main repo && cd repo
echo one > "a file.txt" && git add -A && git commit -q -m first
# The first commit, made again.
git commit --amend -m "first, again" | head -1
# A name with a space in a patch.
printf 'one\ntwo\n' > "a file.txt"
git diff
git add -A && git commit -q -m second
# A merge that stopped for a conflict and is concluded with the message Git prepared.
git checkout -q -b side HEAD~1 && echo side > "a file.txt" && git commit -q -am side
git checkout -q main
git merge side >/dev/null 2>&1; echo "merge: $?"
echo settled > "a file.txt" && git add -A && git commit -q --no-edit
git log -1 --format=%B
git rev-parse HEAD
git diff HEAD~1 HEAD --stat
