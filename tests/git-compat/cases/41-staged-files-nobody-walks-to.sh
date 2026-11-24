# Staged files the look over the tree does not report: one under a directory the rules now leave
# out, one put back to what the last commit has. Both are as the disk has them, not gone. And a
# file moved onto a tracked file already deleted takes its place in the index. (Twelfth round.)
git init -q -b main repo && cd repo
mkdir -p d/u s && echo k > k && echo a > d/u/a && echo 1 > s/one && echo x > x && echo m > m && git add -A && git commit -q -m one
echo b > d/u/b && git add d/u/b && echo u > .gitignore
echo '== under an ignored directory'; git status --porcelain; git diff --name-status
git add -A && git commit -q -m two && git ls-tree -r --name-only HEAD
echo m2 > m && git add m && echo m > m
echo '== back to the commit after add'; git status --porcelain; git diff --name-status
git reset -q --hard
rm x && git mv s x; echo "mv: $?"
echo '== moved onto a deleted file'; git status --porcelain
git commit -q -m three && git ls-tree -r --name-only HEAD && git status --porcelain
