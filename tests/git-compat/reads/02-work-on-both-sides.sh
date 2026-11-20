# Git commits upstream while the clone has commits of its own: the pull is a
# merge, and what the clone pushes back is something Git accepts and reads.
# `$REAL_GIT -C "$ORIGIN"` asks the real Git about the upstream, in both runs.
#@ git
git init -q -b main . && git config user.name "Origin Author" && git config user.email origin@example.com
echo base > base.txt && git add -A && git commit -q -m base
#@ read
echo "start: $(git rev-parse HEAD) on $(git branch --show-current)"
echo "--- level with upstream"; git status
echo mine > mine.txt && git add -A && git commit -q -m "a commit in the clone"
echo "--- one ahead"; git status
#@ git
echo upstream > upstream.txt && git add -A && git commit -q -m "a commit upstream"
git checkout -q --detach
#@ read
echo "--- after the pull"; git log --format='%s|%p'
echo "--- the merge"; git cat-file -p HEAD | sed "s|$ORIGIN|ORIGIN|"
echo "--- files"; ls
echo "--- two ahead after merging"; git status
git push -q; echo "push: $?"
echo "--- level again"; git status
echo "--- upstream now"; "$REAL_GIT" -C "$ORIGIN" log --format='%H %s' main
"$REAL_GIT" -C "$ORIGIN" fsck --strict >/dev/null 2>&1; echo "git fsck of upstream: $?"
echo "--- upstream has the file: $("$REAL_GIT" -C "$ORIGIN" show main:mine.txt)"
#@ git
git checkout -q main
echo again > again.txt && git add -A && git commit -q -m "upstream again"
git checkout -q --detach
#@ read
echo "--- fast-forward this time"; git log -n 2 --format='%s|%p'
echo "--- status"; git status --porcelain; ls
git push -q; echo "push with nothing new: $?"
