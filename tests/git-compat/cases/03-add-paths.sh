# Only what was added is committed.
git init -q -b main repo && cd repo
echo a > a.txt; echo b > b.txt; mkdir sub; echo c > sub/c.txt
git add a.txt
git commit -q -m "only a"
echo "--- tree after first"; git ls-tree -r --name-only HEAD
echo "--- status"; git status --porcelain
git add sub
git commit -q -m "the directory"
echo "--- tree after second"; git ls-tree -r --name-only HEAD
echo a2 > a.txt; echo b2 > b.txt
git commit -q -a -m "tracked changes only"
echo "--- tree after third"; git ls-tree -r HEAD
echo "--- status"; git status --porcelain
echo "--- log"; git log --format='%h %s'
echo "head: $(git rev-parse HEAD)"
