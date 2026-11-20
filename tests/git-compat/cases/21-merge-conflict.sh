# Two lines of work that changed the same lines: someone has to decide.
git init -q -b main repo && cd repo
for i in 1 2 3 4 5 6 7 8 9 10; do echo "line $i"; done > shared.txt
echo keep > keep.txt; echo other > other.txt
git add -A && git commit -q -m base
git checkout -q -b side
sed -i.bak 's/line 5/line five, says side/; s/line 9/line nine/' shared.txt && rm shared.txt.bak
echo side > side.txt; echo "other, by side" > other.txt
git add -A && git commit -q -m "side"
git checkout -q main
sed -i.bak 's/line 5/line 5, says main/; s/line 2/line two/' shared.txt && rm shared.txt.bak
git commit -q -am "main"
git merge side; echo "merge: $?"
echo "--- the file"; cat shared.txt
echo "--- status"; git status --porcelain; git status
git commit -m "too early"; echo "commit while unmerged: $?"
git merge --abort; echo "abort: $?"
echo "--- after abort"; git status --porcelain; cat shared.txt | sed -n 5p; ls
git merge -q side; echo "merge again: $?"
grep -v -e '^<<<<<<<' -e '^=======' -e '^>>>>>>>' -e 'says side' shared.txt > resolved && mv resolved shared.txt
git add shared.txt
echo "--- resolved"; git status --porcelain; git status
git commit -m "Merge branch 'side', keeping main's line"; echo "commit: $?"
echo "--- the merge"; git log --format='%s|%p' -n 1; git cat-file -p HEAD | sed -n 1,3p; git ls-tree -r HEAD; cat shared.txt
echo "--- clean"; git status --porcelain
git fsck --no-dangling; echo "fsck: $?"
