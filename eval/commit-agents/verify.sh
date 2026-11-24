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
# Every public function of a module of the package is exported from it, and named in `__all__`
# where there is one: what a merge that kept both sides' lines can get wrong while the tests pass.
missing=$(cd verify && python3 - <<'PY'
import ast, importlib, pathlib
pkg = importlib.import_module("textkit")
names = [n.name for f in sorted(pathlib.Path("textkit").glob("*.py")) if f.name != "__init__.py"
         for n in ast.parse(f.read_text()).body if isinstance(n, ast.FunctionDef) and not n.name.startswith("_")]
listed = getattr(pkg, "__all__", None)
print(" ".join(n for n in names if not hasattr(pkg, n) or (listed is not None and n not in listed)))
PY
)
echo "exports missing at the tip: ${missing:-none}"
# Where the README lists the functions (CONFLICT=1), every one is listed and the placeholder is gone.
if grep -q '^## Functions' verify/README.md; then
  unlisted=$(cd verify && python3 - <<'PY'
import ast, pathlib
names = [n.name for f in sorted(pathlib.Path("textkit").glob("*.py")) if f.name != "__init__.py"
         for n in ast.parse(f.read_text()).body if isinstance(n, ast.FunctionDef) and not n.name.startswith("_")]
readme = pathlib.Path("README.md").read_text()
section = readme.split("## Functions", 1)[1].split("\n## ", 1)[0]
missing = [n for n in names if n not in section] + (["(none yet)"] if "(none yet)" in section else [])
print(" ".join(missing))
PY
)
  echo "functions missing from the README: ${unlisted:-none}"
fi
echo "files at the tip: $(cd verify && find . -type f -not -path "./.nv/*" -not -path "*/__pycache__/*" | sort | tr "\n" " ")"
