# A history Git made, with what nv does not make itself: a merge within one
# file, a rename with edits, an annotated tag, a binary file, an octopus merge.
#@ git
git init -q -b main . && git config user.name "Origin Author" && git config user.email origin@example.com
printf 'line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n' > shared.txt
printf '#!/bin/sh\necho run\n' > run.sh && chmod +x run.sh && ln -s run.sh link
mkdir -p src/deep && echo "fn a() {}" > src/deep/a.rs && printf '\000\001\002binary\377' > data.bin
git add -A && git commit -q -m "base"
git checkout -q -b left && sed -i.bak 's/line 1/line one/' shared.txt && rm shared.txt.bak && git commit -q -am "left edits the top"
git checkout -q main && sed -i.bak 's/line 8/line eight/' shared.txt && rm shared.txt.bak && git commit -q -am "main edits the bottom"
git merge -q left -m "merge within one file"
git mv src/deep/a.rs src/moved.rs && echo "fn b() {}" >> src/moved.rs && git commit -q -am "move and edit"
git tag -a v1 -m "an annotated tag"
git tag light HEAD~1
git checkout -q -b one HEAD~1 && echo 1 > one.txt && git add -A && git commit -q -m one
git checkout -q -b two HEAD~1 && echo 2 > two.txt && git add -A && git commit -q -m two
git checkout -q main && git merge -q one two -m "octopus"
#@ read
echo "head: $(git rev-parse HEAD) on $(git rev-parse --abbrev-ref HEAD)"
echo "--- log"; git log --format='%H %P | %an <%ae> | %s'
echo "--- tree"; git ls-tree -r HEAD
echo "--- the merge"; git cat-file -p HEAD
echo "--- tags"; git tag; echo "v1 $(git rev-parse v1^)"; echo "light $(git rev-parse light)"
echo "--- since the tag"; git diff --name-status light HEAD
echo "--- the move"; git show --format=%s HEAD~1
echo "--- the first commit"; git show --format='%s' HEAD~4
echo "--- files"; cat shared.txt; ls -l run.sh | cut -c1-10; readlink link
echo "--- status"; git status --porcelain; git status
git fsck --no-dangling; echo "fsck: $?"
#@ git
echo "later" > later.txt && rm data.bin && chmod -x run.sh && git add -A && git commit -q -m "after the clone"
git checkout -q -b elsewhere && echo e > e.txt && git add -A && git commit -q -m "on another branch" && git checkout -q main
git tag v2
#@ read
echo "head: $(git rev-parse HEAD)"
echo "--- log"; git log -n 3 --format='%h %s'
echo "--- what arrived"; git diff --name-status HEAD~1 HEAD
echo "--- as a patch"; git diff HEAD~1 HEAD
echo "--- tags"; git tag
echo "--- the other branch: $(git rev-parse origin/elsewhere)"
echo "--- files"; ls; ls -l run.sh | cut -c1-10
echo "--- status"; git status --porcelain
git fsck --no-dangling; echo "fsck: $?"
