# What status says about every kind of difference.
git init -q -b main repo && cd repo
echo one > tracked.txt; echo two > gone.txt; mkdir -p dir && echo three > dir/inside.txt
git add -A && git commit -q -m base
echo "--- clean"; git status --porcelain
echo changed > tracked.txt
rm gone.txt
echo new > untracked.txt
mkdir -p fresh/deep && echo x > fresh/deep/file.txt
echo y > dir/also-new.txt
echo "--- unstaged"; git status --porcelain
git add tracked.txt untracked.txt
echo "--- some staged"; git status --porcelain
git add -A
echo "--- all staged"; git status --short
