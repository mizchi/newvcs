# Things said every day that are easy to get wrong: names, paths, where a command is run.
git init -q -b main repo && cd repo
echo a > a.txt && mkdir -p "sub dir/deep" d && echo x > "sub dir/deep/x.txt" && echo c > d/c.txt
git add -A && git commit -q -m "sub
ject

body line"
# An empty message is not a commit.
git commit --allow-empty -m "" 2>&1; echo "exit $?"
# Names a branch or a tag can and cannot have.
for name in 'sp ace' 'a..b' 'bad~name' '' 'ok/name' 'end.lock' 'x^y' '.hidden' 'a/.b' 'with@{brace' 'fine-name_1.0'; do
  git branch "$name" 2>/dev/null; echo "branch [$name]: $?"
done
for name in 'bad~name' '' 'rel/1.0' 'v1'; do git tag "$name" 2>/dev/null; echo "tag [$name]: $?"; done
git branch feat/x; git branch feat 2>/dev/null; echo "branch feat beside feat/x: $?"
git branch ok 2>/dev/null; echo "branch ok beside ok/name: $?"
echo "--- branches"; git branch
echo "--- tags"; git tag
# Switching says where it went.
git switch ok/name 2>&1; git checkout main 2>&1; git switch -c new-one 2>&1; git checkout -b other-one 2>&1
git switch main 2>&1; git switch main 2>&1
# A path is a path without the dashes.
echo changed > a.txt; echo changed > d/c.txt
git checkout a.txt 2>&1; echo "a: $(cat a.txt)"
git checkout . 2>&1; echo "c: $(cat d/c.txt)"
git checkout nothing-like-this 2>/dev/null; echo "checkout of nothing: $?"
# From a subdirectory, paths are told from there.
echo y >> "sub dir/deep/x.txt"; echo new > d/new.txt
(cd "sub dir" && git status --short && git status --porcelain)
(cd d && git status --short)
(cd "sub dir/deep" && git status | sed -n '/Changes not staged/,$p')
# A subject is the first paragraph, on one line.
git log --format=%s
git log --oneline | cut -c9-
git log --pretty=format:%s; echo "<nothing ends the last"
git log --pretty=tformat:%s
git log --format='%s|%b|'
# What is below a directory.
git ls-tree -r --name-only HEAD d/
git ls-tree -r --name-only HEAD "sub dir"
git ls-tree --name-only HEAD d
# The start of an id is the id, and a file can be asked for at a commit.
full=$(git rev-parse HEAD); short=$(git rev-parse --short HEAD)
[ "$(git rev-parse "$short")" = "$full" ] && echo "short id resolves"
git show "$short:a.txt"
git show HEAD:d/c.txt
git cat-file -t "$short"
git log --format=%s "$short"
git reset -q --hard nosuch 2>/dev/null; echo "reset to nothing: $?"
git show nosuch:a.txt 2>/dev/null; echo "show of nothing: $?"
git show HEAD:no/such/file 2>/dev/null; echo "show of no file: $?"
