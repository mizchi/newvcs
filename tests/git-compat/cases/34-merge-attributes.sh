# A path `.gitattributes` gives `merge=union` takes both sides' lines where they changed the same
# ones, as a list of entries wants; other paths are merged as text.
git init -q -b main repo && cd repo
printf 'a\n' > list && printf 'a\n' > plain && printf 'list merge=union\n' > .gitattributes
mkdir sub && printf 'a\n' > sub/names && printf '*.txt merge=union\n' > sub/.gitattributes && printf 'a\n' > sub/n.txt
git add -A && git commit -q -m base
git switch -q -c side && printf 'a\nside\n' | tee list plain sub/names sub/n.txt >/dev/null && git commit -q -am side
git switch -q main && printf 'a\nmain\n' | tee list plain sub/names sub/n.txt >/dev/null && git commit -q -am main
# Where a path merged as text clashes, the merge stops; the union paths are merged all the same.
git merge -q side >/dev/null 2>&1; echo "merge: $?"
cat list sub/n.txt; git status --porcelain
git merge --abort
# Where only union paths clash, the merge goes through.
printf 'a\nside\n' | tee plain sub/names >/dev/null && git commit -q -am 'take theirs'
git merge -q -m 'merge side' side; echo "merge: $?"
cat list sub/n.txt sub/names; git log --format=%s -1; git status --porcelain
