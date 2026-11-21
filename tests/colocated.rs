//! A directory that is both a Git repository and an nv workspace: Git moving
//! without nv is noticed, stopped where it can be, and put right.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NV: &str = env!("CARGO_BIN_EXE_nv");

fn clean(c: &mut Command) -> &mut Command {
  c.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null").env_remove("NV_WRITING").env_remove("NV_GIT");
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
fn an_adopted_repository_is_in_step_and_git_does_not_see_nv() {
  let dir = adopted("adopt");
  let (git, nv) = heads(&dir);
  assert_eq!(git, nv);
  assert_eq!(ok(&dir, "$NV check ."), "in step");
  assert_eq!(ok(&dir, "git status --porcelain"), "", "Git must not see .nv as an untracked directory");
  assert_eq!(ok(&dir, "$NV git status --porcelain"), "");
  // Adopting twice, or adopting what is not a Git repository, is refused.
  assert!(!sh(&dir, "$NV adopt .").status.success());
}

#[test]
fn a_raw_git_commit_is_stopped_and_told_why() {
  let dir = adopted("stopped");
  let before = heads(&dir);
  for commit in
    ["git commit -q -am raw", "git commit -q --no-verify -am raw", "git commit -q --amend -m rewritten", "git checkout -q -b other"]
  {
    let out = sh(&dir, &format!("echo more >> a.txt && {commit}"));
    assert!(!out.status.success(), "`{commit}` went through");
    assert!(text(&out).contains("nv"), "`{commit}` was stopped without saying why:\n{}", text(&out));
  }
  assert_eq!(heads(&dir), before);
  assert_eq!(ok(&dir, "git rev-parse --abbrev-ref HEAD"), "main");
  // Reading with Git is free, and so is committing through nv.
  ok(&dir, "git log --oneline && git diff && git status");
  ok(&dir, "$NV git commit -q -am 'through nv'");
  let (git, nv) = heads(&dir);
  assert_eq!(git, nv, "a commit made through nv shows in Git at once");
  assert_ne!(git, before.0);
  assert_eq!(ok(&dir, "git status --porcelain"), "");
  assert_eq!(ok(&dir, "git log --format=%s -n 1"), "through nv");
  ok(&dir, "git fsck --strict");
}

#[test]
fn git_moving_behind_nv_is_noticed_refused_and_reconciled() {
  let dir = adopted("drift");
  // Someone gets past the hooks.
  ok(&dir, "echo four >> a.txt && echo new > n.txt && rm d/x.txt && git add -A && git -c core.hooksPath=/dev/null commit -q -m 'raw git'");
  let (git, nv) = heads(&dir);
  assert_ne!(git, nv);
  let check = sh(&dir, "$NV check .");
  assert_eq!(check.status.code(), Some(1));
  assert!(text(&check).contains("Git moved"), "{}", text(&check));
  // nv will not build on a head Git has left behind.
  let refused = sh(&dir, "echo x > y.txt && $NV git add -A && $NV git commit -q -m 'not knowing'");
  assert!(!refused.status.success() && text(&refused).contains("nv reconcile"), "{}", text(&refused));
  assert_eq!(ok(&dir, "$NV git rev-parse HEAD"), nv);
  // Reading still works, and says what is the matter.
  assert!(text(&sh(&dir, "$NV git status")).contains("nv reconcile"));
  ok(&dir, "rm y.txt && $NV reconcile .");
  let (git_after, nv_after) = heads(&dir);
  assert_eq!((git_after.as_str(), nv_after.as_str()), (git.as_str(), git.as_str()), "nv takes the commit Git made, with Git's id");
  assert_eq!(ok(&dir, "$NV check ."), "in step");
  assert_eq!(ok(&dir, "$NV git status --porcelain"), "");
  assert_eq!(ok(&dir, "$NV git log --format=%s"), "raw git\nbase");
  assert_eq!(ok(&dir, "$NV git ls-tree -r --name-only HEAD"), "a.txt\nn.txt");
  // A branch switched with raw Git is followed too.
  ok(&dir, "git -c core.hooksPath=/dev/null checkout -q -b topic && $NV reconcile .");
  assert_eq!(ok(&dir, "$NV git rev-parse --abbrev-ref HEAD"), "topic");
}

#[test]
fn both_sides_moving_loses_nothing() {
  let dir = adopted("both");
  ok(&dir, "echo four >> a.txt && git -c core.hooksPath=/dev/null commit -q -am 'raw git'");
  // nv will not publish over that knowingly. Here it cannot know: Git's directory is out of sight while it does.
  assert!(!sh(&dir, "echo y > y.txt && $NV commit . 'by nv'").status.success());
  ok(&dir, "rm -rf ../colocated-both-git && mv .git ../colocated-both-git && $NV commit . 'by nv' && mv ../colocated-both-git .git");
  let check = sh(&dir, "$NV check .");
  assert!(text(&check).contains("both moved"), "{}", text(&check));
  let stuck = sh(&dir, "$NV reconcile .");
  assert_eq!(stuck.status.code(), Some(1));
  assert!(text(&stuck).contains("nv git merge git/main"), "{}", text(&stuck));
  // Git's commit is in nv now, under a name; merging it is an ordinary merge.
  ok(&dir, "$NV git merge -q git/main && $NV reconcile .");
  let (git, nv) = heads(&dir);
  assert_eq!(git, nv);
  assert_eq!(ok(&dir, "git log --format=%s --first-parent"), "Merge branch 'git/main'\nby nv\nbase");
  assert_eq!(ok(&dir, "git log --format=%s HEAD^2"), "raw git\nbase");
  assert_eq!(ok(&dir, "git status --porcelain"), "");
  assert_eq!(ok(&dir, "$NV check ."), "in step");
}

#[test]
fn a_shell_function_sends_git_to_nv_inside_a_workspace_only() {
  let dir = adopted("shell");
  let outside = dir.parent().unwrap().join("colocated-shell-outside");
  let _ = std::fs::remove_dir_all(&outside);
  std::fs::create_dir_all(&outside).unwrap();
  ok(&outside, "git init -q -b main .");
  for shell in ["bash", "zsh"] {
    let run = |at: &Path, script: &str| {
      let nv_dir = Path::new(NV).parent().unwrap().display().to_string();
      let script = format!("PATH=\"{nv_dir}:$PATH\"; eval \"$(nv shell-init)\"; {script}");
      clean(Command::new(shell).arg("-c").arg(script).current_dir(at)).output().unwrap()
    };
    // Inside: what nv speaks is answered by nv, what it does not is refused, not handed to Git.
    let rebase = run(&dir.join("d"), "git rebase -i HEAD~1");
    assert_eq!(rebase.status.code(), Some(125), "{shell}: {}", text(&rebase));
    assert!(text(&rebase).contains("NV_GIT=raw"), "{shell}: the refusal should say how to reach Git: {}", text(&rebase));
    assert!(run(&dir, "git status --porcelain").status.success());
    // Asked for by name, the real Git answers.
    assert_eq!(String::from_utf8_lossy(&run(&dir, "NV_GIT=raw git rev-parse --git-dir").stdout).trim(), ".git");
    // Outside a workspace Git is Git.
    assert_eq!(String::from_utf8_lossy(&run(&outside, "git rev-parse --git-dir").stdout).trim(), ".git", "{shell}");
  }
}
