# Commands run below the top, objects named by a path, and names Git has rules about.
git init -q -b main repo && cd repo
mkdir -p sub/deep d && echo s > sub/s.txt && echo x > sub/deep/x.txt && echo c > d/c.txt && echo top > top.txt
git add -A && git commit -q -m "first line
second line of the subject

body"
# Listing a tree from below the top lists where the command was run.
(cd sub && git ls-tree HEAD && git ls-tree -r --name-only HEAD && git ls-tree --name-only HEAD deep/)
# An object named by where it is in a commit.
git rev-parse HEAD:top.txt
git rev-parse HEAD:sub
git cat-file -t HEAD:sub; git cat-file -t HEAD:sub/s.txt; git cat-file -p HEAD:sub/s.txt
git ls-tree --name-only HEAD:sub
git ls-tree HEAD:d
(cd sub && git show HEAD:./s.txt && git show HEAD:top.txt)
git cat-file -e HEAD:nothing 2>/dev/null; echo "cat-file -e of nothing: $?"
git cat-file -e 0000000000000000000000000000000000000000 2>/dev/null; echo "cat-file -e of no object: $?"
# Removing the last file of the directory one is standing in leaves a place to stand.
(cd d && git rm -q c.txt && git status --short && echo "still here: $(pwd | sed 's|.*/||')")
git commit -q -m "removed" && git reset -q --hard HEAD~1 && git reset --hard HEAD
# A branch may not be called HEAD.
git branch HEAD 2>/dev/null; echo "branch HEAD: $?"
git branch | tr -d ' *'
