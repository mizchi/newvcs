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
  "GIT_AUTHOR_DATE=1970-01-01T00:00:00Z" "GIT_AUTHOR_DATE=2023-11-14T22:13:20" \
  "GIT_AUTHOR_DATE=1795000000 +01:00" "GIT_AUTHOR_DATE=100000000 -0000" "GIT_AUTHOR_DATE=1969-12-31T23:59:59Z" \
  "GIT_AUTHOR_DATE=1969-02-01T00:59:00+0100" "GIT_AUTHOR_DATE=2100-01-01T00:00:00Z" "GIT_AUTHOR_DATE=2099-12-31T23:59:59Z" \
  "GIT_AUTHOR_DATE=@0" "GIT_AUTHOR_DATE=@5" "GIT_AUTHOR_DATE=@99999999" "GIT_AUTHOR_DATE=@1700000000 +0130" \
  "GIT_AUTHOR_DATE=@100000000" "GIT_AUTHOR_DATE=@0001700000000" \
  "GIT_AUTHOR_DATE=2023-11-14 22:13:20 +0900" "GIT_AUTHOR_DATE=2023-11-14T22:13:20-05:30"; do
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
echo "--- (fifth pass) a commit -a refused for who makes it stages nothing"
rm -f k.txt; git reset -q --hard; git checkout -q main; git reset -q --hard origin/main
echo z >> a.txt
GIT_AUTHOR_NAME= git commit -qam refused 2>/dev/null; echo "exit: $?"
git status --short
git checkout -q -- a.txt
echo "--- (fifth pass) every -m of a merge, cleaned"
git merge -q --no-ff -m 'x' -m 'y' -m '' -m 'z' origin/dev; echo "exit: $?"
git log -1 --format=%B
git reset -q --hard HEAD~1
git merge -q --no-ff -m "$(printf 'a\n\n\nb\r\nc  ')" origin/dev; echo "exit: $?"
git log -1 --format=%B | od -c | head -3
git reset -q --hard HEAD~1
echo "--- (fifth pass) a tag with no message of an annotated tag names that tag"
git tag -a v2 -m 'second' && git tag lw v2 && git tag lw2 refs/tags/v2
git cat-file -t lw; git rev-parse lw; git rev-parse lw2
echo "--- (fifth pass) the branch HEAD is on, named so that nothing else is found"
git checkout -q -b origin; git rev-parse --abbrev-ref HEAD; git checkout -q main; git branch -D origin >/dev/null
git tag y; git checkout -q -b refs/tags/y 2>/dev/null; echo "exit: $?"; git rev-parse --abbrev-ref HEAD; git checkout -q main
echo "--- (fifth pass) checkout -b in a merge that stopped"
git checkout -q -b clash1 && echo one > a.txt && git commit -qam one && git checkout -q main && echo two > a.txt && git commit -qam two
git merge -q clash1 >/dev/null 2>&1; echo "exit: $?"
git checkout -q -b during 2>/dev/null; echo "exit: $?"
git checkout -b during-loud 2>/dev/null; echo "exit: $?"
git branch --show-current; git status --short
git merge --abort 2>/dev/null; echo "abort: $?"
git checkout -q main; git reset -q --hard; git branch -D during-loud >/dev/null
git merge -q clash1 >/dev/null 2>&1; echo "exit: $?"
echo settled > a.txt && git add a.txt
git checkout -q -b during; echo "exit: $?"
git branch --show-current; git status --short
git merge --abort 2>/dev/null; echo "abort: $?"
git commit -qm settled; git log -1 --format='%s %P' | wc -w
git checkout -q main
echo "--- (sixth pass) a push of a name that finds more than one ref"
git checkout -q -b same && git tag same && echo s >> a.txt && git commit -qam same
git push -q origin same 2>/dev/null; echo "exit: $?"
"$REAL_GIT" -C ../origin.git for-each-ref --format='%(refname)' | grep same
git checkout -q main
