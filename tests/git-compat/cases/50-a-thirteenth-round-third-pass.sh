# What the thirteenth round's third pass found of `nv git` beside Git.
"$REAL_GIT" init -q --bare -b main origin.git
"$REAL_GIT" clone -q origin.git seed 2>/dev/null && cd seed
printf 'alpha\n' > a.txt && "$REAL_GIT" add a.txt && "$REAL_GIT" commit -q -m initial && "$REAL_GIT" push -q origin main 2>/dev/null
"$REAL_GIT" checkout -q -b dev && printf 'on dev\n' > d.txt && "$REAL_GIT" add d.txt && "$REAL_GIT" commit -q -m 'dev work' && "$REAL_GIT" push -q origin dev 2>/dev/null
"$REAL_GIT" tag -a vt -m 'tag msg' && "$REAL_GIT" tag lw && "$REAL_GIT" push -q origin vt lw 2>/dev/null
"$REAL_GIT" checkout -q main && printf 'new upstream\n' > new.txt && "$REAL_GIT" add new.txt && "$REAL_GIT" commit -q -m 'new file' && "$REAL_GIT" push -q origin main 2>/dev/null
cd .. && git clone -q origin.git work && cd work
echo "--- a tag and a branch of one name: the tag, as Git looks for names"
echo 2 >> a.txt && git commit -qam second
git tag same HEAD~1 && git branch same
git rev-parse same 2>/dev/null
git log -1 --format=%s same 2>/dev/null
git branch remotes/origin/main
git rev-parse remotes/origin/main
git rev-parse heads/same
git reset -q --hard same 2>/dev/null; git log -1 --format=%s
git reset -q --hard heads/same
git branch -D same >/dev/null; git branch -D remotes/origin/main >/dev/null; git tag -d same >/dev/null
echo "--- merges of refs named with their prefix"
L=$(git rev-parse HEAD)
for r in remotes/origin/dev refs/tags/vt tags/vt tags/lw; do
  git merge -q --no-edit --no-ff "$r"; echo "exit: $?"
  git log -1 --format='%H %s'
  git reset -q --hard $L
done
echo "--- a branch with what its upstream has not, merged to HEAD, is kept"
git switch -q -c topic
git branch -d main 2>/dev/null; echo "exit: $?"
git branch
echo "--- a branch merged to its upstream and not to HEAD goes"
git checkout -q dev 2>/dev/null; git checkout -q main
git branch -d dev 2>/dev/null; echo "exit: $?"
git branch
echo "--- a file staged as new, gone from disk, that the branch gone to has"
git switch -q main
git checkout -q -b local origin/main~1
echo 'only staged' > new.txt && git add new.txt && rm new.txt
git status --short
git switch -q main 2>/dev/null; echo "exit: $?"
git checkout -q main 2>/dev/null; echo "exit: $?"
git status --short
git branch --show-current
echo "--- show of a file by its id"
git show "$(git rev-parse HEAD:a.txt)"
echo "--- names and addresses cleaned as Git cleans them"
echo x > clean.txt && git add clean.txt
GIT_AUTHOR_NAME='A <b>' GIT_AUTHOR_EMAIL='<a@b>' GIT_COMMITTER_NAME='C>' git commit -qm clean; echo "exit: $?"
git log -1 --format='%H %an|%ae|%cn|%ce'
echo y > clean.txt && git add clean.txt
GIT_AUTHOR_NAME="$(printf ' A\nB, ')" GIT_AUTHOR_EMAIL='"a@b";' git commit -qm clean2; echo "exit: $?"
git log -1 --format='%H %an|%ae'
