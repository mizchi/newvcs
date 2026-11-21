# What an eighth reviewer's random tester found, against Git doing the same.
git init -q -b main repo && cd repo
# A file that became a directory, and one file inside it added: the old file's removal goes in with it.
echo one > f && git add f && git commit -q -m one
rm f && mkdir f && echo precious > f/in1 && git add f/in1 && git commit -q -m 'f is a directory now'
git status --short; git ls-tree -r --name-only HEAD
git commit -q -a -m 'nothing to say' >/dev/null 2>&1; echo "commit -a with nothing changed: $?"; git ls-tree -r --name-only HEAD
# A merge that stops for a conflict, where the other side also made a directory a file.
mkdir e && echo k > e/k && seq 1 8 > x && echo a > a && git add . && git commit -q -m base
git switch -q -c side && rm -r e && echo 'now a file' > e && echo new > anew && echo a2 > a && sed 's/^3$/3 side/' x > x.new && mv x.new x && git add -A && git commit -q -m side
git switch -q main && sed 's/^3$/3 main/' x > x.new && mv x.new x && git commit -q -am main
git merge side >/dev/null 2>&1; echo "merge: $?"; git status --short; cat a e
# In the middle of that merge.
git switch -c during >/dev/null 2>&1; echo "switch -c mid-merge: $?"
git reset --soft HEAD~1 >/dev/null 2>&1; echo "reset --soft mid-merge: $?"
git restore x 2>&1; echo "restore of an unmerged path: $?"
git merge --abort; git status --short; cat a; ls e
