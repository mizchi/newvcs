#!/bin/sh
# verify.sh <work-dir> <ref-url>: what came to be on the ref, from a fresh workspace of it.
# For each commit after the start: its message, its files, and whether the tests pass on it.
set -u
work=$1; url=$2
export PATH="$work/bin:$PATH" GIT_CONFIG_GLOBAL=/dev/null
cd "$work" || exit 2
rm -rf verify && nv git clone -q seed verify && nv remote verify "$url" main && nv sync verify >/dev/null || { echo "cannot sync with the ref"; exit 2; }
start=$(git -C seed rev-parse HEAD)
total=0; passing=0; provisional=0
for oid in $(cd verify && nv git log --format=%H | tac); do
  [ "$oid" = "$start" ] && continue
  total=$((total + 1))
  subject=$(cd verify && nv git log -1 --format=%s "$oid")
  rm -rf at && nv checkout verify/.nv/store "$oid" at >/dev/null 2>&1
  if (cd at && python3 -m unittest discover -q -s tests -t . >/dev/null 2>&1); then result=pass; passing=$((passing + 1)); else result=FAIL; fi
  case "$subject" in "checkpoint: "*) provisional=$((provisional + 1)) ;; esac
  files=$(cd verify && nv git show --name-only --format= "$oid" | tr '\n' ' ')
  printf '%s  %-4s  %s\n      %s\n' "$(echo "$oid" | cut -c1-8)" "$result" "$subject" "$files"
done
rm -rf at
markers=$(grep -rl '^<<<<<<< ' verify --include='*.py' --include='*.md' 2>/dev/null | grep -v '/.nv/' | wc -l)
echo "commits: $total, passing their tests: $passing, provisional messages left: $provisional, files with markers at the tip: $markers"
echo "files at the tip: $(cd verify && find . -type f -not -path "./.nv/*" -not -path "*/__pycache__/*" | sort | tr "\n" " ")"
