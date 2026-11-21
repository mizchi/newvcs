# What a sixth reviewer found: nothing is half done, and nothing outside the repository is touched.
git init -q -b main repo && cd repo
echo a > a.txt && echo m > m.txt && mkdir d && echo c > d/c.txt && git add -A && git commit -q -m base
git switch -q -c w && echo changed > a.txt && git rm -q m.txt && mkdir nd && echo x > nd/x && echo tracked > way && git add -A && git commit -q -m w
git switch -q main
# An untracked file where the other branch has a directory: nothing is touched.
echo precious > nd
git switch w >/dev/null 2>&1; echo "switch: $?"; git status --short; cat a.txt nd; git branch --show-current
git merge w >/dev/null 2>&1; echo "merge: $?"; git status --short; cat a.txt nd
rm nd
# An untracked file under a name the other branch has in other letters: where the file system
# takes the two for one file, it is in the way.
echo 'exists nowhere else' > WAY
git switch w >/dev/null 2>&1; echo "switch over another spelling: $?"; cat WAY; git branch --show-current; git switch -q main 2>/dev/null
rm -f WAY way; git checkout -q -- . 2>/dev/null
# A path beyond a link is not a path of the repository.
mkdir ../out && echo secret > ../out/s && ln -s ../out out
git add out/s 2>&1; echo "add beyond a link: $?"; git status --short
rm out
# A directory of the tree replaced by a link to somewhere else: put back, and somewhere else left alone.
mkdir ../elsewhere && echo 'not ours' > ../elsewhere/c.txt && rm -r d && ln -s ../elsewhere d
git reset -q --hard; echo "reset: $?"; cat ../elsewhere/c.txt d/c.txt; test -L d && echo 'still a link'; git status --short
