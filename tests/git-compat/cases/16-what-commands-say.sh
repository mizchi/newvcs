# The same commands without -q: what they report is part of what they do.
git init -q -b main repo && cd repo
printf 'one\ntwo\nthree\n' > a.txt; echo b > b.txt; printf '#!/bin/sh\n' > run.sh
git add -A
echo "--- first commit"; git commit -m "first"
printf 'one\n2\nthree\nfour\n' > a.txt; rm b.txt; chmod +x run.sh; echo new > n.txt; git mv run.sh tool.sh
git add -A
echo "--- a commit with everything in it"; git commit -m "second, with a longer subject line"
echo "--- nothing to commit"; git commit -m "none"; echo "exit: $?"
echo x >> a.txt
echo "--- nothing added"; git commit -m "none"; echo "exit: $?"
echo "--- commit -a"; git commit -am "third"
echo "--- amend"; git commit --amend -m "third, reworded"
echo "--- stat of a diff"; git diff --stat HEAD~2 HEAD
echo "--- stat of a show"; git show --stat --format=%s HEAD~1
git checkout -q -b fast && echo f > f.txt && mkdir -p deep/dir && echo d > deep/dir/with-a-long-name-for-the-column.txt && git add -A && git commit -q -m fast
git checkout -q main
echo "--- fast-forward"; git merge fast
git checkout -q -b side HEAD~1 && printf 'line\nline\nline\nline\nline\nline\nline\nline\nline\nline\nline\nline\n' > many.txt && git add -A && git commit -q -m side
git checkout -q main
echo "--- a real merge"; git merge side
echo "--- already there"; git merge side
echo y >> a.txt; echo z > z.txt; git add -A; echo more >> a.txt
echo "--- reset"; git reset
echo "--- reset --hard"; git reset --hard HEAD~1
echo "--- rm"; git rm n.txt
echo "--- branch and tag"; git branch topic; git tag v1; git branch -d topic; git tag -d v1
