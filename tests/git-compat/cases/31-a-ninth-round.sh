# What a ninth reviewer's random tester found, against Git doing the same.
git init -q -b main repo && cd repo
# Added, then removed from the disk, then `commit -a` with nothing to commit: the staging is as it was.
echo a > a && git add a && rm a
git commit -q -am x >/dev/null 2>&1; echo "commit -a: $?"; git status --porcelain
echo base > base && git add base && git commit -q -m base
# Added, then changed on disk: putting it back is to what was added.
echo one > n && git add n && echo two >> n
git restore n; echo "restore: $?"; cat n; git status --porcelain
echo three >> n; git checkout -- n; echo "checkout --: $?"; cat n
echo four >> n; git restore .; echo "restore .: $?"; cat n
# Rules on a file system that does not tell `Secret.env` from `secret.env`: as Git matches them there.
printf 'secret.env\nBuild/\n' > .gitignore && echo pw > Secret.env && mkdir build && echo o > build/out
git add -A && git commit -q -m rules && git ls-tree -r --name-only HEAD; git status --porcelain
# Merging a tag: Git's words for it, and an annotated tag's message, are in the commit.
git switch -q -c t1 && echo t1 > t1.txt && git add t1.txt && git commit -q -m t1 && git tag lt
git switch -q -c t2 && echo t2 > t2.txt && git add t2.txt && git commit -q -m t2 && git tag -a at -m 'the tag says this'
git switch -q main && echo m > m.txt && git add m.txt && git commit -q -m m
git merge -q --no-edit lt; git log -1 --format='%H %s'
git merge -q --no-edit at; git log -1 --format='%H%n%B'
