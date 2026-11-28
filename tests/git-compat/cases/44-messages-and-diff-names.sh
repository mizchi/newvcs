# What writers asked for in the agent bench, given Git's prompt: a message of paragraphs (`-m` more
# than once), a message from a file or standard input (`-F`), `diff` of a path named without
# `--`, and `--diff-filter`, among them `U` for the files a merge left unmerged.
git init -q -b main repo && cd repo
echo a > a.txt && echo b > b.txt && git add -A && git commit -q -m one
echo a2 >> a.txt && git commit -q -am "Change a" -m "A body, a paragraph of its own." -m "And a third."
git log -1 --format=%B
printf 'From a file\n\nWith a body.\n' > ../message && echo a3 >> a.txt && git commit -q -a -F ../message
git log -1 --format=%B
echo a4 >> a.txt && git commit -q -a -F - <<'MSG'
From standard input

Its body.
MSG
git log -1 --format=%B
echo a5 >> a.txt && git commit -q -a --file=../message; echo "file=: $?"; git log -1 --format=%s
git commit -q -a -m x -F ../message; echo "-m and -F: $?"
echo b2 >> b.txt && echo c > c.txt && git add c.txt
echo "--- diff b.txt"; git diff b.txt
echo "--- diff HEAD b.txt"; git diff HEAD b.txt
echo "--- diff of a name nobody has"; git diff nowhere.txt; echo "exit: $?"
git branch b.txt
echo "--- diff of a name that is a branch and a file"; git diff b.txt; echo "exit: $?"
git branch -D -q b.txt
echo "--- filters"; git diff HEAD --name-status --diff-filter=A; git diff HEAD --name-status --diff-filter=M; git diff HEAD --name-only --diff-filter=AM
git commit -q -am "More"
git checkout -q -b side && echo side > b.txt && git commit -q -am side
git checkout -q main && echo main > b.txt && git commit -q -am main
git merge -q side; echo "merge: $?"
echo "--- unmerged"; git diff --name-only --diff-filter=U
