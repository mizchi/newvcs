# `git remote -v`, which a writer in the agent bench asked before anything else.
"$REAL_GIT" init -q -b main origin && "$REAL_GIT" -C origin commit -q --allow-empty -m one
git clone -q origin work && cd work
here=$(cd .. && pwd)
git remote; echo "exit: $?"
git remote -v | sed "s|$here|<here>|"
git remote get-url origin | sed "s|$here|<here>|"
cd .. && git init -q -b main lone && cd lone
echo "--- none"; git remote -v; echo "exit: $?"
