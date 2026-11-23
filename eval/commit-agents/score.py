#!/usr/bin/env python3
"""score.py <round-dir> [<reference-round-dir>]: the scores of a round, and against another.

A round directory holds two files beside the workspaces `setup.sh` made:

  verify.txt  what `verify.sh <round-dir> <ref-url>` printed
  usage.tsv   one line per agent, as the orchestrator was told when each finished:
              role (worker, committer, script, hook or baseline), task, tokens, tool uses (for a hook,
              its calls), milliseconds,
              and for a committer the pushes it had refused (from its report); for a script,
              also the milliseconds from the developer's done file to its end

    baseline  -  67316  0   4551    -
    worker    1  73125  4   35387   -
    committer 1  79725  9   82114   0

The baseline is an agent told to answer "OK" and use no tools: what any agent costs before it
does anything. Tokens are counted above it.

Gates come first: every commit on the ref passes its tests, no provisional message and no
conflict marker reached the ref, every public function is exported from the package (what a
merge that kept both sides' lines can get wrong while the tests pass), and there is a commit
for every task. A round that fails one
has no score: speed and tokens are not traded for what reaches the ref.

Scores, lower is better:
  makespan     from launch until the last committer was done, in seconds: every pair is
               launched at once, so this is the longest committer
  latency      for each task, how long after its developer was done its work was on the ref:
               the committer's time less the worker's (both start together); the median
  vcs tokens   tokens the committers used above the baseline, in all
  dev tokens   the same for the workers: version control should not cost them anything
  vcs share    vcs tokens as a part of vcs and dev tokens together
  vcs calls    the committers' tool calls, in all; each call is a turn
  total tokens what every agent of the round used, the cost of an agent's start included: what
               the round costs. A `script` row (publish.sh standing in for a committer) is none
"""
import re
import statistics
import sys
from pathlib import Path


def load(round_dir):
    rows = []
    for line in (Path(round_dir) / "usage.tsv").read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        role, task, tokens, calls, ms, *rest = line.split()
        rows.append(dict(role=role, task=task, tokens=int(tokens), calls=int(calls), ms=int(ms),
                         refused=int(rest[0]) if rest and rest[0] != "-" else 0,
                         after=int(rest[1]) if len(rest) > 1 else None))
    return rows


def gates(round_dir, tasks):
    text = (Path(round_dir) / "verify.txt").read_text()
    found = re.search(r"commits: (\d+), passing their tests: (\d+), provisional messages left: (\d+), "
                      r"files with markers at the tip: (\d+)", text)
    if not found:
        return {"verify.txt has its summary line": False}
    commits, passing, provisional, markers = map(int, found.groups())
    exports = re.search(r"exports missing at the tip: (.*)", text)
    exports = exports.group(1).strip() if exports else "none"
    return {
        f"every function exported ({'all' if exports == 'none' else 'missing ' + exports})": exports == "none",
        f"every commit passes its tests ({passing} of {commits})": passing == commits,
        f"no provisional message pushed ({provisional})": provisional == 0,
        f"no markers at the tip ({markers})": markers == 0,
        f"a commit for every task ({commits} for {tasks})": commits >= tasks,
    }


def scores(round_dir):
    rows = load(round_dir)
    base = [r for r in rows if r["role"] == "baseline"]
    if not base:
        sys.exit(f"{round_dir}/usage.tsv has no baseline line")
    base = base[0]["tokens"]
    workers = {r["task"]: r for r in rows if r["role"] == "worker"}
    # A script that stands in for a committing agent (`publish.sh`) costs no tokens at all.
    committers = {r["task"]: r for r in rows if r["role"] in ("committer", "script")}
    above = lambda r: 0 if r["role"] == "script" else r["tokens"] - base
    held = gates(round_dir, len(committers))
    # A script says how long after the done file it was done (its `after`); an agent's time less
    # the worker's is that, as both start together.
    lag = lambda t: committers[t]["after"] if committers[t]["after"] is not None else committers[t]["ms"] - workers[t]["ms"]
    latency = [lag(t) for t in committers if t in workers]
    vcs = sum(above(r) for r in committers.values())
    dev = sum(above(r) for r in workers.values())
    # A hook (`body.sh`) has no agent's start to cost: what its calls used is counted whole.
    hooks = sum(r["tokens"] for r in rows if r["role"] == "hook")
    vcs += hooks
    total = sum(r["tokens"] for r in rows if r["role"] in ("worker", "committer")) + hooks
    return held, {
        "makespan s": max(r["ms"] for r in committers.values()) / 1000,
        "latency s": statistics.median(latency) / 1000,
        "vcs tokens": vcs,
        "dev tokens": dev,
        "vcs share %": 100 * vcs / (vcs + dev),
        "vcs calls": sum(r["calls"] for r in committers.values()) + sum(r["calls"] for r in rows if r["role"] == "hook"),
        "total tokens": total,
    }, {
        "refused pushes": sum(r["refused"] for r in committers.values()),
        "latency per task s": ", ".join(f"{t}: {lag(t) / 1000:.1f}" for t in sorted(committers) if t in workers),
        "vcs tokens per committer": ", ".join(f"{t}: {above(committers[t])}" for t in sorted(committers)),
    }


def main():
    held, score, context = scores(sys.argv[1])
    for gate, ok in held.items():
        print(f"{'pass' if ok else 'FAIL'}  {gate}")
    if not all(held.values()):
        print("no score: a gate failed")
        sys.exit(1)
    reference = None
    if len(sys.argv) > 2:
        ref_held, reference, _ = scores(sys.argv[2])
        if not all(ref_held.values()):
            sys.exit(f"{sys.argv[2]} failed a gate: nothing to compare with")
    for name, value in score.items():
        line = f"{name:<12} {value:>10.1f}" if isinstance(value, float) else f"{name:<12} {value:>10}"
        if reference:
            was = reference[name]
            line += f"   was {was:.1f}" if isinstance(was, float) else f"   was {was}"
            line += f"   {100 * (value - was) / was:+.0f}%" if was else ""
        print(line)
    for name, value in context.items():
        print(f"({name}: {value})")


if __name__ == "__main__":
    main()
