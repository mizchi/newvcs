#!/usr/bin/env bash
# Run scripts written for Git twice, once with Git and once with nv's `git`,
# and compare what they print and how they exit. Then check that the
# commands nv does not speak fail and say so.
#
#   tools/git-compat.sh <path-to-nv> [case.sh ...]
set -u
nv="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; shift
here="$(cd "$(dirname "$0")/.." && pwd)"
real_git="$(command -v git)"
work="$(mktemp -d "${TMPDIR:-/tmp}/nv-git-compat.XXXXXX")"
# KEEP=1 leaves the directory behind, to look at what each side printed.
if [ -n "${KEEP:-}" ]; then echo "kept in $work"; else trap 'rm -rf "$work"' EXIT; fi

# The same author, committer and clock for both, so that commit ids can agree.
run() { # <label> <dir> <script>
  mkdir -p "$2"
  (
    cd "$2" || exit 97
    export GIT_AUTHOR_NAME="A U Thor" GIT_AUTHOR_EMAIL="author@example.com" GIT_AUTHOR_DATE="1700000000 +0900"
    export GIT_COMMITTER_NAME="C O Mitter" GIT_COMMITTER_EMAIL="committer@example.com" GIT_COMMITTER_DATE="1700000100 -0500"
    export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null HOME="$work/home" TZ=UTC LC_ALL=C
    export ORIGIN="${ORIGIN:-}" REAL_GIT="$real_git"
    if [ "$1" = nv ]; then
      export NV_BIN="$nv" NV_REAL_GIT="$real_git" PATH="$here/tools/git-shim/bin:$PATH"
    else
      export PATH="$(dirname "$real_git"):$PATH"
    fi
    sh "$3"
  )
}

cases=("$@")
if [ ${#cases[@]} -eq 0 ]; then
  while read -r line; do
    case "$line" in ''|\#*) ;; *) cases+=("$line") ;; esac
  done < "$here/tools/git-compat-allowlist.txt"
fi

failed=0
for name in "${cases[@]}"; do
  script="$here/tests/git-compat/cases/$name"
  run git "$work/$name/git" "$script" > "$work/$name.git.out" 2> "$work/$name.git.err"; git_exit=$?
  run nv "$work/$name/nv" "$script" > "$work/$name.nv.out" 2> "$work/$name.nv.err"; nv_exit=$?
  if [ "$git_exit" = "$nv_exit" ] && cmp -s "$work/$name.git.out" "$work/$name.nv.out"; then
    echo "ok       $name ($(wc -l < "$work/$name.git.out" | tr -d ' ') lines)"
  else
    failed=$((failed + 1))
    echo "FAILED   $name (git exited $git_exit, nv exited $nv_exit)"
    diff "$work/$name.git.out" "$work/$name.nv.out" | head -20 | sed 's/^/           /'
    head -5 "$work/$name.nv.err" | sed 's/^/           nv: /'
  fi
done

# What Git wrote, nv reads. Each file under tests/git-compat/reads is a run
# of sections: `#@ git` is run by Git in an upstream repository, and after it
# `#@ read` is run in a clone of that repository, cloned the first time and
# pulled after. The whole run is done twice, the clone being Git's and then
# nv's, each against an upstream of its own at the same path (a pull's merge
# commit names that path), and the two must print the same. `$ORIGIN` is the
# upstream and `$REAL_GIT` the real Git, for asking it what the upstream has.
for script in "$here"/tests/git-compat/reads/*.sh; do
  name="reads/$(basename "$script")"
  dir="$work/$name"; mkdir -p "$dir"
  awk -v dir="$dir" '/^#@ /{n++; kind=$2; file=sprintf("%s/part-%02d-%s.sh", dir, n, kind); next} n{print > file}' "$script"
  broke=""
  for who in git nv; do
    rm -rf "$dir/origin"; mkdir -p "$dir/origin"; : > "$dir/$who.out"; cloned=0
    for part in "$dir"/part-*.sh; do
      case "$part" in
        *-git.sh)
          run git "$dir/origin" "$part" > "$dir/setup.out" 2>&1 || { broke="Git could not run $(basename "$part"): $(tail -1 "$dir/setup.out")"; break 2; } ;;
        *-read.sh)
          if [ "$cloned" = 0 ]; then
            echo "git clone -q \"$dir/origin\" clone" > "$dir/clone.sh"
            run "$who" "$dir/$who" "$dir/clone.sh" >> "$dir/$who.out" 2>> "$dir/$who.err" || echo "clone failed" >> "$dir/$who.out"
            cloned=1
          else
            echo "git pull -q --no-rebase" > "$dir/pull.sh"
            run "$who" "$dir/$who/clone" "$dir/pull.sh" >> "$dir/$who.out" 2>> "$dir/$who.err" || echo "pull failed" >> "$dir/$who.out"
          fi
          ORIGIN="$dir/origin" run "$who" "$dir/$who/clone" "$part" >> "$dir/$who.out" 2>> "$dir/$who.err" ;;
      esac
    done
  done
  if [ -z "$broke" ] && cmp -s "$dir/git.out" "$dir/nv.out"; then
    echo "ok       $name ($(wc -l < "$dir/git.out" | tr -d ' ') lines)"
  else
    failed=$((failed + 1))
    echo "FAILED   $name $broke"
    diff "$dir/git.out" "$dir/nv.out" | head -24 | sed 's/^/           /'
    head -5 "$dir/nv.err" | sed 's/^/           nv: /'
  fi
done

# A repository to refuse things in.
unsupported_repo="$work/unsupported"
cat > "$work/setup.sh" <<'SETUP'
git init -q -b main repo && cd repo
echo a > a.txt && git add -A && git commit -q -m one
echo b >> a.txt && git commit -q -am two
git branch side
SETUP
run nv "$unsupported_repo" "$work/setup.sh" || { echo "FAILED   could not prepare a repository"; failed=$((failed + 1)); }
refused=0
while read -r line; do
  case "$line" in ''|\#*) continue ;; esac
  sub="${line%% *}"
  # It has to be something Git itself knows, or the row proves nothing.
  "$real_git" "$sub" -h >/dev/null 2>&1; known=$?
  if [ "$known" != 129 ] && [ "$known" != 0 ]; then
    failed=$((failed + 1)); echo "FAILED   git $line: Git does not know \`$sub\` either"; continue
  fi
  echo "cd repo && git $line" > "$work/one.sh"
  run nv "$unsupported_repo" "$work/one.sh" > "$work/one.out" 2> "$work/one.err"; code=$?
  if [ "$code" != 0 ] && grep -q "not supported" "$work/one.err"; then
    refused=$((refused + 1))
  else
    failed=$((failed + 1))
    echo "FAILED   git $line: expected a refusal, got exit $code: $(head -1 "$work/one.err")"
  fi
done < "$here/tools/git-compat-unsupported.txt"
echo "refused  $refused commands nv does not speak"
# Refusing must not have changed anything.
echo 'cd repo && git status --porcelain && git log --format=%s && git branch' > "$work/after.sh"
after="$(run nv "$unsupported_repo" "$work/after.sh" 2>&1)"
expected="$(printf 'two\none\n* main\n  side')"
if [ "$after" != "$expected" ]; then
  failed=$((failed + 1)); echo "FAILED   the repository changed while refusing:"; echo "$after" | sed 's/^/           /'
fi

[ "$failed" = 0 ] && echo "all compatible" || echo "$failed failed"
exit "$failed"
