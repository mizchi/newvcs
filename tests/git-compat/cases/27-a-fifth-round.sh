# What a fifth reviewer found different.
git init -q -b main repo && cd repo
echo base > a.txt && mkdir d && echo one > d/one && echo keep > z.txt && git add -A && git commit -q -m base
# Switching where a directory with a file nobody tracks would have to become a file: nothing is touched.
git switch -q -c other && echo other-a > a.txt && rm -r d && echo 'd is a file now' > d && git add -A && git commit -q -m other
git switch -q main && echo precious > d/untracked
git switch other >/dev/null 2>&1; echo "switch: $?"; git status --short; cat a.txt d/untracked; git branch --show-current
rm d/untracked
# Counts that are not numbers.
git log -n abc >/dev/null 2>&1; echo "log -n abc: $?"
git log -n 99999999999999999999 >/dev/null 2>&1; echo "log -n huge: $?"
# Names that are not names.
git branch - 2>&1; echo "branch -: $?"
git init -q -b 'a b' ../sub 2>&1; echo "init -b: $?"
# A path that begins at the root is no path of a commit.
git show HEAD:/a.txt 2>&1; echo "show: $?"
# Taking out of the index what is not in it.
git restore --staged nonexist 2>&1; echo "restore --staged: $?"
# Asked whether an object is there.
git cat-file -e 1234567890123456789012345678901234567890 2>&1; echo "cat-file -e: $?"
# A binary file beside counts of four digits.
seq 1 2000 > many.txt && head -c 5000 /dev/zero > bin && git add -A && git diff --cached --stat
git commit -q -m more && git show --stat --format=%s HEAD
