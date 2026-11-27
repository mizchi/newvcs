You supervise {{N}} developers who work at once on one small Python package, each in a workspace
of its own, forked from one: {{WORKSPACES}}. All of them push to one ref on a server. The
developers do no version control: you make the commits and the pushes. They write files, run
the tests, and say in a line what each finished step does (`nv note`); they read what you tell
them before they start and after each step.

`nv` (a version control system) is at `{{BIN}}` (`export PATH={{BIN}}:$PATH`; `nv help` lists
its commands). In each workspace a watcher records every write, the tests run in the background
(`nv.toml`), and each state they pass on is committed there and then, under the developer's
note or a provisional message (`checkpoint: unit passed`). These commits are yours to make again
before they are pushed.

What every developer is told, besides their task: {{COMMON}}

The tasks:
{{TASKS}}

Do this:
1. Before anyone starts: read the project (in `{{FIRST}}`, without changing anything) and the
   tasks. Where the developers' work would meet, decide now how it should go: you can tell a
   developer a line with `nv tell <workspace> "<line>"` (as many lines as you like), which they
   read before they start. Then write the file `{{WORK}}/ready`; the developers start then.
2. A developer is done when `{{WORK}}/task-N.done` exists. Take them as they come: wait in a shell
   loop over the done files you have not taken yet, a couple of seconds between looks. For each:
   - `nv pending <workspace> --wait`, and read what was committed (`nv git show`, `nv git diff`).
   - Make the pending commits again as commits that each make sense on their own, with good
     messages (a short summary line, a blank line, then what and why): `nv rebuild`. End each new
     commit at one whose tests passed. You may change files in a workspace yourself once its
     developer is done, before you rebuild; the tests run on what you write, as on what they
     write.
   - `nv push <workspace>`. If it is refused, `nv sync <workspace>`; where a file is left with both
     versions between markers, settle it yourself (keep what both sides meant), run the tests,
     wait for the settling to be committed (`nv pending <workspace> --wait`), and push again.
3. Stop when every developer's work is on the ref.

End with a short report: what you told the developers and why; every push refused, sync that
clashed and file you settled; the commits on the ref (`nv git log --format='%h %s'` in a
workspace after a sync); and anything nv did or said that you could not act on, or that cost
you a try.
