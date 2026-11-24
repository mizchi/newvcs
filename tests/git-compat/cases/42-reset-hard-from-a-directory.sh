# `reset --hard` run from a directory the commit it goes to does not have: the directory, empty,
# is left for whoever stands in it, as Git leaves it. (Twelfth round: nv removed it.)
git init -q -b main repo && cd repo
echo t > t && git add t && git commit -q -m one
mkdir -p d/e && echo f > d/e/f && git add d && git commit -q -m two
cd d/e
git reset -q --hard HEAD~1; echo "reset: $?"
pwd >/dev/null && ls -a; git status --porcelain; echo "status: $?"
cd ../..
ls; git status --porcelain
