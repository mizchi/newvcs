# What a third reviewer found different: formats, names of things, messages, and what push is asked.
git init -q -b main repo && cd repo
mkdir -p d/e && echo a > a.txt && echo b > d/b.txt && echo c > d/e/c.txt && git add -A
git commit -q -m "x1
x2"
git commit --allow-empty -m "y1
y2" | head -1
# A format given with its kind in front, an empty one, and the last of two that disagree.
git log --format=format:%s; echo "<"
git log --format=tformat:%s
git log --format= -n 1; echo "nothing above"
git log --oneline --format=%s -n 1
git log --format=%s --oneline -n 1 | cut -c9-
# Other names for the commit one is on.
[ "$(git rev-parse @)" = "$(git rev-parse HEAD)" ] && echo "@ is HEAD"
[ "$(git rev-parse refs/heads/main)" = "$(git rev-parse heads/main)" ] && echo "a branch by its full name"
git rev-parse nope 2>/dev/null; echo "rev-parse of nothing: $?"
git rev-parse 'HEAD~9' 2>/dev/null; echo "rev-parse too far back: $?"
# Listing where one stands.
(cd d && git ls-tree --name-only HEAD . && git ls-tree -r --name-only HEAD .)
# Modes of reset, the last one counting.
echo changed > a.txt && git reset -q --soft --hard && cat a.txt
# Moving into a directory that is not there; adding a directory the rules leave out.
git mv a.txt nodir/a.txt 2>/dev/null; echo "mv into nowhere: $?"; ls
mkdir ig && echo x > ig/f && echo 'ig/' > .gitignore
git add ig 2>/dev/null; echo "add of an ignored directory: $?"
git add .gitignore && git commit -q -m ignore
# Going to a branch that does not have a file that was removed here.
echo z > z.txt && git add -A && git commit -q -m z
git switch -q -c other && git rm -q z.txt && git commit -q -m noz && git switch -q main
rm z.txt && git switch other 2>&1; echo "switch: $?"; git status --short
# Putting a file back says so only when it was not said to be a file.
echo changed > a.txt && git checkout -- a.txt 2>&1; echo changed > a.txt && git checkout a.txt 2>&1
# A branch the remote has never had is not pushed without being told where.
cd .. && "$REAL_GIT" init -q --bare -b main up.git && "$REAL_GIT" clone -q up.git seed 2>/dev/null
(cd seed && echo s > s.txt && "$REAL_GIT" add -A && "$REAL_GIT" commit -q -m seed && "$REAL_GIT" push -q origin HEAD:main)
git clone -q up.git c && cd c
git switch -q -c topic && echo t > t.txt && git add -A && git commit -q -m t
git push -q 2>/dev/null; echo "push with nowhere to go: $?"
git push -q origin topic 2>/dev/null; echo "push of the branch by name: $?"
"$REAL_GIT" -C ../up.git log -1 --format=%s topic
git switch -q main && echo m > m.txt && git add -A && git commit -q -m m && git push -q; echo "push of a branch that follows one: $?"
"$REAL_GIT" -C ../up.git log -1 --format=%s main
