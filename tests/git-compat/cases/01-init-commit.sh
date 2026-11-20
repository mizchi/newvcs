# A first commit, and the three ways of looking at it.
git init -q -b main repo && cd repo
echo "hello" > README.md
mkdir -p src && printf 'fn main() {}\n' > src/main.rs
git add -A
git commit -q -m "first commit"
echo "head: $(git rev-parse HEAD)"
echo "branch: $(git rev-parse --abbrev-ref HEAD)"
echo "--- commit"; git cat-file -p HEAD
echo "--- type: $(git cat-file -t HEAD)"
echo "--- tree"; git ls-tree -r HEAD
echo "--- log"; git log --format='%H %an <%ae> %s'
echo "--- status"; git status --porcelain
