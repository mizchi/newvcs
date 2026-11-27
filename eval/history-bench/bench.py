#!/usr/bin/env python3
"""bench.py: writers driven through nv by script, no agents, and what the ref's history comes to.

    eval/history-bench/bench.py --server http://localhost:8799 --nv A=path/to/nv --nv B=other/nv \\
        [--reps 3] [--work DIR] [--out results.tsv] [scenarios/*.json ...]

Each scenario (scenarios/*.json) is a seed and writers, each writer a list of steps, each step a
note and the edits it makes. Writers push in the order given; each commits its steps one by one
(`nv commit <dir> <note>`), pushes, and where the push is refused syncs, settles what was left
with both versions the way the scenario says, waits for the settling to be committed, and pushes
again. The ref's history is then read from a workspace that only watches.

What is measured, for every binary on the same scenario (one row of the TSV per run):

  commits       commits on the ref after the start
  steps         steps the writers made
  granularity   commits / steps: 1.0 where every step is a commit of its own
  attribution   of the lines the steps wrote that are at the tip, the part brought in by a commit
                whose summary line is the note of the step that wrote them
  folds         syncs that made several commits one
  refused       pushes refused
  trailers      commits on the ref with a `Settled:` trailer
  sync_ms       wall time of every `nv sync`, together
  push_ms       wall time of every `nv push`, together

No numbers of the binaries' own are compared: the same scenario gives each the same writes in
the same order. Times vary from run to run; give --reps and read the medians (compare.py).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
MARKERS = re.compile(r"<<<<<<< yours\n(.*?)=======\n(.*?)>>>>>>> theirs\n", re.S)
FIELDS = ["binary", "scenario", "rep", "commits", "steps", "granularity", "attribution", "folds", "refused", "trailers", "sync_ms", "push_ms"]


class Run:
    def __init__(self, nv: str, work: Path):
        self.nv, self.work = nv, work
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_AUTHOR_NAME="bench", GIT_AUTHOR_EMAIL="bench@example.com",
                        GIT_COMMITTER_NAME="bench", GIT_COMMITTER_EMAIL="bench@example.com")
        self.sync_ms = self.push_ms = 0.0
        self.folds = self.refused = 0

    def sh(self, args, cwd=None, ok=(0,)):
        out = subprocess.run(args, cwd=cwd or self.work, env=self.env, capture_output=True, text=True, timeout=300)
        if out.returncode not in ok:
            raise SystemExit(f"{' '.join(map(str, args))} exited {out.returncode}:\n{out.stdout}{out.stderr}")
        return out

    def nv_(self, *args, ok=(0,)):
        return self.sh([self.nv, *map(str, args)], ok=ok)

    def timed(self, what, *args, ok=(0,)):
        started = time.perf_counter()
        out = self.nv_(what, *args, ok=ok)
        ms = (time.perf_counter() - started) * 1000
        if what == "sync":
            self.sync_ms += ms
        else:
            self.push_ms += ms
        said = out.stdout + out.stderr
        if "now one commit" in said:
            self.folds += 1
        return out


def apply(root: Path, edit: dict, written: list, note: str):
    """Make one edit, and note each line it brought in as the step's."""
    path = root / edit["file"]
    path.parent.mkdir(parents=True, exist_ok=True)
    old = path.read_text() if path.exists() else ""
    if "write" in edit:
        new = edit["write"]
    elif "replace" in edit:
        was, now = edit["replace"]
        if was not in old:
            raise SystemExit(f"{edit['file']}: {was!r} is not there to replace")
        new = old.replace(was, now, 1)
    elif "append" in edit:
        new = old + edit["append"]
    elif "after" in edit:
        anchor, text = edit["after"]
        if anchor not in old:
            raise SystemExit(f"{edit['file']}: {anchor!r} is not there")
        new = old.replace(anchor, anchor + text, 1)
    else:
        raise SystemExit(f"an edit of {edit['file']} says nothing to do")
    path.write_text(new)
    before = old.splitlines()
    for line in new.splitlines():
        if line in before:
            before.remove(line)
        elif line.strip():
            written.append((edit["file"], line, note))


def settle(root: Path, path: str, how: str):
    """Settle a file left with both versions: the lines of both, as the scenario says."""
    file = root / path
    text = file.read_text()

    def both(m):
        ours, theirs = m.group(1).splitlines(True), m.group(2).splitlines(True)
        lines = theirs + [line for line in ours if line not in theirs]
        if how == "sorted":
            lines = sorted(set(lines))
        return "".join(lines)

    file.write_text(MARKERS.sub(both, text))


def one(nv: str, scenario: dict, work: Path, server: str) -> dict:
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    run = Run(nv, work)
    seed = work / "seed"
    seed.mkdir()
    files = dict(scenario["seed"])
    # Conditions that always pass, and a commit made of each state they pass on: what a
    # settling is committed by (`nv checkpoint`), as a runner would.
    files.setdefault("nv.toml", '[checks.unit]\nrun = "true"\n\n[commit]\neach = true\n')
    for name, text in files.items():
        (seed / name).parent.mkdir(parents=True, exist_ok=True)
        (seed / name).write_text(text)
    run.sh(["git", "init", "-q", "-b", "main"], cwd=seed)
    run.sh(["git", "add", "-A"], cwd=seed)
    run.sh(["git", "commit", "-qm", "Start"], cwd=seed)
    url = f"{server.rstrip('/')}/bench/{scenario['name']}-{time.time_ns()}"
    run.nv_("import", "seed", "store")
    run.nv_("checkout", "store", "HEAD", "base")
    run.nv_("remote", "base", url, "main")
    run.nv_("push", "base")
    writers = scenario["writers"]
    for w in writers:
        run.nv_("fork", "base", w["name"])
    run.nv_("fork", "base", "observer")

    written = []
    steps = 0
    # Every writer's steps are made before anyone pushes: they work at once.
    for w in writers:
        root = work / w["name"]
        for step in w["steps"]:
            for edit in step["edits"]:
                apply(root, edit, written, step["note"])
            run.nv_("commit", root, step["note"])
            steps += 1
    for w in writers:
        root = work / w["name"]
        for _ in range(5):
            out = run.timed("push", root, ok=(0, 2))
            if out.returncode == 0:
                break
            run.refused += 1
            out = run.timed("sync", root, ok=(0, 1))
            if out.returncode == 1:
                for path in run.sh(["sh", "-c", "grep -rl '^<<<<<<< yours' --exclude-dir=.nv . || true"], cwd=root).stdout.split():
                    settle(root, path.removeprefix("./"), w.get("settle", scenario.get("settle", "both")))
                run.nv_("checkpoint", root, ok=(0, 1))
                run.nv_("pending", root, "--wait")
        else:
            raise SystemExit(f"{w['name']} could not push in five tries")

    obs = work / "observer"
    run.nv_("sync", obs)
    log = run.nv_("git", "-C", obs, "log", "--format=%H%x00%s%x00%b%x01").stdout
    start = run.sh(["git", "rev-parse", "HEAD"], cwd=seed).stdout.strip()
    commits = []
    for entry in log.split("\x01"):
        if not entry.strip():
            continue
        oid, subject, body = entry.strip("\n").split("\x00")
        if oid == start:
            break
        commits.append((oid, subject, body))
    # Which commit brought in each line at the tip: the oldest whose diff adds it.
    added = {}
    for oid, subject, _ in reversed(commits):
        diff = run.nv_("git", "-C", obs, "show", "--format=", oid).stdout
        file = None
        for line in diff.splitlines():
            if line.startswith("+++ "):
                file = line[6:] if line.startswith("+++ b/") else None
            elif line.startswith("+") and file and not line.startswith("+++"):
                added.setdefault((file, line[1:]), subject)
    at_tip = [(f, line, note) for f, line, note in written if (obs / f).exists() and line in (obs / f).read_text().splitlines()]
    right = sum(1 for f, line, note in at_tip if added.get((f, line)) == note)
    return {
        "commits": len(commits),
        "steps": steps,
        "granularity": round(len(commits) / steps, 3) if steps else 0,
        "attribution": round(right / len(at_tip), 3) if at_tip else 1.0,
        "folds": run.folds,
        "refused": run.refused,
        "trailers": sum(1 for _, _, body in commits if "Settled:" in body),
        "sync_ms": round(run.sync_ms, 1),
        "push_ms": round(run.push_ms, 1),
    }


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--server", required=True)
    p.add_argument("--nv", action="append", required=True, help="LABEL=path to an nv binary; give it once for each to compare")
    p.add_argument("--reps", type=int, default=1)
    p.add_argument("--work", default=None, help="a scratch directory (removed run by run)")
    p.add_argument("--out", default=None, help="the TSV to append rows to (default: stdout)")
    p.add_argument("scenarios", nargs="*")
    a = p.parse_args()
    scenarios = a.scenarios or sorted(str(s) for s in (HERE / "scenarios").glob("*.json"))
    work = Path(a.work or os.environ.get("TMPDIR", "/tmp")) / "history-bench"
    out = open(a.out, "a") if a.out else sys.stdout
    if not a.out or os.path.getsize(a.out) == 0:
        print("\t".join(FIELDS), file=out, flush=True)
    for rep in range(1, a.reps + 1):
        for s in scenarios:
            scenario = json.loads(Path(s).read_text())
            # The binaries take turns within a repetition, so that a slow moment of the machine
            # does not fall on one of them only.
            for spec in a.nv:
                label, _, nv = spec.partition("=")
                row = one(str(Path(nv).resolve()), scenario, work / label, a.server)
                row.update(binary=label, scenario=scenario["name"], rep=rep)
                print("\t".join(str(row[f]) for f in FIELDS), file=out, flush=True)
    shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
