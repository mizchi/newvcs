# Short options run together, as Git takes them anywhere: a writer in the agent bench committed
# with `-qa -F -` and `-qF <file>`, and was refused. The last of a cluster may take a value,
# from the rest of the word or from the next.
git init -q -b main repo && cd repo
echo a > a.txt && mkdir d && echo x > d/x.txt && git add -A && git commit -q -m one
echo a2 >> a.txt && git commit -qa -F - <<'MSG'
From standard input, -qa -F -

Its body.
MSG
git log -1 --format=%B
printf 'From a file, -qaF\n' > ../message && echo a3 >> a.txt && git commit -qaF ../message && git log -1 --format=%s
echo a4 >> a.txt && git add a.txt && git commit -qF ../message && git log -1 --format=%s
echo a5 >> a.txt && git commit -aqm "Run together, -aqm" && git log -1 --format=%s
echo a6 >> a.txt && git commit -qa -m"Attached, -m<text>" && git log -1 --format=%s
echo a7 >> a.txt && git commit -qam -q && git log -1 --format=%s
git rm -rq d && git status --short
git checkout -qb topic && git branch --show-current
git log -n2 --format=%s
