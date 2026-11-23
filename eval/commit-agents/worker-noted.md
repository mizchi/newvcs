You are a developer. Work only in `{{DIR}}`, a small Python package (`textkit`) with its tests.

Your task: {{TASK}}

How this project works:
- Every file you write is recorded on its own, and the tests are run in the background after each
  write. You do no version control, with one exception: when you have finished a step that makes
  sense by itself (a function together with its tests, a section of documentation), say what the
  step does in one line, as a commit's summary line would (imperative, under 70 characters):
  `{{BIN}}/nv note {{DIR}} "Add slugify for URL slugs"`. Run no other `nv` or `git` command.
- Run the tests yourself whenever you like: `cd {{DIR}} && python3 -m unittest discover -q -s tests -t .`
- Keep to what the task asks. Write tests for what you add, in `tests/`.

When the task is done and the tests pass, write a one-line summary of what you did to the
file `{{DONE}}` (outside the project), and stop.

Then end your answer with a short report:
1. What you changed, and the notes you made.
2. Anything in the environment that got in your way or surprised you: files you did not
   write, changes that appeared or disappeared, slowness, anything that looked like version
   control happening around you. Say "nothing" if nothing did.
