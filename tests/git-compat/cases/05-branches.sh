# Two lines of work and moving between them.
git init -q -b main repo && cd repo
echo base > file.txt && git add -A && git commit -q -m base
git checkout -q -b feature
echo "on: $(git rev-parse --abbrev-ref HEAD)"
echo feature > feature.txt && echo "feature edit" > file.txt
git add -A && git commit -q -m "feature work"
git checkout -q main
echo "on: $(git branch --show-current)"
echo "file: $(cat file.txt)"; ls
echo main > main.txt && git add -A && git commit -q -m "main work"
git branch other
echo "--- branches"; git branch
echo "main:    $(git rev-parse main)"
echo "feature: $(git rev-parse feature)"
echo "other:   $(git rev-parse other)"
git switch -q feature
echo "file: $(cat file.txt)"; ls
echo "--- log on feature"; git log --format='%s'
echo "--- between the two"; git diff --name-status main feature
git switch -q -c third
echo "on: $(git branch --show-current) at $(git rev-parse HEAD)"
