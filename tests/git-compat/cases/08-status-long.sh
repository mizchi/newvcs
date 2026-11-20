# Plain `git status`, in each state a day of work passes through.
git init -q -b main repo && cd repo
echo "--- nothing yet"; git status
echo a > a.txt
echo "--- only untracked, no commits"; git status
git add a.txt
echo "--- added, no commits"; git status
git commit -q -m one
echo "--- clean"; git status
echo b > b.txt
echo "--- only untracked"; git status
echo changed > a.txt
echo "--- modified, not added"; git status
git add a.txt
echo "--- modified and added, and untracked"; git status
git add -A && git commit -q -m two
rm a.txt; echo more >> b.txt; echo c > c.txt; git add c.txt
echo "--- deleted, modified, new"; git status
git checkout -q -b topic
echo "--- on another branch"; git status
