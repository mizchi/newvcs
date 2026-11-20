# What was added is what is committed, whatever happens to the file afterwards.
git init -q -b main repo && cd repo
printf 'one\ntwo\nthree\n' > a.txt; echo b > b.txt; echo c > c.txt
git add -A && git commit -q -m base
printf 'one\nTWO\nthree\n' > a.txt
git add a.txt
printf 'one\nTWO\nthree\nfour\n' > a.txt
echo new > n.txt && git add n.txt && echo "edited after add" >> n.txt
echo gone > g.txt && git add g.txt && rm g.txt
git rm -q b.txt && echo "back again" > b.txt
echo "--- added, then changed"; git status --porcelain
echo "--- long"; git status
echo "--- what would be committed"; git diff --cached
echo "--- what would not"; git diff
git commit -q -m "what was added"
echo "--- committed"; git ls-tree -r HEAD
echo "--- a.txt in the commit"; git diff --name-status HEAD~1 HEAD
echo "--- left over"; git status --porcelain; git diff
git restore a.txt
echo "--- restored to the commit: $(tail -1 a.txt)"
echo x >> c.txt && git add c.txt && echo y >> c.txt
git restore c.txt
echo "--- restored to what was added: $(tail -1 c.txt)"; git status --porcelain
git restore --staged c.txt
echo "--- unstaged"; git status --porcelain
git commit -q -am "tracked files as they are"
echo "--- after -a"; git status --porcelain; git log --format=%s
echo z >> c.txt && git add c.txt
git mv c.txt moved.txt && echo "more" >> moved.txt
echo "--- moved with a staged change and an unstaged one"; git status --porcelain
git reset -q --hard
echo "--- hard reset"; git status --porcelain; ls
