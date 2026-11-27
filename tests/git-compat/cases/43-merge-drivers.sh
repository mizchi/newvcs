# A file whose `.gitattributes` names a merge driver is merged by the command the configuration
# gives that driver: given the ancestor's, ours and theirs as files (%O %A %B), the marker size
# (%L) and the path (%P), it leaves the result in %A, and says by its exit whether that is clean.
# A driver the configuration does not define is the text merge.
git init -q -b main repo && cd repo
cat > ../both.sh <<'SH'
# Every line of either side, each once, in order: what a list wants.
[ -f "$1" ] && [ -f "$2" ] && [ -f "$3" ] || exit 9
echo "both.sh on $5, markers of $4" >> ../drivers.log
cat "$2" "$3" | awk '!seen[$0]++' > "$2.new" && mv "$2.new" "$2"
SH
cat > ../refuse.sh <<'SH'
echo "refuse.sh on $2" >> ../drivers.log
printf 'the driver could not merge this\n' > "$1"
exit 1
SH
git config merge.both.name "keep the lines of both"
git config merge.both.driver 'sh ../both.sh %O %A %B %L %P'
git config merge.refuse.driver 'sh ../refuse.sh %A %P'
git config merge.both.driver; echo "read back: $?"
printf 'list.txt merge=both\nhard.txt merge=refuse\nplain.txt merge=nobody\n' > .gitattributes
printf 'a\nb\n' > list.txt; printf 'one\n' > hard.txt; printf '1\n2\n3\n' > plain.txt; echo same > same.txt
git add -A && git commit -q -m base
git checkout -q -b side
printf 'a\nb\nside\n' > list.txt; printf 'one by side\n' > hard.txt; printf '1\n2\n3 side\n' > plain.txt
git commit -q -am side
git checkout -q main
printf 'a\nmain\nb\n' > list.txt; printf 'one by main\n' > hard.txt; printf '1 main\n2\n3\n' > plain.txt
git commit -q -am main
git merge side; echo "merge: $?"
echo "--- drivers"; sort ../drivers.log
echo "--- list"; cat list.txt
echo "--- hard"; cat hard.txt
echo "--- plain"; cat plain.txt
echo "--- status"; git status --porcelain
printf 'one by both\n' > hard.txt && git add hard.txt && git commit -q -m "Merge side"; echo "commit: $?"
git log --format=%s -n 1; git status --porcelain
# Only what changed on both sides goes to a driver: a side's change alone is taken as it is.
git checkout -q -b again && printf 'a\nmain\nb\nside\nagain\n' > list.txt && git commit -q -am again
git checkout -q main && git merge -q again; echo "fast-forward: $?"; cat list.txt; wc -l < ../drivers.log
