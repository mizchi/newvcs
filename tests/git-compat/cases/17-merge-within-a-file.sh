# Two lines of work that changed the same file in different places.
git init -q -b main repo && cd repo
for i in 1 2 3 4 5 6 7 8 9 10 11 12; do echo "line $i"; done > shared.txt
printf '#!/bin/sh\necho one\necho two\necho three\n' > run.sh
echo keep > keep.txt
git add -A && git commit -q -m base
git checkout -q -b side
sed -i.bak 's/line 2$/line two/; s/line 11/line eleven/' shared.txt && rm shared.txt.bak
echo "echo four" >> run.sh
echo side > side.txt
git add -A && git commit -q -m "side: lines 2 and 11, and the end of run.sh"
git checkout -q main
sed -i.bak 's/line 6/line six/' shared.txt && rm shared.txt.bak
sed -i.bak '1a\
# a comment at the top' run.sh && rm run.sh.bak && chmod +x run.sh
git add -A && git commit -q -m "main: line 6, the top of run.sh, and its mode"
git merge side
echo "--- merged file"; cat shared.txt
echo "--- run.sh"; cat run.sh; ls -l run.sh | cut -c1-10
echo "--- the commit"; git cat-file -p HEAD; git ls-tree -r HEAD
echo "--- what the merge changed for main"; git diff --stat HEAD~1 HEAD
echo "--- status"; git status --porcelain
git checkout -q -b same HEAD~1 && sed -i.bak 's/line 2$/line two/' shared.txt && rm shared.txt.bak && git commit -q -am "the same change as side made" && git checkout -q main
git merge -q same
echo "--- both made the same change"; git log -n 1 --format='%s|%p'; git diff --name-status HEAD~1 HEAD
