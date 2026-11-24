# A file `rm --cached` took out of the index is not tracked: `reset --hard` does not take it away
# where the commit has none, and the rules of what is left out apply to it. (Twelfth round: nv
# removed it, edits and all, and `add -A` staged it again though it was ignored.)
git init -q -b main repo && cd repo
echo base > keep && git add keep && git commit -q -m one
echo precious > f && git add f && git commit -q -m two && git tag two
git rm -q --cached f && echo "edited after rm --cached" >> f
echo '== reset --hard to a commit without it'; git reset -q --hard HEAD~1; echo "exit $?"; cat f; git status --porcelain
# (Where the commit has it, Git writes over it and nv refuses: nv writes over nothing it does not track.)
rm f && git reset -q --hard two
# Out of the index and then ignored: the next commit takes it out, and it is ignored.
echo secret > secret.env && git add secret.env && git commit -q -m secret
git rm -q --cached secret.env && echo secret.env > .gitignore
echo '== status'; git status --porcelain
git add -A && git commit -q -m untrack
echo '== committed'; git show --name-status --format=%s HEAD; git status --porcelain; ls
mkdir -p d && echo x > d/x && git add d && git commit -q -m d && git rm -r -q --cached d && echo d/ >> .gitignore
echo '== an ignored directory'; git status --porcelain
# Let go of and ignored as well, it is still not reset --hard's to remove.
git add -A && git commit -q -m gitignore && echo s > s.log && git add s.log && git commit -q -m slog
git rm -q --cached s.log && echo s.log >> .gitignore && echo more >> s.log
echo '== ignored and let go'; git reset -q --hard HEAD~1; echo "exit $?"; cat s.log; git status --porcelain
