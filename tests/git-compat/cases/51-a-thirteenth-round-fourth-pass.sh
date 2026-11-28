# What the thirteenth round's fourth pass found of `nv git` beside Git.
"$REAL_GIT" init -q --bare -b main origin.git
"$REAL_GIT" clone -q origin.git seed 2>/dev/null && cd seed
printf 'alpha\n' > a.txt && "$REAL_GIT" add a.txt && "$REAL_GIT" commit -q -m initial && "$REAL_GIT" push -q origin main 2>/dev/null
"$REAL_GIT" checkout -q -b dev && printf 'on dev\n' > d.txt && "$REAL_GIT" add d.txt && "$REAL_GIT" commit -q -m 'dev work' && "$REAL_GIT" push -q origin dev 2>/dev/null
cd .. && git clone -q origin.git work && cd work
echo "--- names, addresses and dates from the environment, as Git takes them"
n=0
for env in "GIT_AUTHOR_EMAIL=" "GIT_COMMITTER_EMAIL=" "GIT_AUTHOR_NAME=" "GIT_COMMITTER_NAME=" \
  "GIT_AUTHOR_DATE=1700000000 +9999" "GIT_AUTHOR_DATE=1700000000 +2360" "GIT_AUTHOR_DATE=1700000000 -2359" \
  "GIT_AUTHOR_DATE=0 +0000" "GIT_AUTHOR_DATE=-5 +0000" "GIT_AUTHOR_DATE=99999999 +0000" "GIT_AUTHOR_DATE=123456789 +0000" \
  "GIT_AUTHOR_DATE=4102444800 +0000" "GIT_AUTHOR_DATE=@0 +0000" "GIT_AUTHOR_DATE=@-5 +0000" "GIT_AUTHOR_DATE=1700000000" \
  "GIT_AUTHOR_DATE=1970-01-01T00:00:00Z" "GIT_AUTHOR_DATE=2023-11-14T22:13:20"; do
  n=$((n + 1)); echo $n >> e.txt && git add e.txt
  env "$env" git commit -qm "c$n" 2>/dev/null; echo "$env: exit $?"
  git rev-parse HEAD; git cat-file -p HEAD | grep -E '^(author|committer) '
done
echo "--- a merge given an empty message"
git reset -q --hard origin/main
git merge -q --no-ff -m '' origin/dev 2>/dev/null; echo "exit: $?"
git log -1 --format=%s
git status --short
git merge --abort 2>/dev/null; git reset -q --hard origin/main
git merge -q --no-ff -m '   ' origin/dev 2>/dev/null; echo "exit: $?"
git log -1 --format=%s
git merge --abort 2>/dev/null; git reset -q --hard origin/main
echo "--- the branch HEAD is on, by a name nothing else has"
git tag main
git rev-parse --abbrev-ref HEAD
git tag -d main >/dev/null
git checkout -q -b tags/x && git tag x
git rev-parse --abbrev-ref HEAD
git checkout -q main
echo "--- a file taken out of the index and kept, that the branch gone to has not"
git checkout -q -b keep origin/main~0 2>/dev/null || git checkout -q -b keep
echo kept > k.txt && git add k.txt && git commit -qm k && git rm -q --cached k.txt
git status --short
git switch -q main 2>/dev/null; echo "exit: $?"
git branch --show-current; ls k.txt
