#!/bin/sh
# setup.sh <work-dir> <server-url> <tasks>: a small project on a server ref, and one fork of it
# for each task, each with a watcher running. Prints what each agent is to be told.
#
#   eval/commit-agents/setup.sh /tmp/round1 http://localhost:8799 3
set -eu
work=$1; server=$2; tasks=$3
nv=${NV:-$(cd "$(dirname "$0")/../.." && pwd)/target/fast/nv}
[ -x "$nv" ] || { echo "no nv at $nv (build with cargo build --profile fast)"; exit 2; }
rm -rf "$work"; mkdir -p "$work/bin"
# A copy: a build made while the round runs does not change what its agents use.
cp "$nv" "$work/bin/nv"
export PATH="$work/bin:$PATH" GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME=eval GIT_AUTHOR_EMAIL=eval@example.com GIT_COMMITTER_NAME=eval GIT_COMMITTER_EMAIL=eval@example.com
cd "$work"

# The project: a package of text utilities and its tests, which take a fraction of a second.
mkdir -p seed/textkit seed/tests && cd seed
printf '"""Small text utilities."""\n' > textkit/__init__.py
: > tests/__init__.py
cat > tests/test_package.py <<'PY'
import unittest

import textkit


class Package(unittest.TestCase):
    def test_imports(self):
        self.assertTrue(textkit.__doc__)


if __name__ == "__main__":
    unittest.main()
PY
printf '__pycache__/\n*.pyc\n' > .gitignore
cat > nv.toml <<'TOML'
auto = true                              # run the conditions behind every write the watcher records

[checks.unit]
run = "python3 -m unittest discover -q -s tests -t ."
timeout = 60

[commit]
each = true                              # commit each state that passes; made again before a push
TOML
printf '# textkit\n\nSmall text utilities.\n' > README.md
git init -q -b main && git add -A && git commit -qm "Start textkit"
cd ..

# The ref, started by a workspace of the project.
url="$server/eval/textkit-$(date +%s%N)"
nv git clone -q seed base
nv remote base "$url" main
(cd base && printf '\nA package to practise on.\n' >> README.md && nv commit . "Say what textkit is for" >/dev/null && nv push . >/dev/null)

# One fork for each task, a watcher in each.
i=1
while [ "$i" -le "$tasks" ]; do
  nv fork base "task-$i" >/dev/null
  nv watch "task-$i" --start >/dev/null
  i=$((i + 1))
done
echo "work=$work"
echo "url=$url"
echo "bin=$work/bin"
