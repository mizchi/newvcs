# A path whose removal is staged is one the index no longer has: named to `restore`, `checkout --`
# or `add`, it matches nothing, and the command does nothing at all. And `add -A <path>` stages
# what is under the path, not the whole tree. (Twelfth round: nv went ahead with the other paths,
# and an edit was lost.)
git init -q -b main repo && cd repo
mkdir d && echo c > d/c && echo one > b && echo d > e && git add -A && git commit -q -m one
git rm -q d/c && echo "my edit" > b
echo '== restore'; git restore -- b d; echo "exit $?"; cat b; git status --porcelain
echo '== checkout --'; git checkout -- b d; echo "exit $?"; cat b
git rm -q e && echo more >> b
echo '== add'; git add b e; echo "exit $?"; git status --porcelain
git reset -q --hard
mkdir -p in && echo 1 > in/x && echo 2 > out
echo '== add -A <path>'; git add -A in; echo "exit $?"; git status --porcelain
