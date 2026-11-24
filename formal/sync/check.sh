#!/bin/sh
# Check the model of pushing and syncing with idealis (https://github.com/mizchi/idealis).
#
#   formal/sync/check.sh [config ...]      (all of them when none is named)
#
# Each configuration is run twice: as it is, with the fixes nv has, where no invariant may be
# violated; and with `Fixed = {}`, where the counterexample it was written to reach has to be
# found again, so that the model still sees the bug its fix is for.
#
# The configurations are larger than idealis's built-in search bounds and its default IR take.
# This was run with a local build of idealis that reads them from the environment
# (IDEALIS_IR=2, IDEALIS_MAX_STATES, IDEALIS_MAX_WORK as a 64-bit count) and evaluates to a
# nesting of 250; see "Checking the model" in formal/sync/README.md.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here" || exit 1
scratch="$(mktemp -d "${TMPDIR:-/tmp}/nv-sync-model.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
tlc() {
  RUST_MIN_STACK=536870912 IDEALIS_IR=2 IDEALIS_MAX_STATES=4000000000 IDEALIS_MAX_WORK=1000000000000 \
    idealis tlc -workers auto -config "$1" MC.tla
}
configs="$*"
[ -n "$configs" ] || configs="Landed Back-amend Back-aside Back-before-sync SameKeys"
failed=0
for name in $configs; do
  out="$(tlc "$name.cfg" 2>&1)"
  summary="$(printf '%s\n' "$out" | tail -1)"
  case "$out" in
    *"Model checking completed. No error has been found."*) echo "ok       $name: $summary" ;;
    *) echo "FAILED   $name: $(printf '%s\n' "$out" | head -1) ($summary)"; failed=$((failed + 1)) ;;
  esac
  sed 's/^  Fixed = .*/  Fixed = {}/' "$name.cfg" > "$scratch/$name-unfixed.cfg"
  cp MC.tla Sync.tla "$scratch/"
  out="$(cd "$scratch" && tlc "$name-unfixed.cfg" 2>&1)"
  case "$out" in
    *"is violated"*) echo "         $name without the fixes: $(printf '%s\n' "$out" | head -1) ($(printf '%s\n' "$out" | tail -1))" ;;
    *) echo "FAILED   $name without the fixes found nothing: $(printf '%s\n' "$out" | tail -1)"; failed=$((failed + 1)) ;;
  esac
done
exit "$failed"
