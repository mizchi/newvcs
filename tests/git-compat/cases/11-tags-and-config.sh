# Names for commits, and who is committing.
git init -q -b main repo && cd repo
git config user.name "Config Name"
git config user.email "config@example.com"
echo "name: $(git config user.name)"
echo "email: $(git config --get user.email)"
git config user.signingkey >/dev/null 2>&1; echo "unset key: $?"
echo a > a.txt && git add -A && git commit -q -m one
git tag v1
echo b > b.txt && git add -A && git commit -q -m two
git tag v2
git tag old HEAD~1
echo "--- tags"; git tag
echo "v1:  $(git rev-parse v1)"
echo "old: $(git rev-parse old)"
echo "v2^: $(git rev-parse v2^)"
echo "--- log from a tag"; git log --format=%s v1
git tag v1 >/dev/null 2>&1; echo "tag twice: $?"
git tag -d old
echo "--- after delete"; git tag
git branch topic && git branch -d topic
git checkout -q -b unmerged && echo c > c.txt && git add -A && git commit -q -m three && git checkout -q main
git branch -d unmerged >/dev/null 2>&1; echo "delete an unmerged branch: $?"
git branch -D unmerged
echo "--- branches"; git branch
( unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
  echo d > d.txt && git add -A && git commit -q -m "from config" && git log -n 1 --format='%an <%ae> / %cn <%ce>' )
