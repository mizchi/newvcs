# Bringing two lines of work together, where they do not touch the same file.
git init -q -b main repo && cd repo
echo base > base.txt && git add -A && git commit -q -m base
git checkout -q -b fast
echo one > one.txt && git add -A && git commit -q -m "fast one"
git checkout -q main
git merge -q fast
echo "--- fast-forward: $(git log --format=%s | tr '\n' ',')"; echo "main at fast: $([ "$(git rev-parse main)" = "$(git rev-parse fast)" ] && echo yes)"
git merge fast
git checkout -q -b side
echo side > side.txt && rm base.txt && git add -A && git commit -q -m "side work"
git checkout -q main
echo main > main.txt && git add -A && git commit -q -m "main work"
git merge -q side
echo "--- merge commit"; git cat-file -p HEAD
echo "--- files"; git ls-tree -r --name-only HEAD
echo "--- log"; git log --format='%s|%p'
echo "--- medium"; git log -n 1
git checkout -q -b topic
echo t > t.txt && git add -A && git commit -q -m "topic work"
git checkout -q -b other main
echo o > o.txt && git add -A && git commit -q -m "other work"
git merge -q topic
echo "--- into a branch that is not main"; git log -n 1 --format=%s
git checkout -q main
git merge -q --no-ff -m "bring other in" other
echo "--- no-ff"; git log -n 1 --format='%s|%p'
git status --porcelain
git merge -q no-such >/dev/null 2>&1; echo "merge unknown: $?"
