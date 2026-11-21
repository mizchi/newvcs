//! A directory that is both a Git repository and an nv workspace. There Git
//! is Git: its commands, its index, its configuration and its hooks are its
//! own, nothing of nv's stands in its way, and nv follows wherever it goes.
//! What nv adds is the record of work between commits.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NV: &str = env!("CARGO_BIN_EXE_nv");

fn clean(c: &mut Command) -> &mut Command {
  c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null").env_remove("NV_GIT");
  for (key, value) in
    [("AUTHOR_NAME", "A"), ("AUTHOR_EMAIL", "a@example.com"), ("COMMITTER_NAME", "C"), ("COMMITTER_EMAIL", "c@example.com")]
  {
    c.env(format!("GIT_{key}"), value);
  }
  c.env("GIT_AUTHOR_DATE", "1700000000 +0000").env("GIT_COMMITTER_DATE", "1700000000 +0000")
}

fn sh(dir: &Path, script: &str) -> Output {
  common::bounded(clean(Command::new("sh").arg("-c").arg(script).current_dir(dir).env("NV", NV)))
}

fn text(out: &Output) -> String {
  format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn ok(dir: &Path, script: &str) -> String {
  let out = sh(dir, script);
  assert!(out.status.success(), "`{script}` failed:\n{}", text(&out));
  String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A Git repository with one commit, adopted by nv.
fn adopted(name: &str) -> PathBuf {
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("colocated-{name}"));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  ok(
    &dir,
    "git init -q -b main . && printf 'one\\ntwo\\nthree\\n' > a.txt && mkdir d && echo x > d/x.txt && git add -A && git commit -q -m base",
  );
  ok(&dir, "$NV adopt .");
  dir
}

fn heads(dir: &Path) -> (String, String) {
  (ok(dir, "git rev-parse HEAD"), ok(dir, "$NV git rev-parse HEAD"))
}

#[test]
fn adopting_leaves_git_as_it_was_and_can_be_undone() {
  let dir = adopted("adopt");
  assert_eq!(ok(&dir, "$NV check ."), "in step");
  assert_eq!(ok(&dir, "git status --porcelain"), "", "Git must not see .nv as an untracked directory");
  // Nothing was put in Git's way: no hook is installed.
  assert_eq!(ok(&dir, "ls .git/hooks | grep -v '\\.sample$' | wc -l").trim(), "0");
  // Adopting twice, or adopting what is not a Git repository, is refused.
  assert!(!sh(&dir, "$NV adopt .").status.success());
  assert!(!sh(&dir, "mkdir plain && $NV adopt plain").status.success());
  // Undone, it is the repository it was.
  ok(&dir, "echo more >> a.txt && $NV snapshot . >/dev/null && $NV adopt . --undo");
  assert!(!dir.join(".nv").exists());
  assert_eq!(ok(&dir, "cat .git/info/exclude | grep -c '^.nv/$'; git status --porcelain; git fsck --strict 2>&1"), "0\n M a.txt");
}

#[test]
fn git_there_is_git_itself() {
  let dir = adopted("git-itself");
  ok(&dir, "git branch side && git tag v1 && echo two >> d/x.txt");
  // Whatever Git speaks, said through nv, is Git's answer: nothing is refused or approximated.
  for command in [
    "status",
    "status -sb",
    "branch -v",
    "tag -l",
    "log --graph --oneline --all",
    "diff HEAD --stat",
    "-c core.quotepath=false status --porcelain=v2",
    "--version",
    "ls-files -s",
    "rev-parse --verify HEAD^{tree}",
    "show HEAD:a.txt",
    "stash list",
  ] {
    let (git, nv) = (sh(&dir, &format!("git {command}")), sh(&dir, &format!("$NV git {command}")));
    assert_eq!((text(&nv), nv.status.code()), (text(&git), git.status.code()), "git {command}");
  }
  // From a subdirectory, and writing: one index, Git's.
  ok(&dir, "cd d && $NV git add x.txt && $NV git commit -q -m 'through nv' && $NV git stash list");
  assert_eq!(ok(&dir, "git log --format=%s -1; git status --porcelain"), "through nv");
  assert_eq!(ok(&dir, "git log -1 --format='%an <%ae>'"), "A <a@example.com>");
}

#[test]
fn raw_git_goes_where_it_likes_and_nv_follows() {
  let dir = adopted("follows");
  ok(&dir, "echo four >> a.txt && $NV mark . before >/dev/null");
  // A commit, a branch, a reset, a rebase: each just works, the way it does without nv.
  ok(&dir, "git commit -q -am 'by git' && git checkout -q -b side && echo s > s.txt && git add s.txt && git commit -q -m side");
  ok(&dir, "git checkout -q main && echo m > m.txt && git add m.txt && git commit -q -m main2 && git rebase -q main side");
  assert_eq!(ok(&dir, "git log --format=%s"), "side\nmain2\nby git\nbase");
  // Nothing nv believed is in the way, and it says where Git is without being asked to catch up.
  assert_eq!(ok(&dir, "$NV since . before"), "A m.txt\nA s.txt");
  assert_eq!(ok(&dir, "$NV check ."), "in step");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "");
  // A reset that throws work away is Git's to do; what nv recorded before it is still there to go back to.
  ok(&dir, "echo precious >> a.txt && $NV snapshot . >/dev/null && git reset -q --hard HEAD~2");
  assert_eq!(ok(&dir, "cat a.txt | tail -1; ls"), "four\na.txt\nd");
  ok(&dir, "$NV restore . '~0' >/dev/null");
  assert_eq!(ok(&dir, "tail -1 a.txt; ls"), "precious\na.txt\nd\nm.txt\ns.txt");
  // No branch at all is somewhere Git can be, too.
  ok(&dir, "git stash -q -u && git checkout -q --detach && echo detached > det.txt && $NV commit . 'on no branch' >/dev/null");
  assert_eq!(
    ok(&dir, "git log -1 --format=%s; git status --porcelain; git symbolic-ref -q HEAD || echo detached"),
    "on no branch\ndetached"
  );
  ok(&dir, "git fsck --strict");
}

#[test]
fn work_recorded_between_commits_is_published_as_one_commit_by_git() {
  let dir = adopted("publish");
  ok(&dir, "git config user.name Real && git config user.email real@example.com");
  let before = ok(&dir, "git rev-parse HEAD");
  // An attempt, a good state, a bad attempt undone, more work: five recorded states, none of them Git's business.
  ok(&dir, "echo one >> a.txt && $NV snapshot . >/dev/null && echo new > new.txt && $NV mark . green >/dev/null");
  ok(&dir, "echo broken > a.txt && rm d/x.txt && $NV snapshot . >/dev/null && $NV restore . green >/dev/null && echo done >> new.txt");
  assert_eq!(ok(&dir, "git rev-parse HEAD"), before);
  assert_eq!(ok(&dir, "git status --porcelain | tr ' ' _"), "_M_a.txt\n??_new.txt");
  let said = ok(&dir, "$NV commit . 'the work'");
  // One commit, made by Git with Git's identity, holding exactly the difference.
  assert_eq!(ok(&dir, "git rev-list --count HEAD; git log -1 --format='%s|%an <%ae>|%cn'"), "2\nthe work|A <a@example.com>|C");
  assert_eq!(ok(&dir, "git show --format= --name-status HEAD"), "M\ta.txt\nA\tnew.txt");
  assert_eq!(ok(&dir, "git status --porcelain; $NV status . 2>/dev/null"), "");
  assert!(said.contains(&ok(&dir, "git rev-parse HEAD")), "{said}");
  let (git, nv) = heads(&dir);
  assert_eq!(git, nv);
  assert_eq!(ok(&dir, "$NV commit . 'nothing more'"), "nothing to commit");
  ok(&dir, "git fsck --strict");
  // Reported writes are published without a walk, and only they.
  ok(&dir, "echo r >> a.txt && echo unreported > stray.txt && $NV record . a.txt && $NV commit . 'reported' --recorded >/dev/null");
  assert_eq!(ok(&dir, "git show --format= --name-status HEAD; git status --porcelain"), "M\ta.txt\n?? stray.txt");
  // Without the identity from the environment, Git's configured one.
  ok(
    &dir,
    "echo c >> a.txt && env -u GIT_AUTHOR_NAME -u GIT_AUTHOR_EMAIL -u GIT_COMMITTER_NAME -u GIT_COMMITTER_EMAIL $NV commit . 'configured' >/dev/null",
  );
  assert_eq!(ok(&dir, "git log -1 --format='%an <%ae>'"), "Real <real@example.com>");
}

#[test]
fn what_git_would_not_commit_nv_does_not_either() {
  let dir = adopted("rules");
  // Excluded by a file only this clone has, and by one the user has for every repository.
  ok(
    &dir,
    "echo '*.secret' >> .git/info/exclude && echo '*.bak' > ../colocated-rules-global-ignore && git config core.excludesFile \"$PWD/../colocated-rules-global-ignore\"",
  );
  ok(&dir, "echo s > x.secret && echo b > x.bak && echo fine > fine.txt");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null"), "A fine.txt");
  // Stored by Git through a filter, and with its line endings made Git's.
  ok(&dir, "git config filter.up.clean 'tr a-z A-Z' && git config filter.up.smudge cat && git config core.autocrlf true");
  ok(&dir, "echo '*.up filter=up' > .gitattributes && echo hello > f.up && printf 'a\\r\\nb\\r\\n' > crlf.txt");
  ok(&dir, "$NV commit . 'by the rules' >/dev/null");
  assert_eq!(ok(&dir, "git ls-tree --name-only HEAD"), ".gitattributes\na.txt\ncrlf.txt\nd\nf.up\nfine.txt");
  assert_eq!(ok(&dir, "git cat-file -p HEAD:f.up; git cat-file -p HEAD:crlf.txt | od -c | head -1"), "HELLO\n0000000    a  \\n   b  \\n");
  assert_eq!(ok(&dir, "git status --porcelain"), "");
  // A hook of the user's own runs, and its no is a no: nothing is committed and nothing recorded is lost.
  ok(
    &dir,
    "printf '#!/bin/sh\\necho not today >&2\\nexit 1\\n' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit && echo again >> fine.txt",
  );
  let refused = sh(&dir, "$NV commit . 'refused'");
  assert!(!refused.status.success() && text(&refused).contains("not today"), "{}", text(&refused));
  assert_eq!(ok(&dir, "git log -1 --format=%s; git status --porcelain; $NV since . base"), "by the rules\n M fine.txt\nM fine.txt");
  let _ = std::fs::remove_file(dir.join("../colocated-rules-global-ignore"));
}

#[test]
fn a_shared_directory_can_be_moved_and_killed_commands_leave_git_whole() {
  let dir = adopted("moved-from");
  let to = dir.with_file_name("colocated-moved-to");
  let _ = std::fs::remove_dir_all(&to);
  std::fs::rename(&dir, &to).unwrap();
  ok(&to, "echo m >> a.txt && $NV commit . 'after the move' >/dev/null && $NV check .");
  assert!(!dir.exists());
  // Killed a hundred times in the middle of publishing: Git's history and index are always Git's own, and whole.
  ok(
    &to,
    "i=0; while [ $i -lt 100 ]; do i=$((i+1)); echo \"edit $i\" >> a.txt; echo $i > k$i.txt; $NV commit . \"c $i\" >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,rand(0.03))'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; rm -f .git/index.lock; done; true",
  );
  ok(&to, "git fsck --strict && $NV commit . 'the rest' >/dev/null; git status --porcelain");
  assert_eq!(ok(&to, "git status --porcelain; $NV status . 2>/dev/null; $NV check ."), "in step");
  assert_eq!(ok(&to, "git show HEAD:a.txt | tail -1; git ls-files | grep -c '^k'"), "edit 100\n100");
}

#[test]
fn a_shell_function_sends_git_to_nv_inside_a_workspace_only() {
  let dir = adopted("shell");
  let run = |script: &str| ok(&dir, &format!("export PATH=\"$(dirname $NV):$PATH\"; eval \"$($NV shell-init)\"; {script}"));
  // In a directory shared with Git it reaches Git; in a workspace with no Git, nv's own Git commands.
  assert_eq!(run("echo z >> a.txt && git commit -q -am typed && git log -1 --format=%s && git stash list"), "typed");
  ok(&dir, "mkdir pure && cd pure && $NV git init -q -b main . && echo p > p.txt && $NV git add p.txt && $NV git commit -q -m pure");
  assert_eq!(run("cd pure && git log --format=%s && git rev-parse --abbrev-ref HEAD"), "pure\nmain");
  let refused = sh(&dir, "export PATH=\"$(dirname $NV):$PATH\"; eval \"$($NV shell-init)\"; cd pure && git stash");
  assert_eq!(refused.status.code(), Some(125), "{}", text(&refused));
  assert!(text(&refused).contains("NV_GIT=raw"));
}

#[test]
fn publishing_keeps_out_of_what_git_is_in_the_middle_of() {
  let dir = adopted("mid-merge");
  // Two sides change the same line, and one adds files: the merge stops for a person.
  ok(
    &dir,
    "git checkout -q -b side && printf 'one\\nSIDE\\nthree\\n' > a.txt && echo s > s.txt && echo s2 > s2.txt && git add -A && git commit -q -m side",
  );
  ok(&dir, "git checkout -q main && printf 'one\\nMAIN\\nthree\\n' > a.txt && git commit -q -am main");
  assert!(!sh(&dir, "git merge side").status.success());
  let before = ok(&dir, "git status --porcelain | tr ' ' _");
  assert_eq!(before, "UU_a.txt\nA__s.txt\nA__s2.txt");
  // nv says whose turn it is and touches nothing of Git's: the merge is as it was, and concludes whole.
  let refused = sh(&dir, "$NV commit . oops");
  assert!(!refused.status.success() && text(&refused).contains("in the middle of a merge"), "{}", text(&refused));
  assert_eq!(ok(&dir, "git status --porcelain | tr ' ' _"), before);
  ok(&dir, "$NV mark . mid >/dev/null && $NV since . base >/dev/null");
  assert_eq!(ok(&dir, "git status --porcelain | tr ' ' _"), before);
  ok(&dir, "printf 'one\\nBOTH\\nthree\\n' > a.txt && git add a.txt && git commit -q -m merged");
  assert_eq!(
    ok(&dir, "git ls-tree --name-only HEAD | tr '\\n' ' '; git rev-list --parents -1 HEAD | wc -w"),
    "a.txt d s.txt s2.txt        3"
  );
  // The same during a rebase that stopped: no commit with both versions in a file.
  ok(&dir, "git checkout -q -b r1 HEAD~1 && printf 'one\\nR1\\nthree\\n' > a.txt && git commit -q -am r1");
  assert!(!sh(&dir, "git rebase main").status.success());
  let refused = sh(&dir, "$NV commit . oops");
  assert!(!refused.status.success() && text(&refused).contains("in the middle of a rebase"), "{}", text(&refused));
  ok(&dir, "git rebase --abort && $NV commit . 'nothing to say' && git fsck --strict");
}

#[test]
fn publishing_leaves_what_the_user_staged_as_the_user_staged_it() {
  let dir = adopted("staging");
  ok(&dir, "echo b > b.txt && echo c > c.txt && git add -A && git commit -q -m more");
  // A removal and a rename the user made with Git are published like any other change.
  ok(&dir, "git rm -q b.txt && git mv c.txt c2.txt && echo more >> a.txt && $NV commit . 'removed and renamed' >/dev/null");
  assert_eq!(ok(&dir, "git show --format= --name-status -M HEAD; git status --porcelain"), "M\ta.txt\nD\tb.txt\nR100\tc.txt\tc2.txt");
  // A hook of the user's that says no: what the user had staged, and only half staged, is as it was.
  ok(&dir, "printf '#!/bin/sh\\nexit 1\\n' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit");
  ok(&dir, "echo us > us.txt && git add us.txt && echo half >> a.txt && git add a.txt && echo other-half >> a.txt && echo x >> d/x.txt");
  let staged = ok(&dir, "git status --porcelain | tr ' ' _");
  assert_eq!(staged, "MM_a.txt\n_M_d/x.txt\nA__us.txt");
  assert!(!sh(&dir, "$NV commit . refused").status.success());
  assert_eq!(ok(&dir, "git status --porcelain | tr ' ' _"), staged);
  assert_eq!(ok(&dir, "git diff --cached a.txt | grep -c '^+half'"), "1");
  // A hook that changes what is committed and adds it again: the index ends up as the commit, as after `git commit -a`.
  ok(
    &dir,
    "printf '#!/bin/sh\\nfor f in $(git diff --cached --name-only); do echo formatted >> \"$f\"; git add \"$f\"; done\\n' > .git/hooks/pre-commit",
  );
  ok(&dir, "git reset -q && $NV commit . 'through a formatter' >/dev/null");
  assert_eq!(ok(&dir, "git status --porcelain; git show HEAD:us.txt | tail -1"), "formatted");
  // A file Git is told to leave out of the tree stops the commit, with the index as it was.
  ok(&dir, "rm .git/hooks/pre-commit && git update-index --skip-worktree d/x.txt && echo skipped >> d/x.txt && echo plain >> a.txt");
  let out = sh(&dir, "$NV commit . 'with a skipped file'");
  assert_eq!(ok(&dir, "git diff --cached --name-only | wc -l").trim(), "0", "{}", text(&out));
}

#[test]
fn an_adoption_cut_short_is_finished_by_adopting_again() {
  let dir = adopted("cut-short");
  // As a kill during the import leaves it: a workspace that was never joined to Git.
  ok(&dir, "rm .nv/git-synced");
  assert_eq!(ok(&dir, "git status --porcelain"), "", "Git never saw nv's directory, even then");
  assert!(ok(&dir, "$NV adopt .").starts_with("adopted"));
  assert_eq!(ok(&dir, "$NV check ."), "in step");
  assert!(!sh(&dir, "$NV adopt .").status.success(), "and once it is done, it is done");
}

#[test]
fn what_git_keeps_out_of_the_tree_on_purpose_is_not_a_file_removed() {
  let dir = adopted("sparse");
  ok(&dir, "mkdir big && echo b > big/b.txt && git add -A && git commit -q -m big && $NV since . base");
  ok(&dir, "git sparse-checkout set --no-cone '/*' '!/big/' && test ! -e big/b.txt");
  assert_eq!(ok(&dir, "$NV status . 2>/dev/null; $NV snapshot . >/dev/null; $NV since . base"), "");
  ok(&dir, "echo more >> a.txt && $NV commit . 'beside what is kept out' >/dev/null");
  assert_eq!(ok(&dir, "git ls-tree -r --name-only HEAD | tr '\\n' ' '; $NV since . base; $NV check ."), "a.txt big/b.txt d/x.txt in step");
  // Seen from a directory below the top, the two are as much in step.
  assert_eq!(ok(&dir, "cd d && $NV check ."), "in step");
}

#[test]
fn a_file_staged_in_part_and_a_bisect_are_the_users_to_finish() {
  let dir = adopted("in-part");
  // Half of a file's edits staged, half not: neither half is nv's to decide about.
  ok(&dir, "printf 'ONE\\ntwo\\nthree\\n' > a.txt && git add a.txt && printf 'ONE\\ntwo\\nTHREE\\n' > a.txt");
  let staged = ok(&dir, "git status --porcelain | tr ' ' _; git diff --cached | grep -c '^+ONE'; git diff | grep -c '^+THREE'");
  let refused = sh(&dir, "$NV commit . half");
  assert!(!refused.status.success() && text(&refused).contains("staged in part"), "{}", text(&refused));
  assert_eq!(ok(&dir, "git status --porcelain | tr ' ' _; git diff --cached | grep -c '^+ONE'; git diff | grep -c '^+THREE'"), staged);
  assert_eq!(ok(&dir, "git log --format=%s"), "base");
  // Staged whole, it is published.
  ok(&dir, "git add a.txt && $NV commit . whole >/dev/null && git status --porcelain");
  // In the middle of a bisect a commit would be on no branch.
  ok(&dir, "echo n >> a.txt && git commit -q -am next && git bisect start -q HEAD HEAD~2 >/dev/null 2>&1; echo edit >> d/x.txt");
  let refused = sh(&dir, "$NV commit . oops");
  assert!(!refused.status.success() && text(&refused).contains("in the middle of a bisect"), "{}", text(&refused));
  ok(&dir, "git checkout -q d/x.txt; git bisect reset -q >/dev/null 2>&1; git fsck --strict");
}

#[test]
fn what_a_fifth_reviewer_found_beside_git_stays_fixed() {
  let dir = adopted("fifth");
  ok(&dir, "echo k > keep.txt && echo k2 > keep2.txt && git add -A && git commit -q -m more");
  // A file removed, and nothing else: published as a removal. A file moved: both halves of the move.
  // (Named from the directory above, as a tool that does not change directory names it.)
  ok(&dir, "rm keep.txt && cd .. && $NV commit colocated-fifth removed >/dev/null");
  assert_eq!(ok(&dir, "git show --format=%s --name-status HEAD; git status --porcelain"), "removed\n\nD\tkeep.txt");
  ok(&dir, "mv keep2.txt kept.txt && $NV commit . moved >/dev/null");
  assert_eq!(ok(&dir, "git show --format=%s --name-status -M HEAD; git status --porcelain"), "moved\n\nR100\tkeep2.txt\tkept.txt");
  // Back to a mark over a commit of one's own, and that published: the file the bad commit added goes.
  ok(
    &dir,
    "$NV mark . green >/dev/null && echo bad > bad.txt && $NV commit . 'bad attempt' >/dev/null && $NV restore . green >/dev/null && $NV commit . 'back to green' >/dev/null",
  );
  assert_eq!(ok(&dir, "git status --porcelain; git show --format=%s --name-status HEAD"), "back to green\n\nD\tbad.txt");
  // The repository is the one in the directory, whatever the environment says of another.
  ok(&dir, "git init -q -b main ../colocated-fifth-other && git -C ../colocated-fifth-other commit -q --allow-empty -m z");
  ok(
    &dir,
    "echo e2 >> a.txt && GIT_DIR=$PWD/../colocated-fifth-other/.git GIT_INDEX_FILE=$PWD/../colocated-fifth-other/.git/elsewhere $NV commit . 'with a foreign GIT_DIR' >/dev/null",
  );
  assert_eq!(
    ok(&dir, "git log -1 --format=%s; git status --porcelain; git -C ../colocated-fifth-other log --format=%s"),
    "with a foreign GIT_DIR\nz"
  );
  let _ = std::fs::remove_dir_all(dir.join("../colocated-fifth-other"));
  // A mark from before Git brought commits in: going back to it would undo them, and is refused.
  ok(
    &dir,
    "$NV mark . before-pull >/dev/null && git checkout -q -b theirs && echo t > theirs.txt && echo more >> a.txt && git add -A && git commit -q -m theirs && git checkout -q main && git merge -q --ff-only theirs",
  );
  let refused = sh(&dir, "$NV restore . before-pull");
  assert!(!refused.status.success() && text(&refused).contains("Git moved"), "{}", text(&refused));
  assert_eq!(ok(&dir, "git status --porcelain; cat theirs.txt"), "t");
}

#[test]
fn taking_nv_out_leaves_the_exclude_file_as_it_was_found() {
  for (name, before) in [("no-newline", Some("foo")), ("absent", None), ("had-line", Some("x\n.nv/\ny\n")), ("plain", Some("# c\nfoo\n"))] {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("colocated-exclude-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ok(&dir, "git init -q -b main . && echo a > a.txt && git add -A && git commit -q -m base");
    let exclude = dir.join(".git/info/exclude");
    match before {
      Some(text) => std::fs::write(&exclude, text).unwrap(),
      None => std::fs::remove_dir_all(dir.join(".git/info")).unwrap(),
    }
    ok(&dir, "$NV adopt . >/dev/null && git status --porcelain && $NV adopt . --undo >/dev/null");
    assert_eq!(std::fs::read_to_string(&exclude).ok().as_deref(), before, "{name}");
    assert_eq!(dir.join(".git/info").exists(), before.is_some(), "{name}");
  }
}

#[test]
fn a_reported_write_is_published_however_often_publishing_is_killed() {
  let dir = adopted("reported-kills");
  // Each write is reported, then the publishing of it killed: Git's own commit may finish behind nv's back.
  ok(
    &dir,
    "i=0; while [ $i -lt 150 ]; do i=$((i+1)); echo \"e$i\" >> a.txt; echo $i > new-$i.txt; $NV record . a.txt new-$i.txt; $NV commit . \"c$i\" --recorded >/dev/null 2>&1 & pid=$!; perl -e 'select(undef,undef,undef,rand(0.07))'; kill -9 $pid 2>/dev/null; wait $pid 2>/dev/null; done; sleep 1",
  );
  // Without looking at any file nobody reported, the next publishing has all of them.
  ok(&dir, "$NV commit . rest --recorded >/dev/null 2>&1; git fsck --strict >/dev/null");
  assert_eq!(ok(&dir, "git status --porcelain; git ls-files | grep -c '^new-'"), "150");
}

#[test]
fn a_publishing_that_is_killed_leaves_gits_index_with_gits_commit() {
  let dir = adopted("killed-late");
  // A slow hook: the command is killed while Git's commit is under way, as a tool's time limit would.
  ok(
    &dir,
    "printf '#!/bin/sh\\nsleep 0.4\\necho hooked >> hooked.txt; git add hooked.txt\\n' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit",
  );
  ok(
    &dir,
    "echo x >> a.txt; echo n > n.txt; $NV commit . m >/dev/null 2>&1 & pid=$!; sleep 0.2; kill -9 $pid; wait $pid 2>/dev/null; sleep 1.2; true",
  );
  // The commit was made all the same, and Git's index knows of it: nothing is staged that would undo it.
  assert_eq!(ok(&dir, "git log -1 --format=%s; git status --porcelain"), "m");
  // And nothing is taken for staged in part afterwards.
  ok(
    &dir,
    "rm .git/hooks/pre-commit && echo edit >> hooked.txt && echo y >> a.txt && $NV commit . two >/dev/null && git status --porcelain",
  );
  // A hook that runs nv on the workspace being published is told so; nothing waits for ever.
  ok(&dir, "printf '#!/bin/sh\\n\"$NV\" snapshot . >/dev/null 2>&1\\nexit 0\\n' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit");
  assert_eq!(ok(&dir, "echo z >> a.txt && $NV commit . three >/dev/null && git log -1 --format=%s && git status --porcelain"), "three");
  // A file the user took out of Git and wrote a rule for is not put back in by nv.
  ok(&dir, "rm .git/hooks/pre-commit && echo KEY=1 > .env && git add .env && git commit -q -m oops && $NV status . >/dev/null 2>&1");
  ok(&dir, "git rm -q --cached .env && echo .env >> .gitignore && echo KEY=secret2 >> .env && $NV commit . m4 >/dev/null");
  // (Its removal from Git stays staged, the user's to commit.)
  assert_eq!(ok(&dir, "git show --format= --name-status HEAD; git status --porcelain | tr ' ' _"), "A\t.gitignore\nD__.env");
}

#[test]
fn what_a_seventh_reviewer_found_beside_git_stays_fixed() {
  let dir = adopted("seventh");
  // A publishing that came to nothing, then the user moving Git themselves: their staging is theirs.
  ok(
    &dir,
    "echo new > g.txt && $NV commit . 'nv publishes g.txt' >/dev/null && echo a2 >> a.txt && $NV snapshot . >/dev/null && git reset -q --hard",
  );
  ok(&dir, "$NV commit . x --recorded >/dev/null 2>&1; git reset -q --soft HEAD~1 && $NV commit . y --recorded >/dev/null 2>&1; true");
  assert_eq!(ok(&dir, "git status --porcelain | tr ' ' _"), "A__g.txt");
  ok(&dir, "git commit -q -m 'the user commits it'");
  // A new file nv recorded is still published after a commit Git made on its own.
  ok(&dir, "echo new > n1.txt && echo a3 >> a.txt && $NV mark . m1 >/dev/null && echo x2 >> d/x.txt && git commit -q -am 'by git'");
  ok(&dir, "echo a4 >> a.txt && $NV commit . pub --tracked >/dev/null");
  assert_eq!(ok(&dir, "git show --format= --name-status HEAD | sort; git status --porcelain"), "A\tn1.txt\nM\ta.txt");
}

#[test]
fn what_an_eighth_reviewer_found_beside_git_stays_fixed() {
  let dir = adopted("eighth");
  // A publishing a hook refused, then a commit of the user's own, then staging of theirs: recording leaves it alone.
  ok(&dir, "printf '#!/bin/sh\\nexit 1\\n' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit && echo a2 >> a.txt");
  assert!(!sh(&dir, "$NV commit . 'nv tries'").status.success());
  ok(&dir, "rm .git/hooks/pre-commit && git commit -q -am 'by hand' && echo x2 >> d/x.txt && echo c > c.txt && git add d/x.txt c.txt");
  let staged = ok(&dir, "git status --porcelain | tr ' ' _; git reflog | wc -l");
  ok(&dir, "$NV snapshot . >/dev/null && $NV mark . m >/dev/null && $NV since . m >/dev/null && $NV snapshot . --tracked >/dev/null; true");
  assert_eq!(ok(&dir, "git status --porcelain | tr ' ' _; git reflog | wc -l"), staged);
  ok(&dir, "git commit -q -m 'the user commits'");
  // A file recorded and then removed without a word: publishing by report does not stop at it.
  ok(
    &dir,
    "echo n > new.txt && $NV snapshot . >/dev/null && rm new.txt && echo more >> a.txt && $NV record . a.txt && $NV commit . m --recorded >/dev/null",
  );
  assert_eq!(ok(&dir, "git show --format= --name-status HEAD; git status --porcelain"), "M\ta.txt");
}

#[test]
fn a_hook_that_adds_to_the_message_does_not_leave_gits_index_behind() {
  let dir = adopted("trailer");
  ok(&dir, "echo b > b.txt && echo c > c.txt && git add -A && git commit -q -m more");
  // A commit-msg hook that signs off, and something the user has staged of their own.
  ok(
    &dir,
    "printf '#!/bin/sh\\nprintf \"\\\\nSigned-off-by: t\\\\n\" >> \"$1\"\\n' > .git/hooks/commit-msg && chmod +x .git/hooks/commit-msg",
  );
  ok(&dir, "echo a2 >> a.txt && git add a.txt && echo b2 >> b.txt && echo c2 >> c.txt && $NV commit . publish >/dev/null");
  // nv's commit has all three, with the hook's line in its message; Git's index knows it.
  assert_eq!(
    ok(&dir, "git log -1 --format=%B | head -1; git show --format= --name-only HEAD | sort; git status --porcelain"),
    "publish\na.txt\nb.txt\nc.txt"
  );
  // So the user's next commit does not take b and c back.
  ok(&dir, "echo x2 >> d/x.txt && git commit -q -am \"the user's own\"");
  assert_eq!(ok(&dir, "git show --format= --name-only HEAD; git show HEAD:b.txt | tail -1"), "d/x.txt\nb2");
  assert_eq!(ok(&dir, "ls .nv | grep -c git-index-publishing || true"), "0");
}

#[test]
fn a_hook_that_never_returns_is_stopped_and_gits_index_is_as_it_was() {
  let dir = adopted("hook-hangs");
  ok(
    &dir,
    "printf '#!/bin/sh\\necho $$ > .git/hook.pid\\nexec sleep 60\\n' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit && echo a2 >> a.txt",
  );
  let before = ok(&dir, "git status --porcelain; git rev-parse HEAD; shasum .git/index");
  let started = std::time::Instant::now();
  let out = sh(&dir, "NV_GIT_TIMEOUT=2 $NV commit . 'waits on a hook'");
  std::thread::sleep(std::time::Duration::from_millis(300));
  assert!(started.elapsed().as_secs() < 20, "it ran for {:?}", started.elapsed());
  assert!(!out.status.success() && text(&out).contains("did not finish"), "{}", text(&out));
  assert_eq!(ok(&dir, "git status --porcelain; git rev-parse HEAD; shasum .git/index"), before);
  // And the hook's sleep is gone with it.
  assert_eq!(ok(&dir, "kill -0 $(cat .git/hook.pid) 2>/dev/null && echo alive || echo gone"), "gone");
  // Without the hook, the work is published as ever.
  ok(&dir, "rm .git/hooks/pre-commit && $NV commit . 'no hook' >/dev/null && git status --porcelain");
}
