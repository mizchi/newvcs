# Patches: between commits, of the working tree, of what was added, and `show`.
git init -q -b main repo && cd repo
cat > lib.py <<'PY'
import os

def first(x):
    return x + 1

def second(x):
    y = x * 2
    return y

def third(x):
    return x - 1

def fourth(x):
    return x
PY
printf 'one\ntwo\nthree\n' > notes.txt
printf 'no newline at end' > tail.txt
git add -A && git commit -q -m "base"
sed -i.bak 's/y = x \* 2/y = x * 3/' lib.py && rm lib.py.bak
printf 'one\ntwo\nthree\nfour\n' > notes.txt
printf 'no newline at end, changed' > tail.txt
echo new > added.txt
echo "--- working tree"; git diff
git add lib.py added.txt
echo "--- what was added"; git diff --cached
echo "--- what was not"; git diff
git add -A && git commit -q -m "change things"
git rm -q notes.txt && chmod +x lib.py
git commit -q -am "remove and chmod"
echo "--- two commits back to now"; git diff HEAD~2 HEAD
echo "--- one commit"; git diff HEAD~1 HEAD
echo "--- show HEAD~1"; git show HEAD~1
echo "--- show the first"; git show HEAD~2
echo "--- names of a show"; git show --name-status --format=%s HEAD
