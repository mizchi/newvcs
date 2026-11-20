# Taking things back, and moving them.
git init -q -b main repo && cd repo
echo a > a.txt; echo b > b.txt; mkdir d && echo c > d/c.txt && echo e > d/e.txt
git add -A && git commit -q -m base
git rm a.txt
git mv b.txt renamed.txt
git rm -r -q d
echo "--- after rm and mv"; git status --porcelain; ls
git commit -q -m "rm and mv"
echo "--- tree"; git ls-tree -r --name-only HEAD
echo local > renamed.txt
git restore renamed.txt
echo "--- restored: $(cat renamed.txt)"; git status --porcelain
rm renamed.txt && git checkout -- renamed.txt
echo "--- brought back: $(cat renamed.txt)"
echo x > x.txt; echo edit >> renamed.txt; git add -A
echo "--- added"; git status --porcelain
git reset -q
echo "--- reset"; git status --porcelain
git add x.txt; git restore --staged x.txt
echo "--- restore --staged"; git status --porcelain
git add -A; git reset -q x.txt
echo "--- reset one path"; git status --porcelain
git reset -q --hard
echo "--- reset --hard keeps untracked"; git status --porcelain; cat renamed.txt
git add x.txt && git commit -q -m "x"
git reset -q --soft HEAD~1
echo "--- reset --soft"; git status --porcelain; git log --format=%s
git commit -q -m "x again"
git reset -q --mixed HEAD~1
echo "--- reset --mixed"; git status --porcelain; git log --format=%s
git add -A && git commit -q -m "x once more"
git reset -q --hard HEAD~2
echo "--- reset --hard two back"; git status --porcelain; git log --format=%s; ls
git rm missing.txt >/dev/null 2>&1; echo "rm a missing path: $?"
echo dirty >> a.txt; git rm a.txt >/dev/null 2>&1; echo "rm a modified file: $?"
