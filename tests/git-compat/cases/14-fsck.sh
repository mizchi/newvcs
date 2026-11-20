# A sound repository has nothing to report.
git init -q -b main repo && cd repo
echo a > a.txt && mkdir d && echo b > d/b.txt && git add -A && git commit -q -m one
git checkout -q -b side && echo c > c.txt && git add -A && git commit -q -m two && git checkout -q main
git tag v1
git fsck; echo "fsck: $?"
git fsck --full --strict --no-dangling; echo "fsck with flags: $?"
