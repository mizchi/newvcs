# A tag that is an object of its own.
git init -q -b main repo && cd repo
echo a > a.txt && git add -A && git commit -q -m one
echo b > b.txt && git add -A && git commit -q -m two
git tag -a v1 -m "the first release" HEAD~1
git tag -m "the second" v2
git tag light
echo "--- tags"; git tag
echo "v1:           $(git rev-parse v1)"
echo "v1^{commit}:  $(git rev-parse 'v1^{commit}')"
echo "v1~0:         $(git rev-parse v1~0)"
echo "light:        $(git rev-parse light)"
echo "types: $(git cat-file -t v1) $(git cat-file -t light) $(git cat-file -t v2)"
echo "--- the tag object"; git cat-file -p v1
echo "--- show"; git show v1
echo "--- show with a format"; git show --format=%s --name-status v2
echo "--- log from it"; git log --format=%s v1
echo "--- diff between tags"; git diff --name-status v1 v2
git tag -d v1
echo "--- after delete"; git tag
git tag -a v2 -m again >/dev/null 2>&1; echo "tag twice: $?"
git fsck --no-dangling; echo "fsck: $?"
