You are joining a project and have only its history: `{{REPO}}`, a bare Git repository. There
is no working tree and nobody to ask. Use `git -C {{REPO}} ...` (log, show, diff, blame, grep on a
revision); read as little as answers the questions, and say for each answer which commits it
rests on.

Answer each question in a line or two. Where the history does not say, write "not recorded" and
what you would have needed; do not guess.

1. In what order did the package's functions arrive, and which commit brought each?
2. What does `textkit.reading_time` return for text with no words, and for `wpm=0`? Which commit
   decided each?
3. Does `textkit.slugify` keep letters outside ASCII (such as "è")? Where is that said or shown?
4. Which commit added the README's "Usage" section?
5. Did any commit in this history have to reconcile its writer's change with another writer's
   change to the same file? If so, which commit, which file, and what were the two sides?
6. You are to make `wrap` split a word longer than `width` instead of giving it a line of its
   own. Which files would you change, and which existing test says the current behaviour?

End with one line: `answers: <n> of 6 rest on what the history says; not recorded: <list>`.
