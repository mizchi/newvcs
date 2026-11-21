# What a seventh reviewer's random tester found, against Git doing the same.
git init -q -b main repo && cd repo
echo 'content of the repository' > b.txt && echo a > a.txt && git add . && git commit -q -m one
# A tracked file replaced by a link that leads nowhere, committed; going back writes the file, not through the link.
git branch b1 && rm b.txt && ln -s ../out/created b.txt && git commit -q -am link
git switch -q b1; echo "switch: $?"; cat b.txt; test -L b.txt && echo 'still a link'; ls .. | grep -c out; git status --short
# Two histories with nothing in common are not merged.
git checkout -q -b lone && git commit -q --amend -m 'another first commit' && git merge main >/dev/null 2>&1; echo "merge of unrelated: $?"
git switch -q main
# A merge that would have to make a directory where an untracked file stands touches nothing.
echo z > zz.txt && git add -A && git commit -q -m base
git switch -q -c w && echo a2 > a.txt && echo z2 > zz.txt && mkdir nd && echo x > nd/x && git add -A && git commit -q -m w
git switch -q main && echo m > mainonly && git add mainonly && git commit -q -m mainonly
echo precious > nd
git merge -q w >/dev/null 2>&1; test $? -ne 0 && echo 'merge refused'; cat a.txt zz.txt nd; git status --short
rm nd
# A file replaced by a directory, and the directory added: the file's removal goes in with it.
echo c0 > link && echo k > keep && git add -A && rm link && mkdir link && echo 1 > link/x && echo 2 > link/y
git commit -q -m one && git add link && git commit -q -m two; git status --short
echo more >> keep && git commit -q -am three && git ls-tree -r --name-only HEAD
# A hard reset where an untracked file stands in the place of a tracked directory.
git log -1 --format=%H
