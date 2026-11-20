# Executables, links, and names whose order differs between files and directories.
git init -q -b main repo && cd repo
printf '#!/bin/sh\necho hi\n' > run.sh && chmod +x run.sh
ln -s run.sh link
echo x > a.b; mkdir a && echo y > a/inner; echo z > "a b"
mkdir -p "dir with space/deeper" && echo w > "dir with space/deeper/f.txt"
echo "日本語" > "名前.txt"
git add -A && git commit -q -m names
echo "head: $(git rev-parse HEAD)"
echo "--- recursive"; git ls-tree -r HEAD
echo "--- top level"; git ls-tree HEAD
echo "--- the tree object"; git cat-file -p "$(git rev-parse 'HEAD^{tree}')"
echo "--- a blob: $(git cat-file -t "$(git ls-tree HEAD run.sh | cut -c13-52)")"
git cat-file -p "$(git ls-tree HEAD run.sh | cut -c13-52)"
chmod -x run.sh
echo "--- mode change"; git status --porcelain
