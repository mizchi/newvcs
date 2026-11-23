#!/bin/sh
# publish.sh <work-dir> <n>: what stands in for a committing agent (pattern C+D): wait for task n's
# developer to be done, then `nv publish` its workspace, the line it wrote for the last commit.
# Prints the milliseconds from its start to the end of the publish, and the publish's output.
set -u
work=$1; n=$2
start=$(date +%s%3N)
until [ -e "$work/task-$n.done" ]; do sleep 0.2; done
"$work/bin/nv" publish "$work/task-$n" --message "$(head -1 "$work/task-$n.done" | cut -c1-200)" > "$work/publish-$n.out" 2>&1
code=$?
end=$(date +%s%3N)
echo "ms=$((end - start)) code=$code"
cat "$work/publish-$n.out"
