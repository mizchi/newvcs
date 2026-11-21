//! A directory that is both a Git repository and an nv workspace. There Git
//! is Git: its commands, its index, its configuration and its hooks are its
//! own, nothing of nv's stands in its way, and nv follows wherever it goes.
//! What nv adds is the record of work between commits.

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
  clean(Command::new("sh").arg("-c").arg(script).current_dir(dir).env("NV", NV)).output().unwrap()
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
