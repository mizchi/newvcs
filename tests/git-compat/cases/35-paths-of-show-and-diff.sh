# `show` and `diff` given paths after `--` keep to the changes under them: a file, a directory,
# several; a commit that changes none of them still shows itself, with no diff.
git init -q -b main repo && cd repo
mkdir -p src/deep docs && printf 'one\ntwo\nthree\nfour\nfive\nsix\n' > src/a.txt && printf 'b\n' > src/deep/b.txt
printf 'doc\n' > docs/d.md && printf 'top\n' > top.txt
git add -A && git commit -q -m base
printf 'one\n2\nthree\nfour\nfive\nsix\n' > src/a.txt && printf 'b2\n' > src/deep/b.txt && printf 'doc2\n' > docs/d.md && echo new > src/new.txt
git add -A && git commit -q -m second
echo '== show -- file'; git show HEAD -- src/a.txt
echo '== show -- dir'; git show --stat HEAD -- src
echo '== show -- two'; git show --name-status HEAD -- docs top.txt src/deep
echo '== show -- none of it'; git show --format=%s HEAD -- top.txt; echo "exit $?"
echo '== show -- nothing there'; git show --name-only HEAD -- nowhere; echo "exit $?"
echo '== show -- none of it, as Git writes a commit'; git show HEAD -- top.txt; echo "exit $?"
echo '== show -- dir/'; git show --name-only --format=%s HEAD -- src/deep/
echo '== diff A B -- dir'; git diff HEAD~1 HEAD -- src
echo '== diff A B --name-only -- two'; git diff --name-only HEAD~1 HEAD -- docs src/new.txt
# A file moved, and the paths name only one end of it: the other end is not in the diff.
git mv src/a.txt docs/a.txt && git commit -q -m move
echo '== moved, both ends'; git show --name-status HEAD -- src docs
echo '== moved, new end'; git show --name-status HEAD -- docs
echo '== moved, old end'; git show --stat HEAD -- src
# What is on disk and staged, kept to paths too.
printf 'doc3\n' > docs/d.md && printf 'top2\n' > top.txt && git add top.txt
echo '== diff -- path'; git diff -- docs
echo '== diff -- staged path'; git diff --name-only -- top.txt
echo '== diff --cached -- path'; git diff --cached --stat -- top.txt docs
# Paths are from where the command is run, as Git takes them; `.` is everything below.
cd src
echo '== from a directory'; git show --name-only --format=%s HEAD~1 -- deep ../docs
echo '== dot'; git show --name-only --format=%s HEAD~1 -- .
echo '== diff from a directory'; git diff --stat HEAD~2 HEAD -- .
cd ..
echo '== the first commit'; git show --name-status --format=%s HEAD~2 -- src/deep
