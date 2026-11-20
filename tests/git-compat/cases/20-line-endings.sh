# Line endings as .gitattributes asks for them, going in and coming out.
git init -q -b main repo && cd repo
printf '* text=auto\n*.bat text eol=crlf\n*.sh text eol=lf\n*.bin -text\nwin/** eol=crlf\n' > .gitattributes
mkdir -p win/deep
printf 'echo one\r\necho two\r\n' > run.bat
printf 'echo one\r\necho two\r\n' > run.sh
printf 'a\r\nb\r\n' > data.bin
printf 'plain\r\ntext\r\n' > notes.txt
printf 'a\nb\n' > win/deep/a.txt
printf 'a\000b\r\n' > win/blob
git add -A 2>/dev/null && git commit -q -m "files with CRLF"
echo "--- what is stored"; git ls-tree -r HEAD
echo "--- clean after commit"; git status --porcelain
rm run.bat run.sh data.bin notes.txt win/deep/a.txt win/blob
git restore . 2>/dev/null
echo "--- what comes back"
for f in run.bat run.sh data.bin notes.txt win/deep/a.txt win/blob; do printf '%s: ' "$f"; od -c "$f" | head -2 | tr -s ' ' | tr '\n' ' '; echo; done
echo "--- clean after restore"; git status --porcelain
printf 'echo one\r\necho two\r\necho three\r\n' > run.bat
echo "--- an edit shows as an edit of lines, not of endings"; git status --porcelain; git diff
git commit -q -am "one more line"
git clone -q . ../copy 2>/dev/null && cd ../copy
echo "--- a clone"; od -c run.bat | head -3; od -c win/deep/a.txt | head -1; git status --porcelain
