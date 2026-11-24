# `mv` into the top of the tree, by `.` or `..`, and of the top itself. (Twelfth round: the file
# went to the root of the file system.)
git init -q -b main repo && cd repo
mkdir -p d/e && echo f > d/f && echo g > d/e/g && echo t > t && git add -A && git commit -q -m base
cd d
echo '== mv f ..'; git mv f ..; echo "exit $?"; ls ..; git status --porcelain
echo '== mv e ..'; git mv e ..; echo "exit $?"; git status --porcelain
cd ..
echo '== mv t .'; git mv t .; echo "exit $?"
echo '== mv . x'; git mv . x; echo "exit $?"
mkdir -p k && echo k > k/k && git add k
cd k
echo '== mv . ../m'; git mv . ../m; echo "exit $?"
cd ..
ls; git status --porcelain
