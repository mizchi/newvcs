# Files Git is told not to see.
git init -q -b main repo && cd repo
printf '*.log\nbuild/\n!keep.log\n/only-at-root.txt\ntemp-*\n' > .gitignore
mkdir -p build/deep src/sub
echo x > app.log; echo x > keep.log; echo x > build/out.bin; echo x > build/deep/more.bin
echo x > only-at-root.txt; echo x > src/only-at-root.txt; echo x > src/main.c; echo x > src/sub/debug.log; echo x > temp-1
printf '*.tmp\n!important.tmp\n' > src/.gitignore
echo x > src/a.tmp; echo x > src/important.tmp; echo x > src/sub/b.tmp
echo "--- status"; git status --porcelain
echo "--- long"; git status
git add -A && git commit -q -m "what is not ignored"
echo "--- committed"; git ls-tree -r --name-only HEAD
echo "--- clean"; git status --porcelain
git add app.log >/dev/null 2>&1; echo "add an ignored file: $?"
echo changed > keep.log; echo y > build/new.bin; echo y > src/sub/c.tmp; echo y > src/new.c
echo "--- after more work"; git status --porcelain
echo x > tracked.log && git add -A && git status --porcelain
printf 'tracked-anyway.txt\n' >> .gitignore
echo t > tracked-anyway.txt
echo "--- an ignored file that is already tracked stays tracked"
git add keep.log && git commit -q -m more
echo more >> keep.log; git status --porcelain
