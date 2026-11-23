# `rm --cached` takes a file out of the index and leaves it on disk, where it is then not tracked.
git init -q -b main repo && cd repo
mkdir -p dir && printf 'a\n' > a.txt && printf 'b\n' > b.txt && printf 'c\n' > c.txt && printf 'x\n' > dir/x.txt && printf 'y\n' > dir/y.txt
git add -A && git commit -q -m first
echo '== one file'; git rm --cached a.txt; echo "exit $?"; cat a.txt; git status --porcelain
echo '== a directory needs -r'; git rm --cached dir; echo "exit $?"
echo '== with -r, quietly'; git rm -r -q --cached dir; echo "exit $?"; ls dir; git status --porcelain
# Edited on disk only, the file keeps the edit and may go out of the index; staged and then edited
# again, what is staged is in neither the file nor the commit, and Git keeps it unless forced.
printf 'b2\n' > b.txt
echo '== edited on disk'; git rm --cached b.txt; echo "exit $?"; cat b.txt
printf 'c2\n' > c.txt && git add c.txt && printf 'c3\n' > c.txt
echo '== staged, then edited'; git rm --cached c.txt; echo "exit $?"
echo '== forced'; git rm -f --cached c.txt; echo "exit $?"; cat c.txt
echo new > new.txt && git add new.txt
echo '== added, not committed'; git rm --cached new.txt; echo "exit $?"; git status --porcelain
git commit -q -m second; echo "commit $?"
git show --format=%s --name-status; git status --porcelain
echo '== not tracked'; git rm --cached a.txt; echo "exit $?"
# A directory whose tracked files are gone from the index holds nothing tracked, whichever way
# they went.
mkdir -p gone && echo g > gone/g.txt && git add gone && git commit -q -m gone && git rm -q gone/g.txt && echo loose > gone/loose.txt
echo '== emptied by rm'; git status --porcelain
# A file taken out that the rules ignore is not shown as untracked.
echo i > i.log && git add i.log && git commit -q -m log && echo 'i.log' > .gitignore && git rm -q --cached i.log
echo '== ignored'; git status --porcelain
