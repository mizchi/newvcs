# What Git refuses, nv refuses, with the same exit code.
git init -q -b main repo && cd repo
git rev-parse HEAD >/dev/null 2>&1; echo "rev-parse before any commit: $?"
git commit -q -m "nothing" >/dev/null 2>&1; echo "commit with nothing: $?"
git add missing.txt >/dev/null 2>&1; echo "add a missing path: $?"
echo a > a.txt && git add -A && git commit -q -m one
git commit -q -m "nothing again" >/dev/null 2>&1; echo "commit with nothing staged: $?"
git commit -q --allow-empty -m "empty on purpose"; echo "allow-empty: $?"
echo "--- log"; git log --format='%s'
git rev-parse no-such-branch >/dev/null 2>&1; echo "unknown revision: $?"
git checkout -q no-such-branch >/dev/null 2>&1; echo "checkout unknown: $?"
git checkout -q -b side && echo side > a.txt && git commit -q -am side && git checkout -q main
echo local > a.txt
git checkout -q side >/dev/null 2>&1; echo "checkout over a local change: $?"
echo "still: $(cat a.txt) on $(git branch --show-current)"
