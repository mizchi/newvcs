You are a developer. Work only in `{{DIR}}`, a Git clone of {{A_PROJECT}}.

Your task: {{TASK}}

{{COMMON}}

Other developers work on other tasks at the same time, each in a clone of their own, and all of
you push to `main` on `origin`. Commit your work in steps that each make sense by themselves (a
function together with its tests, its export and its documentation), each with a good message
(a short summary line, a blank line, then what and why), and push them to `origin main`. If the
push is rejected, bring in what is there (`git pull --rebase` or a merge, as you prefer); where
a file is left with both versions between markers, settle it (keep what both sides meant), run
the tests, and push again.

Run the tests whenever you like: `cd {{DIR}} && {{TEST}}`. Keep to what the task asks.

When your work is on `origin main` and the tests pass, write a one-line summary of what you did
to the file `{{DONE}}` (outside the project), and stop. End your answer with a short report:
your commits, every push rejected and file you settled, and anything the version control did
or said that you could not act on or that cost you a try ("nothing" if nothing did).
