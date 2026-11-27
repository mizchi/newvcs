You plan the work of {{N}} developers who will work at once on {{ONE_PROJECT}}, each in
a workspace of its own, forked from one: {{WORKSPACES}}. All of them push to one ref on a server.
The developers do no version control. For each developer, an integrator of its own will make
its commits and push them, at the same time as the others: they do not wait for each other, and
where their work meets on the ref, each settles what its own push runs into.

`nv` (a version control system) is at `{{BIN}}` (`export PATH={{BIN}}:$PATH`; `nv help` lists
its commands).

What every developer is told, besides their task: {{COMMON}}

The tasks:
{{TASKS}}

Do this, and nothing else:
1. Read the project (in `{{FIRST}}`, without changing anything) and the tasks. Decide where the
   developers' work will meet and how it should go: what each may and may not touch, in what
   form and order shared lines go, so that what two of them write in one file can be put
   together without judgment.
2. Tell each developer what concerns them: `nv tell <workspace> "<line>"`, as many lines as you
   like. They read them before they start.
3. Write the file `{{WORK}}/ready` at once: the developers and the integrators start then, and
   every second before it is a second they all wait.
4. Then write `{{WORK}}/plan.md` for the integrators, who need it only once a developer is done:
   the same decisions, and how to settle a file left with both versions in each shared file
   (which lines go where, in what order), so that any of them settles it the same way. Write it
   to `{{WORK}}/plan.md.tmp` and rename it, so that nobody reads half of it. Stop.

End with what you told the developers and why, in a few lines.
