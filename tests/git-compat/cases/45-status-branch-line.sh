# `status -sb`, which writers in the agent bench ran after a fetch: the branch and what it
# publishes to on the first line, and how far apart the two are.
"$REAL_GIT" init -q -b main origin && cd origin
echo a > a.txt && "$REAL_GIT" add a.txt && "$REAL_GIT" commit -q -m one && cd ..
git clone -q origin work && cd work
echo "--- up to date"; git status -sb; git status --short --branch; git status --porcelain -b
echo b > b.txt; echo "--- untracked"; git status -sb
git add b.txt && git commit -q -m two
echo "--- ahead"; git status -sb; git status -b
cd ../origin && echo a2 >> a.txt && "$REAL_GIT" commit -q -am three && cd ../work
git fetch -q
echo "--- diverged"; git status -bs
git checkout -q -b lone
echo "--- no upstream"; git status -sb
cd .. && git init -q -b main empty && cd empty
echo "--- no commits"; git status -sb
