# Walking back through commits and comparing them.
git init -q -b main repo && cd repo
for i in 1 2 3 4; do
  echo "version $i" > file.txt
  echo "$i" > "only-in-$i.txt"
  [ "$i" = 3 ] && rm only-in-1.txt
  git add -A && git commit -q -m "commit $i"
done
echo "head:   $(git rev-parse HEAD)"
echo "head^:  $(git rev-parse HEAD^)"
echo "head~2: $(git rev-parse HEAD~2)"
echo "head~3: $(git rev-parse HEAD~3)"
echo "short:  $(git rev-parse --short HEAD)"
echo "tree:   $(git rev-parse 'HEAD^{tree}')"
echo "--- oneline"; git log --oneline
echo "--- last two"; git log -n 2 --format='%s'
echo "--- from an older commit"; git log --format='%s|%P' HEAD~2
echo "--- medium"; git log -n 2
echo "--- diff 1..4"; git diff --name-status HEAD~3 HEAD
echo "--- diff 3..4"; git diff --name-only HEAD~1 HEAD
echo edit >> file.txt
echo "--- worktree"; git diff --name-only
echo "--- old commit"; git cat-file -p HEAD~3
