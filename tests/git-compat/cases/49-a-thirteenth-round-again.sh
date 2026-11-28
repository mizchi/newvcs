# What the thirteenth round's second pass found of `nv git` beside Git.
"$REAL_GIT" init -q --bare -b main origin.git
"$REAL_GIT" clone -q origin.git seed 2>/dev/null && cd seed
printf 'alpha\n' > a.txt && "$REAL_GIT" add a.txt && "$REAL_GIT" commit -q -m initial && "$REAL_GIT" push -q origin main 2>/dev/null
"$REAL_GIT" checkout -q -b dev && printf 'on dev\n' > d.txt && "$REAL_GIT" add d.txt && "$REAL_GIT" commit -q -m 'dev work' && "$REAL_GIT" push -q origin dev 2>/dev/null
"$REAL_GIT" checkout -q -b more && printf 'more\n' > m.txt && "$REAL_GIT" add m.txt && "$REAL_GIT" commit -q -m 'more work' && "$REAL_GIT" push -q origin more 2>/dev/null
cd .. && git clone -q origin.git work && cd work
echo "--- tag messages: -m given more than once, and lines of #"
git tag -a t1 -m x -m y -m z; git cat-file -p t1 | tail -n +5
git tag -a t2 -m '' -m x; git cat-file -p t2 | tail -n +5
git tag -a t3 -m 'a
# c
b'; git cat-file -p t3 | tail -n +5
for t in t1 t2 t3; do git rev-parse $t; done
echo "--- merging what is not a branch's name"
git merge -q --no-edit --no-ff origin/dev~0; echo "exit: $?"
git log -1 --format=%s
git reset -q --hard HEAD~1
git merge -q --no-edit --no-ff refs/remotes/origin/dev; echo "exit: $?"
git log -1 --format='%H %s'
git reset -q --hard HEAD~1
echo "--- a branch made from origin's follows it"
git checkout -q -b more origin/more; echo "exit: $?"
git status -sb | head -1
git checkout -q main
echo "--- ref names by their kind"
git rev-parse tags/t1; git rev-parse remotes/origin/dev
echo "--- an amend refused in a merge that stopped stages nothing"
git checkout -q -b x && echo x > a.txt && git commit -qam x && git checkout -q main && echo y > a.txt && git commit -qam y
git merge -q x >/dev/null 2>&1; echo "exit: $?"
git commit --amend -qam amended 2>/dev/null; echo "exit: $?"
git status --porcelain
git commit -qm after >/dev/null 2>&1; echo "exit: $?"
git merge --abort
