#!/usr/bin/env python3
"""run.py: one round of agents given a task of several parts, and what it cost to get there.

    eval/agent-bench/run.py --task strkit --pattern supervisor --nv target/fast/nv \\
        --server http://localhost:8799 --work /tmp/round --out /tmp/results.tsv [--label now] [--model M]

Patterns:
  supervisor  one agent makes every commit and push (prompts/supervisor.md); the developers
              only write, test and note (prompts/worker-supervised.md); a watcher in each
              workspace, the tests behind every write, a commit at each state that passes
  nv          each developer commits, pushes and syncs with nv itself (prompts/worker-nv.md)
  parallel    the supervisor's work in two: a planner that only tells the developers how to keep
              apart and writes the rules of settling (prompts/supervisor-plan.md), then an
              integrator for each developer (prompts/integrator.md), all at once
  git         each developer commits, pushes and pulls with Git, in a clone of a bare
              repository (prompts/worker-git.md): what the others are measured against

Every agent is `claude -p` with no settings of the machine's (no hooks, no MCP servers), the
file and shell tools only, and its transcript kept (agent-*.jsonl in the work directory).

What came of it is read from the ref, not from what the agents said: each commit after the start
and whether the project's tests pass on it, and at the tip the task's acceptance tests, which no
agent sees. One row per round goes to --out (a TSV), and everything to <work>/result.json.
"""
import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
TOOLS = ["Bash", "Read", "Write", "Edit", "Glob", "Grep"]
FIELDS = [
    "label", "pattern", "task", "model", "accept_passed", "accept_total", "reached", "makespan_s", "first_start_s",
    "tokens", "output_tokens", "cost_usd", "turns", "tool_calls", "vcs_calls", "refused", "clashes",
    "commits", "commits_passing", "provisional", "markers", "timed_out", "agents",
]
REFUSED = ("the push was refused", "[rejected]", "! [remote rejected]")
CLASHED = ("both sides changed the same lines", "CONFLICT (")


def sh(args, cwd, env, ok=(0,), timeout=600):
    out = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
    if out.returncode not in ok:
        raise SystemExit(f"{' '.join(map(str, args))} (in {cwd}) exited {out.returncode}:\n{out.stdout}{out.stderr}")
    return out


def fill(template: str, values: dict) -> str:
    for key, value in values.items():
        template = template.replace("{{" + key + "}}", value)
    return template


class Round:
    def __init__(self, a):
        self.a = a
        self.task = json.loads((HERE / "tasks" / a.task / "tasks.json").read_text())
        self.work = Path(a.work).resolve()
        self.n = len(self.task["tasks"])
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_AUTHOR_NAME="bench", GIT_AUTHOR_EMAIL="bench@example.com",
                        GIT_COMMITTER_NAME="bench", GIT_COMMITTER_EMAIL="bench@example.com")
        self.bin = self.work / "bin"
        self.nv = str(self.bin / "nv")
        self.env["PATH"] = f"{self.bin}:{self.env['PATH']}"

    def dirs(self):
        return [self.work / f"task-{i}" for i in range(1, self.n + 1)]

    # The project, on a ref every workspace pushes to.
    def setup(self):
        shutil.rmtree(self.work, ignore_errors=True)
        self.bin.mkdir(parents=True)
        shutil.copy2(self.a.nv, self.nv)
        seed = self.work / "seed"
        shutil.copytree(HERE / "tasks" / self.a.task / "seed", seed)
        if self.a.pattern in ("supervisor", "parallel"):
            (seed / "nv.toml").write_text(
                "auto = true                              # run the conditions behind every write the watcher records\n\n"
                f'[checks.unit]\nrun = "{self.task["test"]}"\ntimeout = 60\n\n'
                "[commit]\neach = true                              # commit each state that passes; made again before a push\n"
            )
        e, w = self.env, self.work
        sh(["git", "init", "-q", "-b", "main"], seed, e)
        sh(["git", "add", "-A"], seed, e)
        sh(["git", "commit", "-qm", f"Start {self.task['name']}"], seed, e)
        self.start = sh(["git", "rev-parse", "HEAD"], seed, e).stdout.strip()
        if self.a.pattern == "git":
            sh(["git", "init", "-q", "--bare", "-b", "main", "origin.git"], w, e)
            sh(["git", "push", "-q", str(w / "origin.git"), "main"], seed, e)
            for i, d in enumerate(self.dirs(), 1):
                sh(["git", "clone", "-q", str(w / "origin.git"), d.name], w, e)
            return
        self.url = f"{self.a.server.rstrip('/')}/bench/{self.task['name']}-{time.time_ns()}"
        sh([self.nv, "git", "clone", "-q", "seed", "base"], w, e)
        sh([self.nv, "remote", "base", self.url, "main"], w, e)
        sh([self.nv, "push", "base"], w, e)
        for d in self.dirs():
            sh([self.nv, "fork", "base", d.name], w, e)
            if self.a.pattern in ("supervisor", "parallel"):
                sh([self.nv, "watch", d.name, "--start"], w, e)
        sh([self.nv, "fork", "base", "observer"], w, e)

    def prompts(self):
        common, test = self.task["common"], self.task["test"]
        base = {"BIN": str(self.bin), "WORK": str(self.work), "COMMON": common, "TEST": test, "N": str(self.n)}
        worker = {"supervisor": "worker-supervised.md", "parallel": "worker-supervised.md", "nv": "worker-nv.md", "git": "worker-git.md"}[self.a.pattern]
        template = (HERE / "prompts" / worker).read_text()
        out = {}
        for i, (d, task) in enumerate(zip(self.dirs(), self.task["tasks"]), 1):
            out[f"worker-{i}"] = (d, fill(template, {**base, "DIR": str(d), "TASK": task, "DONE": str(self.work / f"task-{i}.done")}))
        if self.a.pattern in ("supervisor", "parallel"):
            tasks = "\n".join(f"{i}. {t}" for i, t in enumerate(self.task["tasks"], 1))
            spaces = ", ".join(f"`{d}`" for d in self.dirs())
            which = "supervisor.md" if self.a.pattern == "supervisor" else "supervisor-plan.md"
            text = fill((HERE / "prompts" / which).read_text(), {**base, "TASKS": tasks, "WORKSPACES": spaces, "FIRST": str(self.dirs()[0])})
            out["supervisor"] = (self.work, text)
        if self.a.pattern == "parallel":
            template = (HERE / "prompts" / "integrator.md").read_text()
            for i, (d, task) in enumerate(zip(self.dirs(), self.task["tasks"]), 1):
                out[f"integrator-{i}"] = (self.work, fill(template, {**base, "DIR": str(d), "TASK": task, "DONE": str(self.work / f"task-{i}.done")}))
        for name, (_, text) in out.items():
            (self.work / f"{name}.prompt").write_text(text)
        return out

    def launch(self, name, cwd, prompt):
        cmd = ["claude", "-p", prompt, "--setting-sources", "", "--strict-mcp-config", "--no-session-persistence",
               "--output-format", "stream-json", "--verbose", "--permission-mode", "acceptEdits", "--allowedTools", *TOOLS]
        if self.a.model:
            cmd += ["--model", self.a.model]
        out = open(self.work / f"agent-{name}.jsonl", "w")
        env = dict(self.env, GIT_AUTHOR_NAME=name, GIT_COMMITTER_NAME=name)
        proc = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
        return {"proc": proc, "started": time.time(), "ended": None}

    def run(self):
        self.setup()
        prompts = self.prompts()
        self.launched = time.time()
        deadline = self.launched + self.a.timeout
        agents = {}
        if self.a.pattern in ("supervisor", "parallel") and not self.a.dry:
            agents["supervisor"] = self.launch("supervisor", *prompts.pop("supervisor"))
            while not (self.work / "ready").exists():
                if time.time() > deadline or agents["supervisor"]["proc"].poll() is not None:
                    break
                time.sleep(1)
        for name, (cwd, prompt) in prompts.items():
            if not self.a.dry:
                agents[name] = self.launch(name, cwd, prompt)
        self.timed_out = False
        while any(a["ended"] is None for a in agents.values()):
            for a in agents.values():
                if a["ended"] is None and a["proc"].poll() is not None:
                    a["ended"] = time.time()
            if time.time() > deadline:
                self.timed_out = True
                for a in agents.values():
                    if a["ended"] is None:
                        os.killpg(a["proc"].pid, signal.SIGTERM)
                        a["proc"].wait()
                        a["ended"] = time.time()
            time.sleep(1)
        self.agents = agents
        if self.a.pattern in ("supervisor", "parallel"):
            for d in self.dirs():
                sh([self.nv, "watch", d.name, "--stop"], self.work, self.env, ok=(0, 1, 2))

    # What each agent cost, from its transcript.
    def usage(self, name):
        rows = [json.loads(line) for line in open(self.work / f"agent-{name}.jsonl") if line.startswith("{")]
        result = next((r for r in reversed(rows) if r.get("type") == "result"), {})
        u = result.get("usage", {})
        tool_calls = vcs = refused = clashes = 0
        for r in rows:
            content = (r.get("message") or {}).get("content")
            if not isinstance(content, list):
                continue
            for block in content:
                if block.get("type") == "tool_use":
                    tool_calls += 1
                    command = str(block.get("input", {}).get("command", ""))
                    if any(word in command.replace("/", " ").split() for word in ("nv", "git")):
                        vcs += 1
                elif block.get("type") == "tool_result":
                    text = json.dumps(block.get("content", ""))
                    refused += any(s in text for s in REFUSED)
                    clashes += any(s in text for s in CLASHED)
        a = self.agents[name]
        return {
            "tokens": sum(u.get(k, 0) for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens")),
            "output_tokens": u.get("output_tokens", 0),
            "cost_usd": result.get("total_cost_usd", 0),
            "turns": result.get("num_turns", 0),
            "tool_calls": tool_calls,
            "vcs_calls": vcs,
            "refused": refused,
            "clashes": clashes,
            "seconds": round(a["ended"] - a["started"], 1),
            "started_s": round(a["started"] - self.launched, 1),
            "error": bool(result.get("is_error")) or not result,
            "report": (result.get("result") or "")[-4000:],
        }

    # What came to be on the ref.
    def history(self):
        e, w = self.env, self.work
        tip = w / "verify"
        shutil.rmtree(tip, ignore_errors=True)
        if self.a.pattern == "git":
            sh(["git", "clone", "-q", str(w / "origin.git"), "verify"], w, e)
            log = sh(["git", "log", "--format=%H %s"], tip, e).stdout
        else:
            sh([self.nv, "sync", "observer"], w, e, ok=(0, 1))
            shutil.copytree(w / "observer", tip, ignore=shutil.ignore_patterns(".nv"))
            log = sh([self.nv, "git", "-C", str(w / "observer"), "log", "--format=%H %s"], w, e).stdout
        commits = []
        for line in log.splitlines():
            oid, _, subject = line.partition(" ")
            if oid == self.start:
                break
            commits.append((oid, subject))
        passing = 0
        for oid, _ in commits:
            at = w / "at"
            shutil.rmtree(at, ignore_errors=True)
            if self.a.pattern == "git":
                at.mkdir()
                sh(["sh", "-c", f"git -C {tip} archive {oid} | tar -x -C {at}"], w, e)
            else:
                sh([self.nv, "checkout", str(w / "observer" / ".nv" / "store"), oid, str(at)], w, e)
            passing += sh(["sh", "-c", self.task["test"]], at, e, ok=tuple(range(256))).returncode == 0
        shutil.rmtree(w / "at", ignore_errors=True)
        markers = sh(["sh", "-c", "grep -rl '^<<<<<<< ' --include='*.py' --include='*.md' . | wc -l"], tip, e).stdout.strip()
        # The acceptance tests, at the tip, one count of each file's tests.
        accept = tip / "accept"
        shutil.copytree(HERE / "tasks" / self.a.task / "accept", accept)
        (accept / "__init__.py").write_text("")
        counted = sh([sys.executable, "-c", COUNT], tip, e).stdout
        passed, total = map(int, counted.split())
        return {
            "commits": len(commits),
            "commits_passing": passing,
            "provisional": sum(1 for _, s in commits if s.startswith("checkpoint: ")),
            "markers": int(markers or 0),
            "accept_passed": passed,
            "accept_total": total,
            "log": [s for _, s in commits],
        }


COUNT = """
import unittest
suite = unittest.defaultTestLoader.discover("accept", top_level_dir=".")
result = unittest.TestResult()
suite.run(result)
print(result.testsRun - len(result.failures) - len(result.errors), result.testsRun)
"""


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--task", required=True)
    p.add_argument("--pattern", required=True, choices=["supervisor", "parallel", "nv", "git"])
    p.add_argument("--nv", required=True, help="the nv binary (copied: a build made meanwhile does not change the round)")
    p.add_argument("--server", default="http://localhost:8799")
    p.add_argument("--work", required=True)
    p.add_argument("--out", required=True, help="the TSV a row is appended to")
    p.add_argument("--label", default="", help="what to call this binary in the results")
    p.add_argument("--model", default="")
    p.add_argument("--dry", action="store_true", help="set up and read the ref, with no agents: to try the harness")
    p.add_argument("--timeout", type=int, default=2400, help="seconds before every agent still running is stopped")
    a = p.parse_args()
    r = Round(a)
    r.run()
    agents = {name: r.usage(name) for name in r.agents}
    history = r.history()
    makespan = max((x["ended"] for x in r.agents.values()), default=r.launched) - r.launched
    workers = [x for name, x in agents.items() if name.startswith("worker")]
    row = {
        "label": a.label or Path(a.nv).name, "pattern": a.pattern, "task": a.task, "model": a.model or "default",
        "accept_passed": history["accept_passed"], "accept_total": history["accept_total"],
        "reached": int(history["accept_passed"] == history["accept_total"] and not r.timed_out),
        "makespan_s": round(makespan, 1),
        "first_start_s": min((x["started_s"] for x in workers), default=0),
        **{k: round(sum(x[k] for x in agents.values()), 4 if k == "cost_usd" else None) for k in ("tokens", "output_tokens", "cost_usd", "turns", "tool_calls", "vcs_calls", "refused", "clashes")},
        **{k: history[k] for k in ("commits", "commits_passing", "provisional", "markers")},
        "timed_out": int(r.timed_out), "agents": len(agents),
    }
    (r.work / "result.json").write_text(json.dumps({"row": row, "agents": agents, "log": history["log"]}, indent=1))
    new = not Path(a.out).exists() or Path(a.out).stat().st_size == 0
    with open(a.out, "a") as f:
        if new:
            f.write("\t".join(FIELDS) + "\n")
        f.write("\t".join(str(row[k]) for k in FIELDS) + "\n")
    print(json.dumps(row, indent=1))


if __name__ == "__main__":
    main()
