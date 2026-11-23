# `diff <rev>` is that commit against the files as they are: what is staged and what is not, as
# one change. Files Git does not track are not in it.
git init -q -b main repo && cd repo
mkdir -p src && printf 'one\ntwo\nthree\nfour\nfive\nsix\n' > src/a.txt && printf 'b\n' > b.txt
printf 'c\n' > c.txt && printf 'd\n' > d.txt && printf 'e\n' > e.txt && printf 'run\n' > run.sh
git add -A && git commit -q -m first
printf 'b2\n' > b.txt && git commit -q -am second
echo '== nothing changed since'; git diff HEAD; echo "exit $?"
echo '== a commit before, nothing on disk'; git diff --name-status HEAD~1
# Staged, not staged, both; a new file staged; one not tracked; one removed on disk; one by `git rm`.
printf 'one\n2\nthree\nfour\nfive\nsix\n' > src/a.txt
printf 'c2\n' > c.txt && git add c.txt && printf 'c3\n' > c.txt
echo new > staged.txt && git add staged.txt && echo loose > untracked.txt
rm d.txt && git rm -q e.txt && chmod +x run.sh
echo '== diff HEAD'; git diff HEAD
echo '== diff HEAD~1 --stat'; git diff --stat HEAD~1
echo '== diff main --name-status'; git diff --name-status main
echo '== kept to paths'; git diff --name-only HEAD~1 -- src b.txt e.txt
# A file moved and staged, and one moved on disk only.
git mv src/a.txt src/moved.txt
echo '== moved'; git diff --name-status HEAD -- src
cd src
echo '== from a directory'; git diff --name-only HEAD -- .
cd ..
echo '== the index against a commit'; git diff --cached --name-status HEAD~1
echo '== and kept to paths'; git diff --cached --stat HEAD -- c.txt staged.txt
