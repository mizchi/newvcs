# Redoing the last commit.
git init -q -b main repo && cd repo
echo a > a.txt && git add -A && git commit -q -m one
echo b > b.txt && git add -A && git commit -q -m "two, badly worded"
before=$(git rev-parse HEAD)
git commit -q --amend -m "two"
echo "--- reworded"; git log --format='%s|%p'
echo "changed id: $([ "$before" != "$(git rev-parse HEAD)" ] && echo yes)"
echo "head: $(git rev-parse HEAD)"
echo c > c.txt && git add c.txt
git commit -q --amend --no-edit
echo "--- with a forgotten file"; git ls-tree -r --name-only HEAD; git log --format=%s
echo more >> a.txt
git commit -q -a --amend -m "two, and a change to a"
echo "--- with -a"; git cat-file -p HEAD; git status --porcelain
