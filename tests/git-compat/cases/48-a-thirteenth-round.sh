# What the thirteenth round found of `nv git` beside real Git in a twin directory.
"$REAL_GIT" init -q --bare -b main origin.git
"$REAL_GIT" clone -q origin.git seed 2>/dev/null && cd seed
printf 'alpha\n' > a.txt && "$REAL_GIT" add a.txt && "$REAL_GIT" commit -q -m initial && "$REAL_GIT" push -q origin main
"$REAL_GIT" checkout -q -b dev && printf 'on dev\n' > d.txt && "$REAL_GIT" add d.txt && "$REAL_GIT" commit -q -m 'dev work' && "$REAL_GIT" push -q origin dev
cd .. && git clone -q origin.git work && cd work
echo "--- merging a branch of origin's is merging a remote-tracking branch"
git merge -q --no-ff --no-edit origin/dev; echo "exit: $?"
git log -1 --format='%H %s'
echo "--- a branch only origin has, checked out"
git checkout -q main~0 2>/dev/null; git checkout -q main
git branch -D dev >/dev/null 2>&1
git checkout dev; echo "exit: $?"
git log -1 --format='%H %s'
git checkout -q main
echo "--- a branch that follows nothing is not pulled"
git checkout -q -b lone
git pull; echo "exit: $?"
git log -1 --format=%H
git checkout -q main
echo "--- tag messages are cleaned up as commit messages are"
git tag -a t1 -m ' '; echo "exit: $?"
git tag -a t2 -m ''; echo "exit: $?"
git tag -a t3 -m 'l1



l2

'; echo "exit: $?"
for t in t1 t2 t3; do git rev-parse "$t" 2>/dev/null; git cat-file -p "$t" 2>/dev/null | tail -n +5; done
echo "--- the empty tree"
git rm -q -r . && git commit -q -m empty
git cat-file -t 'HEAD^{tree}'; git rev-parse 'HEAD^{tree}'
git cat-file -t 4b825dc642cb6eb9a060e54bf8d69288fbee4904; echo "exit: $?"
