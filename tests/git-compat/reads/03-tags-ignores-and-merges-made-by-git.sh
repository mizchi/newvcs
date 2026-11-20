# Git makes annotated tags, ignore rules and a merge within a file; then whoever is asked reads them.
#@ git
git init -q -b main . && git config user.name "Origin Author" && git config user.email origin@example.com
printf '*.log\nout/\n' > .gitignore
for i in 1 2 3 4 5 6 7 8 9 10; do echo "line $i"; done > shared.txt
git add -A && git commit -q -m base
git tag -a v1 -m "first release"
git checkout -q -b side && sed -i.bak 's/line 9/line nine/' shared.txt && rm shared.txt.bak && git commit -q -am side
git checkout -q main && sed -i.bak 's/line 2/line two/' shared.txt && rm shared.txt.bak && git commit -q -am main
git merge -q side
git tag -a v2 -m "second release"
git tag light
#@ read
echo "--- tags"; git tag
echo "types: $(git cat-file -t v1) $(git cat-file -t v2) $(git cat-file -t light)"
echo "ids: $(git rev-parse v1) $(git rev-parse v2) $(git rev-parse 'v2^{commit}')"
echo "--- the tag"; git cat-file -p v2; git show --format=%s --name-status v1
echo "--- the merge"; git log --format='%s|%p' -n 3; cat shared.txt
mkdir out && echo x > out/a.bin && echo x > debug.log && echo x > seen.txt
echo "--- what is seen"; git status --porcelain
git fsck --no-dangling; echo "fsck: $?"
