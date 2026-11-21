# What a fourth reviewer found different.
git init -q -b main repo && cd repo
mkdir -p d0/e && echo a > a.txt && echo f > d0/f0.txt && echo g > d0/e/g.txt && git add -A && git commit -q -m one
# Moving into a directory named with a slash that is not there.
git mv a.txt nodir/ 2>/dev/null; echo "mv: $?"; ls; git status --short
# One path of a tree, wherever it is; and a tree by its name.
git ls-tree HEAD d0/f0.txt; git ls-tree HEAD d0/e; git ls-tree --name-only HEAD d0/e/g.txt
git ls-tree --name-only 'HEAD^{tree}'; git rev-parse --short 'HEAD^{tree}'
# A message as Git cleans it: the id is over the cleaned one.
echo x >> a.txt && git add a.txt && git commit -m "

blank first


body  

"
git log -1 --format='%H|%s|%b|'
git commit --allow-empty -m "x  
second" | head -1
git log -1 --format='%H|%s'
# Showing a commit with a format: what stands between the format's output and the patch.
git show --format= --name-only HEAD~1
git show --pretty=format:%s --name-only HEAD~1
git show --format=%s --name-only HEAD~1
# A name that is a branch and a file.
echo m > main && git add main && git commit -q -m 'a file called main'
git log --oneline main 2>/dev/null; echo "log of a name that is both: $?"
# Removing nothing.
git rm 2>/dev/null; echo "rm of nothing: $?"
# A merge will not start over what has been added.
git switch -q -c side && echo s > s.txt && git add -A && git commit -q -m side
git switch -q main && echo m2 > m2.txt && git add -A && git commit -q -m m2
echo st > st.txt && git add st.txt
git merge side >/dev/null 2>&1; echo "merge over something staged: $?"; git log -1 --format=%s
git reset -q && rm st.txt
# A merge that stopped is concluded by committing every tracked file as it is.
printf 'l1\nl2\nl3\n' > c.txt && git add -A && git commit -q -m base-c
git switch -q -c feat && printf 'l1\nF\nl3\n' > c.txt && git commit -q -am feat
git switch -q main && printf 'l1\nM\nl3\n' > c.txt && git commit -q -am main-c
git merge -q feat >/dev/null 2>&1; echo "merge: $?"
printf 'l1\nBOTH\nl3\n' > c.txt
git commit -q -am resolved; echo "commit -a during the merge: $?"
git log -1 --format='%s|%P' | tr ' ' '\n' | wc -l | tr -d ' '; git log -1 --format=%s; git status --short
# The bars of a stat, when they are scaled to fit.
seq 1 50 > sw1.txt; seq 51 100 > sw2.txt; seq 1 105 > split.txt; git add -A; git commit -q -m s
seq 51 100 > sw1.txt; git rm -q split.txt sw2.txt; git add -A; git diff --cached --stat
